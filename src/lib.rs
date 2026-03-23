//! `openclaw-bridge` — local transport infrastructure for connecting Claude Code
//! to OpenClaw gateways.
//!
//! Provides the core primitives for connecting from a local Claude Code instance
//! to a remote OpenClaw gateway over WebSocket:
//!
//! - **[`auth`]** — Ed25519 device identity: load, cache, sign, verify. Handles
//!   the challenge/response handshake required by the OpenClaw gateway.
//!
//! - **[`protocol`]** — JSON-RPC v3 frame types (`WsFrame`, `ConnectParams`,
//!   `AgentParams`, etc.). The wire format for all WebSocket communication.
//!
//! - **[`ws`]** — Async WebSocket client ([`ws::WsClient`]). Manages the full
//!   connection lifecycle: connect, authenticate, send/receive agent messages,
//!   disconnect. Exposes `next_frame` for TUI streaming consumers.
//!
//! - **[`ssh`]** — SSH transport helpers. Wraps `ssh` CLI for gateway status
//!   queries, agent spawning, and workspace operations that don't require the
//!   WebSocket path.
//!
//! - **[`conversation`]** — Thread persistence layer. JSONL-backed storage for
//!   local-to-gateway conversation history, with auto-archiving after 48h inactivity.
//!
//! # Quick start
//!
//! ```no_run
//! use openclaw_bridge::ws::WsClient;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     let token = std::env::var("OPENCLAW_TOKEN")?;
//!     let mut client = WsClient::connect("your-gateway-host", 18789, &token).await?;
//!     let result = client.agent_chat("main", "Hello from Claude Code", None).await?;
//!     println!("{}", result.text);
//!     client.disconnect().await?;
//!     Ok(())
//! }
//! ```

pub mod auth;
pub mod conversation;
pub mod error;
pub mod protocol;
pub mod ssh;
pub mod ws;

#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "cli")]
pub mod watch;

use std::sync::atomic::{AtomicBool, Ordering};

/// Global verbose flag. When `true`, the library emits `[ws]` and `[auth]`
/// diagnostic messages to stderr. Disabled by default.
///
/// Set via [`set_verbose`] from the binary before making any library calls.
pub static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose diagnostic output from the library.
///
/// Call this once at startup from the `ocb` binary when `--verbose` is passed.
pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::Relaxed);
}

/// Macro for library-internal diagnostic output.
///
/// Prints to stderr only when [`VERBOSE`] is `true`. Suppressed by default
/// so that `ocb` output is clean JSON when running as a non-interactive tool.
#[macro_export]
macro_rules! verbose {
    ($($arg:tt)*) => {
        if $crate::VERBOSE.load(::std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($arg)*);
        }
    };
}

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Gateway token loading
// ---------------------------------------------------------------------------

/// Load the gateway token from the environment or local token files.
///
/// Resolution order:
/// 1. `OPENCLAW_TOKEN` env var (highest priority)
/// 2. `~/.config/openclaw-bridge/gateway-token` (plain text, 0600)
///
/// Returns the token as a trimmed `String`, or an error message if none is
/// found. Call sites in `main.rs` and `tui/ws_task.rs` both use this function.
pub fn load_gateway_token() -> Result<String, String> {
    // 1. Environment variable.
    if let Ok(token) = std::env::var("OPENCLAW_TOKEN") {
        let t = token.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }

    // 2. Token file.
    let config_base = config_dir()?;
    let candidates = [
        config_base.join("openclaw-bridge").join("gateway-token"),
    ];
    for path in &candidates {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let token = contents.trim().to_string();
                if !token.is_empty() {
                    return Ok(token);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                verbose!(
                    "[token] warning: failed to read token file {}: {e}",
                    path.display()
                );
            }
        }
    }

    Err(
        "No gateway token found: OPENCLAW_TOKEN not set and no token file found at ~/.config/openclaw-bridge/gateway-token".to_string(),
    )
}

/// Default WebSocket port for the OpenClaw gateway.
pub const WS_PORT: u16 = 18789;

/// Default host for the OpenClaw gateway.
///
/// Set `OPENCLAW_HOST` to your gateway's IP or hostname. `localhost` is the
/// safe default for local development; remote connections require this env var.
pub const DEFAULT_WS_HOST: &str = "localhost";

/// Resolve the WebSocket host.
///
/// Resolution order:
/// 1. `OPENCLAW_WS_HOST` — WS-specific override (must be an IP or hostname)
/// 2. `OPENCLAW_HOST` — shared fallback
/// 3. [`DEFAULT_WS_HOST`] — `localhost` (safe default; set env var for remote)
///
/// Setting `OPENCLAW_HOST` to an SSH alias would silently break WS connections;
/// this ladder allows independent overrides while keeping a shared default.
pub fn resolve_ws_host() -> String {
    std::env::var("OPENCLAW_WS_HOST")
        .or_else(|_| std::env::var("OPENCLAW_HOST"))
        .unwrap_or_else(|_| DEFAULT_WS_HOST.to_string())
}

/// Return the platform config base directory.
///
/// Resolution order:
/// 1. `XDG_CONFIG_HOME` — honoured on all platforms for portability
/// 2. `~/.config` via [`directories::BaseDirs`]
///
/// Both `auth.rs` and `conversation.rs` call this to ensure they resolve to the
/// same directory regardless of platform or `XDG_CONFIG_HOME` overrides.
pub fn config_dir() -> Result<PathBuf, String> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg));
    }
    if let Some(base) = directories::BaseDirs::new() {
        return Ok(base.home_dir().join(".config"));
    }
    Err("cannot determine config directory".to_string())
}

/// Bridge an async future into the current sync context.
///
/// Uses `tokio::task::block_in_place` when already inside a tokio runtime
/// (the binary runs under `#[tokio::main]`), so the calling thread can block
/// without starving the executor. Falls back to spinning up a fresh runtime
/// when no handle is available (e.g. unit tests).
pub fn block_on_async<F, T>(
    future: F,
) -> Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>
where
    F: std::future::Future<
        Output = Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>,
    >,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
        Err(_) => {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
            rt.block_on(future)
        }
    }
}
