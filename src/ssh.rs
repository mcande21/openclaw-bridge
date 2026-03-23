//! SSH transport helpers for the OpenClaw connector.

use std::process::Command;

use crate::error::BridgeError;

// ---------------------------------------------------------------------------
// SSH transport
// ---------------------------------------------------------------------------

/// Run a command on the OpenClaw VPS over SSH and return stdout as a String.
pub fn run_ssh(host: &str, cmd: &str) -> Result<String, BridgeError> {
    let output = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, cmd])
        .output()
        .map_err(|e| BridgeError::Ssh(format!("failed to run ssh: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BridgeError::Ssh(format!(
            "ssh command failed: {}",
            stderr.trim()
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| BridgeError::Ssh(format!("ssh returned non-UTF8 output: {e}")))
}

/// Run a command on the OpenClaw VPS over SSH and parse the output as JSON.
pub fn run_ssh_json(host: &str, cmd: &str) -> Result<serde_json::Value, BridgeError> {
    let raw = run_ssh(host, cmd)?;
    serde_json::from_str(raw.trim())
        .map_err(|e| BridgeError::Ssh(format!("failed to parse JSON from ssh output: {e}\nraw: {raw}")))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the OpenClaw SSH host from the environment or a default.
///
/// Resolution order:
/// 1. `OPENCLAW_SSH_HOST` — SSH-specific override
/// 2. `OPENCLAW_HOST` — shared fallback (must be an SSH alias or hostname)
/// 3. `"openclaw"` — default SSH alias from `~/.ssh/config`
pub fn resolve_host() -> String {
    std::env::var("OPENCLAW_SSH_HOST")
        .or_else(|_| std::env::var("OPENCLAW_HOST"))
        .unwrap_or_else(|_| "openclaw".to_string())
}

/// Minimally escape a string for safe use as a shell argument in single quotes.
///
/// Single-quotes everything and escapes any embedded single-quotes.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Validate that an identifier contains only alphanumeric characters, dashes,
/// and underscores, and is non-empty.
///
/// Agent IDs interpolated directly into SSH shell commands must never contain
/// shell metacharacters. Rejecting invalid input early is safer than relying
/// solely on quoting.
pub fn validate_id(id: &str) -> Result<(), BridgeError> {
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(BridgeError::InvalidInput(format!(
            "invalid identifier (alphanumeric, dash, and underscore only): {id:?}"
        )))
    }
}

/// Validate a workspace filename — allows alphanumeric, dash, underscore, and
/// dot (for extensions like `SOUL.md`), but rejects path traversal sequences
/// (`..`, `/`, `\`) and any shell metacharacters.
pub fn validate_filename(name: &str) -> Result<(), BridgeError> {
    if name.is_empty() {
        return Err(BridgeError::InvalidInput(
            "filename must not be empty".to_string(),
        ));
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(BridgeError::InvalidInput(format!(
            "invalid filename (path traversal not allowed): {name:?}"
        )));
    }
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err(BridgeError::InvalidInput(format!(
            "invalid filename (alphanumeric, dash, underscore, and dot only): {name:?}"
        )))
    }
}
