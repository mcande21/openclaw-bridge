//! Ed25519 authentication module for OpenClaw WebSocket connections.
//!
//! Handles device identity loading, signature payload construction, and
//! Ed25519 signing required for the connect handshake.
//!
//! # Device Identity
//!
//! The device keypair is generated locally on first use and cached at
//! `~/.config/openclaw-bridge/openclaw-device.json`. No SSH fetch is required.
//! Key rotation requires deleting the local file and restarting the process.
//!
//! # Signature Format (v3)
//!
//! ```text
//! v3|<deviceId>|<clientId>|<clientMode>|<role>|<scopes-csv>|<signedAtMs>|<token>|<nonce>|<platform>|<deviceFamily>
//! ```

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::BridgeError;

/// Boxed error type used throughout this module.
type AuthError = Box<dyn std::error::Error + Send + Sync + 'static>;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Nested JSON shape written on generation and produced by the VPS.
///
/// ```json
/// {
///   "version": 1,
///   "deviceId": "hex-string",
///   "keys": {
///     "operator": {
///       "privateKeyPem": "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n"
///     }
///   }
/// }
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentityNestedJson {
    version: u32,
    device_id: String,
    keys: DeviceKeysJson,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeviceKeysJson {
    operator: DeviceKeyEntryJson,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceKeyEntryJson {
    private_key_pem: String,
}

/// Legacy flat JSON shape that may exist in older cached files.
///
/// ```json
/// {
///   "deviceId": "hex-string",
///   "privateKeyPem": "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n",
///   "publicKeyPem": ""
/// }
/// ```
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentityFlatJson {
    device_id: String,
    private_key_pem: String,
    // acknowledged but derived from signing key for consistency
    #[allow(dead_code)]
    public_key_pem: Option<String>,
}

/// Device identity loaded from JSON, ready for signing.
pub struct DeviceIdentity {
    /// Hex-encoded device ID.
    pub device_id: String,
    /// Ed25519 signing key parsed from PKCS#8 PEM.
    pub signing_key: SigningKey,
    /// Raw 32-byte Ed25519 public key.
    pub public_key_bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Identity cache
// ---------------------------------------------------------------------------

/// Process-level cache for the parsed device identity.
///
/// Loaded once on first call to [`load_device_identity`]. Key rotation
/// requires deleting the local identity file and restarting the process.
/// Avoids re-reading the file, re-parsing JSON, and re-deriving the
/// public key on every WebSocket connect.
static DEVICE_IDENTITY: OnceLock<DeviceIdentity> = OnceLock::new();

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load or generate the device identity.
///
/// Returns a `&'static` reference — the identity is loaded or generated once
/// per process and reused for all subsequent connections.
///
/// Resolution order:
/// 1. In-process `OnceLock` cache (populated on first call)
/// 2. Local file `~/.config/openclaw-bridge/openclaw-device.json`
///    - If the file is corrupted, logs a warning, removes it, and falls
///      through to generation.
/// 3. Generate a new Ed25519 key pair and write the file (mode 0600, atomic).
pub fn load_device_identity() -> Result<&'static DeviceIdentity, AuthError> {
    if let Some(id) = DEVICE_IDENTITY.get() {
        return Ok(id);
    }
    let identity = load_device_identity_inner()?;
    // A concurrent call may have won the race to set — either way, use the winner.
    let _ = DEVICE_IDENTITY.set(identity);
    Ok(DEVICE_IDENTITY.get().expect("identity was just set"))
}

/// Inner implementation — called at most once per process by [`load_device_identity`].
fn load_device_identity_inner() -> Result<DeviceIdentity, AuthError> {
    let cache_path = device_cache_path()?;

    match fs::read_to_string(&cache_path) {
        Ok(raw_json) => {
            match parse_device_identity(&raw_json) {
                Ok(identity) => return Ok(identity),
                Err(e) => {
                    // Corrupted identity file — log warning, remove it, fall
                    // through to generation so the process can continue.
                    crate::verbose!(
                        "[auth] warning: failed to parse device identity at {}: {e} — \
                         removing corrupted file and generating a new identity",
                        cache_path.display()
                    );
                    let _ = fs::remove_file(&cache_path);
                    // fall through to generation
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No local file — generate a new identity below.
        }
        Err(e) => return Err(e.into()),
    }

    generate_and_persist_identity(&cache_path)
}

/// Generate a new Ed25519 identity, persist it atomically with 0600 perms.
fn generate_and_persist_identity(path: &PathBuf) -> Result<DeviceIdentity, AuthError> {
    // Generate new Ed25519 signing key.
    let signing_key = SigningKey::generate(&mut OsRng);

    // Device ID: SHA-256 of the Ed25519 public key bytes, encoded as 64 lowercase hex chars.
    // The gateway cryptographically verifies device.id == SHA-256(device.publicKey).
    let verifying_key = signing_key.verifying_key();
    let public_key_bytes = verifying_key.as_bytes().to_vec();
    let hash = Sha256::digest(&public_key_bytes);
    let device_id = hex::encode(hash);

    // Encode key as PKCS#8 PEM.
    let pem = signing_key
        .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
        .map_err(|e| format!("failed to encode signing key as PKCS#8 PEM: {e}"))?;

    // Serialize to canonical nested JSON format.
    let json_value = DeviceIdentityNestedJson {
        version: 1,
        device_id: device_id.clone(),
        keys: DeviceKeysJson {
            operator: DeviceKeyEntryJson {
                private_key_pem: pem.to_string(),
            },
        },
    };
    let raw_json = serde_json::to_string_pretty(&json_value)
        .map_err(|e| format!("failed to serialize device identity: {e}"))?;

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config dir {}: {e}", parent.display()))?;
    }

    // Atomic write: temp file → set 0600 → rename to final path.
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, &raw_json)
        .map_err(|e| format!("failed to write temp identity file: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("failed to set identity file permissions: {e}"))?;
    }

    fs::rename(&tmp_path, path)
        .map_err(|e| format!("failed to rename identity file: {e}"))?;

    crate::verbose!(
        "[auth] generated new device identity: device_id={device_id} path={}",
        path.display()
    );

    Ok(DeviceIdentity {
        device_id,
        signing_key,
        public_key_bytes,
    })
}

/// Load the device auth token from the local cache only.
///
/// Returns `None` if no local token file exists (device not yet paired;
/// the gateway will issue a token during the pairing flow).
pub fn load_device_token() -> Result<Option<String>, AuthError> {
    let cache_dir = crate::config_dir().map_err(|e| -> AuthError { e.into() })?;
    let cache_path = cache_dir.join("openclaw-bridge").join("openclaw-device-auth.json");

    let raw_json = match fs::read_to_string(&cache_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };

    #[derive(Deserialize)]
    struct DeviceAuth {
        tokens: std::collections::HashMap<String, TokenEntry>,
    }
    #[derive(Deserialize)]
    struct TokenEntry {
        token: String,
    }

    let auth: DeviceAuth = serde_json::from_str(&raw_json)
        .map_err(|e| format!("failed to parse device-auth.json: {e}"))?;

    Ok(auth.tokens.get("operator").map(|t| t.token.clone()))
}

/// Construct the v3 signature payload string.
///
/// Format: `v3|<deviceId>|<clientId>|<clientMode>|<role>|<scopes-csv>|<signedAtMs>|<token>|<nonce>|<platform>|<deviceFamily>`
#[allow(clippy::too_many_arguments)]
pub fn build_signature_payload(
    device_id: &str,
    client_id: &str,
    client_mode: &str,
    role: &str,
    scopes: &str,
    signed_at_ms: u64,
    token: &str,
    nonce: &str,
    platform: &str,
    device_family: &str,
) -> String {
    format!(
        "v3|{device_id}|{client_id}|{client_mode}|{role}|{scopes}|{signed_at_ms}|{token}|{nonce}|{platform}|{device_family}"
    )
}

/// Sign the payload with the device's Ed25519 key.
///
/// Returns a base64url-encoded (no padding) signature string.
pub fn sign_payload(key: &SigningKey, payload: &str) -> String {
    let signature = key.sign(payload.as_bytes());
    URL_SAFE_NO_PAD.encode(signature.to_bytes())
}

/// Get the public key as a raw base64url-encoded string (no padding).
///
/// The 32-byte raw Ed25519 public key — suitable for inclusion in the
/// connect request.
pub fn public_key_base64url(identity: &DeviceIdentity) -> String {
    URL_SAFE_NO_PAD.encode(&identity.public_key_bytes)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the local device identity cache path.
///
/// Uses the same XDG-aware base directory as `conversation.rs` via
/// [`crate::config_dir`], so both modules resolve to the same path
/// even when `XDG_CONFIG_HOME` is set.
fn device_cache_path() -> Result<PathBuf, AuthError> {
    let base = crate::config_dir().map_err(|e| -> AuthError { BridgeError::ConfigDir(e).into() })?;
    Ok(base.join("openclaw-bridge").join("openclaw-device.json"))
}

/// Parse a JSON string into a [`DeviceIdentity`].
///
/// Attempts the nested format first (`keys.operator.privateKeyPem`).
/// Falls back to the legacy flat format (`privateKeyPem` at top level)
/// for backward compatibility with files fetched from the VPS before this
/// change was deployed.
fn parse_device_identity(raw_json: &str) -> Result<DeviceIdentity, AuthError> {
    // Try nested format first (canonical format for newly generated files).
    if let Ok(nested) = serde_json::from_str::<DeviceIdentityNestedJson>(raw_json) {
        let signing_key = SigningKey::from_pkcs8_pem(&nested.keys.operator.private_key_pem)
            .map_err(|e| format!("failed to parse private key PEM: {e}"))?;
        let verifying_key = signing_key.verifying_key();
        let public_key_bytes = verifying_key.as_bytes().to_vec();
        return Ok(DeviceIdentity {
            device_id: nested.device_id,
            signing_key,
            public_key_bytes,
        });
    }

    // Fall back to legacy flat format.
    let flat = serde_json::from_str::<DeviceIdentityFlatJson>(raw_json)
        .map_err(|e| format!("failed to parse device identity JSON: {e}"))?;
    let signing_key = SigningKey::from_pkcs8_pem(&flat.private_key_pem)
        .map_err(|e| format!("failed to parse private key PEM: {e}"))?;
    let verifying_key = signing_key.verifying_key();
    let public_key_bytes = verifying_key.as_bytes().to_vec();

    Ok(DeviceIdentity {
        device_id: flat.device_id,
        signing_key,
        public_key_bytes,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signature;
    use ed25519_dalek::Verifier;
    use ed25519_dalek::VerifyingKey;

    /// Generate a deterministic Ed25519 keypair for test use (no real credentials).
    fn test_signing_key() -> SigningKey {
        // Deterministic seed — 32 bytes of 0x42.  Not a real key.
        let seed = [0x42u8; 32];
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn build_signature_payload_produces_correct_pipe_delimited_string() {
        let payload = build_signature_payload(
            "deviceabc",
            "gateway-client",
            "operator",
            "operator",
            "operator.admin",
            1_700_000_000_000,
            "tok_abc",
            "nonce123",
            "darwin",
            "",
        );

        assert_eq!(
            payload,
            "v3|deviceabc|gateway-client|operator|operator|operator.admin|1700000000000|tok_abc|nonce123|darwin|"
        );
    }

    #[test]
    fn build_signature_payload_multiple_scopes() {
        let payload = build_signature_payload(
            "dev1",
            "gateway-client",
            "operator",
            "operator",
            "operator.admin,read.all",
            999,
            "t",
            "n",
            "linux",
            "desktop",
        );

        assert_eq!(
            payload,
            "v3|dev1|gateway-client|operator|operator|operator.admin,read.all|999|t|n|linux|desktop"
        );
    }

    #[test]
    fn sign_payload_roundtrip_verify() {
        let key = test_signing_key();
        let payload = "v3|deviceabc|gateway-client|operator|operator|operator.admin|1700000000000|tok|nonce|darwin|";

        let sig_b64 = sign_payload(&key, payload);

        let sig_bytes = URL_SAFE_NO_PAD
            .decode(&sig_b64)
            .expect("base64url decode failed");
        assert_eq!(sig_bytes.len(), 64, "Ed25519 signature must be 64 bytes");

        let sig_array: [u8; 64] = sig_bytes.try_into().expect("signature wrong length");
        let signature = Signature::from_bytes(&sig_array);

        let verifying_key = key.verifying_key();
        verifying_key
            .verify(payload.as_bytes(), &signature)
            .expect("signature verification failed");
    }

    #[test]
    fn sign_payload_is_base64url_no_padding() {
        let key = test_signing_key();
        let sig = sign_payload(&key, "test payload");
        assert!(!sig.contains('+'), "must not contain + (standard base64)");
        assert!(!sig.contains('/'), "must not contain / (standard base64)");
        assert!(!sig.contains('='), "must not contain = padding");
    }

    #[test]
    fn public_key_base64url_returns_32_byte_encoding() {
        let key = test_signing_key();
        let identity = DeviceIdentity {
            device_id: "test".to_string(),
            public_key_bytes: key.verifying_key().as_bytes().to_vec(),
            signing_key: key,
        };

        let encoded = public_key_base64url(&identity);
        let decoded = URL_SAFE_NO_PAD
            .decode(&encoded)
            .expect("base64url decode failed");
        assert_eq!(decoded.len(), 32, "Ed25519 public key must be 32 bytes");
    }

    #[test]
    fn parse_device_identity_with_nested_format() {
        use ed25519_dalek::pkcs8::EncodePrivateKey;

        let original_key = test_signing_key();
        let pem = original_key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .expect("failed to encode PKCS#8 PEM");

        let json = serde_json::json!({
            "version": 1,
            "deviceId": "testdevice001",
            "keys": {
                "operator": {
                    "privateKeyPem": pem.as_str()
                }
            }
        })
        .to_string();

        let identity = parse_device_identity(&json).expect("parse nested format should succeed");

        assert_eq!(identity.device_id, "testdevice001");
        assert_eq!(identity.public_key_bytes.len(), 32);

        let expected_pub = original_key.verifying_key().as_bytes().to_vec();
        assert_eq!(
            identity.public_key_bytes, expected_pub,
            "public key bytes must match original key"
        );
    }

    #[test]
    fn parse_device_identity_with_flat_format_backward_compat() {
        use ed25519_dalek::pkcs8::EncodePrivateKey;

        let original_key = test_signing_key();
        let pem = original_key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .expect("failed to encode PKCS#8 PEM");

        // Legacy flat format (fetched from VPS before this change).
        let json = serde_json::json!({
            "version": 1,
            "deviceId": "testdevice001",
            "privateKeyPem": pem.as_str(),
            "publicKeyPem": "",
            "createdAtMs": 0
        })
        .to_string();

        let identity = parse_device_identity(&json).expect("parse flat format should succeed");

        assert_eq!(identity.device_id, "testdevice001");
        assert_eq!(identity.public_key_bytes.len(), 32);

        let expected_pub = original_key.verifying_key().as_bytes().to_vec();
        assert_eq!(
            identity.public_key_bytes, expected_pub,
            "public key bytes must match original key"
        );
    }

    #[test]
    fn sign_and_verify_end_to_end() {
        use ed25519_dalek::pkcs8::EncodePrivateKey;

        let original_key = test_signing_key();
        let pem = original_key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .expect("failed to encode PKCS#8 PEM");

        let json = serde_json::json!({
            "version": 1,
            "deviceId": "testdevice001",
            "keys": {
                "operator": {
                    "privateKeyPem": pem.as_str()
                }
            }
        })
        .to_string();

        let identity = parse_device_identity(&json).unwrap();

        let payload = build_signature_payload(
            &identity.device_id,
            "gateway-client",
            "operator",
            "operator",
            "operator.admin",
            1_700_000_000_000,
            "gw_token",
            "challenge_nonce",
            std::env::consts::OS,
            "",
        );

        let sig_b64 = sign_payload(&identity.signing_key, &payload);

        let pub_bytes: [u8; 32] = identity
            .public_key_bytes
            .clone()
            .try_into()
            .expect("32 bytes required");
        let verifying_key = VerifyingKey::from_bytes(&pub_bytes).expect("valid public key");

        let sig_bytes: [u8; 64] = URL_SAFE_NO_PAD
            .decode(&sig_b64)
            .expect("decode")
            .try_into()
            .expect("64 bytes");
        let signature = Signature::from_bytes(&sig_bytes);

        verifying_key
            .verify(payload.as_bytes(), &signature)
            .expect("end-to-end signature verification must succeed");
    }

    #[test]
    fn generate_and_persist_identity_creates_file_and_is_parseable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("openclaw-device.json");

        let identity = generate_and_persist_identity(&path).expect("generate should succeed");

        assert!(!identity.device_id.is_empty(), "device_id should not be empty");
        assert_eq!(identity.device_id.len(), 64, "device_id should be 64 hex chars (SHA-256)");
        assert_eq!(identity.public_key_bytes.len(), 32, "public key should be 32 bytes");

        // File should exist and be re-parseable.
        assert!(path.exists(), "identity file should have been created");
        let raw = fs::read_to_string(&path).expect("read identity file");
        let reparsed = parse_device_identity(&raw).expect("re-parse generated identity");
        assert_eq!(reparsed.device_id, identity.device_id);
    }

    #[test]
    fn generate_and_persist_identity_device_id_is_sha256_of_public_key() {
        use sha2::{Digest, Sha256};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("openclaw-device.json");

        let identity = generate_and_persist_identity(&path).expect("generate should succeed");

        // Re-derive the expected device ID from the public key bytes.
        let expected_hash = Sha256::digest(&identity.public_key_bytes);
        let expected_id = hex::encode(expected_hash);

        assert_eq!(
            identity.device_id, expected_id,
            "device_id must equal SHA-256(public_key_bytes) as 64 lowercase hex chars"
        );
    }
}
