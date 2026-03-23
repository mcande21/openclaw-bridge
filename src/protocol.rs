//! OpenClaw WebSocket JSON-RPC protocol v3 frame types.
//!
//! All frames are discriminated by a `type` field:
//!
//! - `"req"`   — client-to-server request
//! - `"res"`   — server-to-client response
//! - `"event"` — server-to-client broadcast event

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Top-level frame
// ---------------------------------------------------------------------------

/// Top-level WebSocket frame. Discriminated by the `type` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WsFrame {
    /// Client-to-server JSON-RPC request.
    Req {
        id: String,
        method: String,
        params: Value,
    },
    /// Server-to-client JSON-RPC response.
    Res {
        id: String,
        ok: bool,
        payload: Option<Value>,
        error: Option<WsError>,
    },
    /// Server-to-client broadcast event.
    Event {
        event: String,
        payload: Value,
        seq: Option<u64>,
    },
}

impl WsFrame {
    /// Build a `connect` request frame.
    pub fn connect_request(id: impl Into<String>, params: ConnectParams) -> Self {
        WsFrame::Req {
            id: id.into(),
            method: "connect".to_string(),
            params: serde_json::to_value(params).expect("ConnectParams is always serializable"),
        }
    }

    /// Build an `agent` request frame.
    pub fn agent_request(id: impl Into<String>, params: AgentParams) -> Self {
        WsFrame::Req {
            id: id.into(),
            method: "agent".to_string(),
            params: serde_json::to_value(params).expect("AgentParams is always serializable"),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Structured error payload on a `Res` frame when `ok` is `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WsError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(rename = "retryAfterMs", skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// connect method types
// ---------------------------------------------------------------------------

/// Parameters for the `connect` method request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectParams {
    #[serde(rename = "minProtocol")]
    pub min_protocol: u32,
    #[serde(rename = "maxProtocol")]
    pub max_protocol: u32,
    pub client: ClientInfo,
    pub role: String,
    pub scopes: Vec<String>,
    pub auth: AuthParams,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(rename = "userAgent", skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// Identifies the connecting client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientInfo {
    pub id: String,
    pub version: String,
    pub platform: String,
    pub mode: String,
}

/// Authentication credentials sent with `connect`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Pre-issued device token for returning paired devices.
    #[serde(rename = "deviceToken", skip_serializing_if = "Option::is_none")]
    pub device_token: Option<String>,
}

/// Optional device attestation parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceParams {
    pub id: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub signature: String,
    #[serde(rename = "signedAt")]
    pub signed_at: u64,
    pub nonce: String,
}

// ---------------------------------------------------------------------------
// agent method types
// ---------------------------------------------------------------------------

/// Parameters for the `agent` method request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentParams {
    pub message: String,
    #[serde(rename = "agentId", skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
    #[serde(rename = "sessionKey", skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

// ---------------------------------------------------------------------------
// Event payload types
// ---------------------------------------------------------------------------

/// Payload of the `connect.challenge` event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengePayload {
    pub nonce: String,
    pub ts: u64,
}

/// Payload of `agent` event broadcasts (streaming delta/tool_call/done).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEventPayload {
    #[serde(rename = "runId")]
    pub run_id: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
    /// The delta/tool_call/done payload — shape varies by event kind.
    pub data: Value,
}

// ---------------------------------------------------------------------------
// Convenience: generate a fresh request ID
// ---------------------------------------------------------------------------

/// Generate a new UUID v4 string suitable for use as a request `id`.
pub fn new_request_id() -> String {
    Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- RequestFrame roundtrip ---

    #[test]
    fn req_frame_roundtrip() {
        let frame = WsFrame::Req {
            id: "req-001".to_string(),
            method: "agent".to_string(),
            params: json!({"message": "hello"}),
        };

        let serialized = serde_json::to_string(&frame).unwrap();
        let deserialized: WsFrame = serde_json::from_str(&serialized).unwrap();
        assert_eq!(frame, deserialized);
    }

    #[test]
    fn req_frame_type_field() {
        let frame = WsFrame::Req {
            id: "x".to_string(),
            method: "connect".to_string(),
            params: json!({}),
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "req");
        assert_eq!(v["id"], "x");
        assert_eq!(v["method"], "connect");
    }

    // --- ResponseFrame roundtrip ---

    #[test]
    fn res_frame_ok_roundtrip() {
        let frame = WsFrame::Res {
            id: "req-001".to_string(),
            ok: true,
            payload: Some(json!({"status": "connected"})),
            error: None,
        };

        let serialized = serde_json::to_string(&frame).unwrap();
        let deserialized: WsFrame = serde_json::from_str(&serialized).unwrap();
        assert_eq!(frame, deserialized);
    }

    #[test]
    fn res_frame_error_roundtrip() {
        let frame = WsFrame::Res {
            id: "req-002".to_string(),
            ok: false,
            payload: None,
            error: Some(WsError {
                code: "AUTH_FAILED".to_string(),
                message: "Invalid token".to_string(),
                details: None,
                retryable: false,
                retry_after_ms: None,
            }),
        };

        let serialized = serde_json::to_string(&frame).unwrap();
        let deserialized: WsFrame = serde_json::from_str(&serialized).unwrap();
        assert_eq!(frame, deserialized);
    }

    #[test]
    fn res_frame_type_field() {
        let frame = WsFrame::Res {
            id: "y".to_string(),
            ok: true,
            payload: None,
            error: None,
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "res");
    }

    // --- EventFrame roundtrip ---

    #[test]
    fn event_frame_roundtrip() {
        let frame = WsFrame::Event {
            event: "agent.delta".to_string(),
            payload: json!({"text": "hello world"}),
            seq: Some(42),
        };

        let serialized = serde_json::to_string(&frame).unwrap();
        let deserialized: WsFrame = serde_json::from_str(&serialized).unwrap();
        assert_eq!(frame, deserialized);
    }

    #[test]
    fn event_frame_type_field() {
        let frame = WsFrame::Event {
            event: "connect.challenge".to_string(),
            payload: json!({}),
            seq: None,
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "event");
    }

    // --- Serde rename verification ---

    #[test]
    fn ws_error_retry_after_ms_rename() {
        let err = WsError {
            code: "RATE_LIMITED".to_string(),
            message: "Too many requests".to_string(),
            details: None,
            retryable: true,
            retry_after_ms: Some(5000),
        };
        let v: Value = serde_json::to_value(&err).unwrap();
        assert!(
            v.get("retryAfterMs").is_some(),
            "should serialize as retryAfterMs"
        );
        assert!(
            v.get("retry_after_ms").is_none(),
            "should not have snake_case key"
        );
    }

    #[test]
    fn connect_params_field_renames() {
        let params = ConnectParams {
            min_protocol: 3,
            max_protocol: 3,
            client: ClientInfo {
                id: "cli-001".to_string(),
                version: "0.1.0".to_string(),
                platform: "darwin".to_string(),
                mode: "cli".to_string(),
            },
            role: "client".to_string(),
            scopes: vec!["agent:send".to_string()],
            auth: AuthParams {
                token: Some("tok_abc".to_string()),
                device_token: None,
            },
            device: None,
            locale: None,
            user_agent: Some("openclaw-bridge/0.1.0".to_string()),
        };
        let v: Value = serde_json::to_value(&params).unwrap();
        assert!(v.get("minProtocol").is_some());
        assert!(v.get("maxProtocol").is_some());
        assert!(v.get("userAgent").is_some());
        assert!(v.get("min_protocol").is_none());
    }

    #[test]
    fn agent_params_field_renames() {
        let params = AgentParams {
            message: "run this".to_string(),
            agent_id: Some("edi".to_string()),
            idempotency_key: "idem-123".to_string(),
            session_key: None,
            thinking: None,
            timeout: None,
        };
        let v: Value = serde_json::to_value(&params).unwrap();
        assert!(v.get("agentId").is_some());
        assert!(v.get("idempotencyKey").is_some());
        assert!(v.get("agent_id").is_none());
    }

    #[test]
    fn device_params_field_renames() {
        let device = DeviceParams {
            id: "dev-001".to_string(),
            public_key: "pk_abc".to_string(),
            signature: "sig_abc".to_string(),
            signed_at: 1_700_000_000,
            nonce: "nonce123".to_string(),
        };
        let v: Value = serde_json::to_value(&device).unwrap();
        assert!(v.get("publicKey").is_some());
        assert!(v.get("signedAt").is_some());
        assert!(v.get("public_key").is_none());
        assert!(v.get("signed_at").is_none());
    }

    // --- Helper constructors ---

    #[test]
    fn connect_request_constructor() {
        let params = ConnectParams {
            min_protocol: 3,
            max_protocol: 3,
            client: ClientInfo {
                id: "cli".to_string(),
                version: "0.1.0".to_string(),
                platform: "darwin".to_string(),
                mode: "cli".to_string(),
            },
            role: "client".to_string(),
            scopes: vec![],
            auth: AuthParams {
                token: Some("tok".to_string()),
                device_token: None,
            },
            device: None,
            locale: None,
            user_agent: None,
        };
        let frame = WsFrame::connect_request("id-1", params);
        match &frame {
            WsFrame::Req { method, .. } => assert_eq!(method, "connect"),
            _ => panic!("expected Req variant"),
        }
    }

    #[test]
    fn agent_request_constructor() {
        let params = AgentParams {
            message: "hello".to_string(),
            agent_id: None,
            idempotency_key: "idem-1".to_string(),
            session_key: None,
            thinking: None,
            timeout: None,
        };
        let frame = WsFrame::agent_request("id-2", params);
        match &frame {
            WsFrame::Req { method, .. } => assert_eq!(method, "agent"),
            _ => panic!("expected Req variant"),
        }
    }

    #[test]
    fn new_request_id_is_valid_uuid() {
        let id = new_request_id();
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "new_request_id should return a valid UUID"
        );
    }
}
