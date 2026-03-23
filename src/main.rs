//! `ocb` — OpenClaw Bridge binary.
//!
//! Thin dispatcher: parse args, call library functions, emit JSON, exit with
//! structured codes.
//!
//! Exit codes:
//!   0 — success
//!   1 — user error (bad args, missing env, not found)
//!   2 — network / gateway error
//!   3 — auth error

mod cli;

use std::io::Write as _;
use std::process;
use std::time::Duration;

use clap::Parser;
use serde_json::{Value, json};

use cli::{AuthCommand, Cli, Command, ConversationCommand, WorkspaceCommand};
use openclaw_bridge::conversation::find_thread_by_prefix;
use openclaw_bridge::conversation::{self, MessageRole};
use openclaw_bridge::watch;
use openclaw_bridge::ws::WsClient;
use openclaw_bridge::{auth, block_on_async, resolve_ws_host, ssh};

// ---------------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------------

const EXIT_OK: i32 = 0;
const EXIT_USER_ERROR: i32 = 1;
const EXIT_NETWORK_ERROR: i32 = 2;
const EXIT_AUTH_ERROR: i32 = 3;

// ---------------------------------------------------------------------------
// Output configuration
// ---------------------------------------------------------------------------

/// Flags that control how command output is formatted and filtered.
#[derive(Clone, Copy)]
struct OutputConfig {
    /// Emit indented JSON. When false (default), compact single-line JSON.
    pretty: bool,
    /// Emit full unfiltered output. When false (default), summary projection.
    full: bool,
    /// Emit bare text for chat responses (no JSON envelope).
    bare: bool,
    /// Truncate response text to at most this many characters.
    max_chars: Option<usize>,
    /// Stream agent deltas to stderr as they arrive.
    stream: bool,
}

fn main() {
    let cli = Cli::parse();

    // Wire up verbose flag before any library calls.
    if cli.verbose {
        openclaw_bridge::set_verbose(true);
    }

    let out = OutputConfig {
        pretty: cli.pretty,
        full: cli.full,
        bare: cli.bare,
        max_chars: cli.max_chars,
        stream: cli.stream,
    };

    match run(cli, out) {
        Ok(value) => {
            let output = if out.pretty {
                serde_json::to_string_pretty(&value).unwrap()
            } else {
                serde_json::to_string(&value).unwrap()
            };
            println!("{output}");
            process::exit(EXIT_OK);
        }
        Err(e) => {
            eprintln!("{}", e.json());
            process::exit(e.code);
        }
    }
}

// ---------------------------------------------------------------------------
// Structured error
// ---------------------------------------------------------------------------

struct CmdError {
    message: String,
    /// Machine-readable error code string (always present).
    error_code: String,
    /// Optional hint for the caller.
    hint: Option<String>,
    /// Process exit code.
    code: i32,
}

impl CmdError {
    fn user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_code: "INVALID_ARGS".to_string(),
            hint: None,
            code: EXIT_USER_ERROR,
        }
    }

    fn network(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_code: "GATEWAY_UNREACHABLE".to_string(),
            hint: Some(
                "Check that the OpenClaw gateway is running and OPENCLAW_HOST is correct"
                    .to_string(),
            ),
            code: EXIT_NETWORK_ERROR,
        }
    }

    fn auth(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_code: "AUTH_ERROR".to_string(),
            hint: Some("Run `ocb pair` to authenticate this device with the gateway.".to_string()),
            code: EXIT_AUTH_ERROR,
        }
    }

    fn with_code(mut self, code: impl Into<String>) -> Self {
        self.error_code = code.into();
        self
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("error".to_string(), json!(self.message));
        obj.insert("code".to_string(), json!(self.error_code));
        if let Some(ref h) = self.hint {
            obj.insert("hint".to_string(), json!(h));
        }
        serde_json::to_string_pretty(&Value::Object(obj)).unwrap()
    }
}

// Classify box errors into structured CmdError values by inspecting the message.
//
// Pattern matching follows the error strings emitted by ws.rs and ssh.rs.
fn classify_box_error(e: Box<dyn std::error::Error + Send + Sync>) -> CmdError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();

    // Auth / pairing errors — inspect before generic auth check.
    if lower.contains("not_paired") || lower.contains("not paired") || lower.contains("pairing required") {
        return CmdError::auth(msg)
            .with_code("PAIRING_REQUIRED")
            .with_hint("Run `ocb pair` to initiate pairing, then approve on the gateway");
    }
    if lower.contains("device identity mismatch") || lower.contains("identity mismatch") {
        return CmdError::auth(msg)
            .with_code("IDENTITY_MISMATCH")
            .with_hint("Run `ocb auth reset` to regenerate device identity, then re-pair");
    }
    if lower.contains("unauthorized") || lower.contains("forbidden") || lower.contains("token expired") || lower.contains("auth_expired") || lower.contains("auth_failed") {
        return CmdError::auth(msg)
            .with_code("TOKEN_EXPIRED")
            .with_hint("Device token expired. Run `ocb pair` to re-authenticate");
    }
    if lower.contains("openclaw device token not configured") || lower.contains("token not configured") {
        return CmdError::auth(msg)
            .with_code("TOKEN_MISSING")
            .with_hint(
                "Set OPENCLAW_TOKEN env var or write token to ~/.config/openclaw-bridge/gateway-token",
            );
    }

    // Network errors.
    if lower.contains("handshake timed out") || lower.contains("agent_chat timed out") {
        return CmdError::network(msg)
            .with_code("GATEWAY_TIMEOUT")
            .with_hint("Check that the OpenClaw gateway is running and OPENCLAW_HOST is correct");
    }
    if lower.contains("websocket connect to") || lower.contains("connection refused") || lower.contains("connection reset") || lower.contains("no route to host") {
        return CmdError::network(msg)
            .with_code("GATEWAY_UNREACHABLE")
            .with_hint("Check that the OpenClaw gateway is running and OPENCLAW_HOST is correct");
    }
    if lower.contains("websocket closed") || lower.contains("stream ended unexpectedly") || lower.contains("connection dropped") {
        return CmdError::network(msg)
            .with_code("WS_DISCONNECT")
            .with_hint("WebSocket disconnected mid-conversation; retry the request");
    }
    if lower.contains("ssh command failed") || lower.contains("failed to run ssh") {
        // Extract host from message if present (best-effort).
        let host = std::env::var("OPENCLAW_SSH_HOST")
            .or_else(|_| std::env::var("OPENCLAW_HOST"))
            .unwrap_or_else(|_| "openclaw".to_string());
        return CmdError::network(msg)
            .with_code("SSH_ERROR")
            .with_hint(format!("Check SSH connectivity: ssh {host} openclaw --version"));
    }

    // Generic auth fallback.
    if lower.contains("auth") {
        return CmdError::auth(msg)
            .with_code("AUTH_ERROR")
            .with_hint("Run `ocb pair` to authenticate this device with the gateway.");
    }

    // IO errors.
    if lower.contains("i/o error") || lower.contains("io error") || lower.contains("no such file") || lower.contains("permission denied") {
        return CmdError {
            message: msg,
            error_code: "IO_ERROR".to_string(),
            hint: None,
            code: EXIT_USER_ERROR,
        };
    }

    // Default: treat as network error.
    CmdError::network(msg)
        .with_code("GATEWAY_UNREACHABLE")
}

// ---------------------------------------------------------------------------
// Token loading
// ---------------------------------------------------------------------------

/// Load the gateway token, mapping the library error into a `CmdError`.
fn load_gateway_token() -> Result<String, CmdError> {
    openclaw_bridge::load_gateway_token().map_err(|_| {
        CmdError::auth(
            "No gateway token found: OPENCLAW_TOKEN not set and no token file found",
        )
        .with_code("TOKEN_MISSING")
        .with_hint(
            "Set OPENCLAW_TOKEN env var or write token to ~/.config/openclaw-bridge/gateway-token",
        )
    })
}

fn ws_host() -> String {
    resolve_ws_host()
}

fn ws_port() -> u16 {
    std::env::var("OPENCLAW_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(openclaw_bridge::WS_PORT)
}

fn ssh_host() -> String {
    ssh::resolve_host()
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

fn run(cli: Cli, out: OutputConfig) -> Result<Value, CmdError> {
    match cli.command {
        Command::Chat {
            agent,
            message,
            session,
            timeout,
        } => cmd_chat(&agent, &message, session.as_deref(), timeout, out),

        Command::Spawn { agent, message } => cmd_spawn(&agent, &message),

        Command::Status => cmd_status(out),

        Command::Agents => cmd_agents(out),

        Command::Workspace { command } => match command {
            WorkspaceCommand::List { agent } => cmd_workspace_list(&agent),
            WorkspaceCommand::Read { agent, filename } => cmd_workspace_read(&agent, &filename),
        },

        Command::Conversation { command } => match command {
            ConversationCommand::New { agent } => cmd_conversation_new(&agent),
            ConversationCommand::Send {
                thread,
                agent,
                message,
                timeout,
            } => cmd_conversation_send(thread.as_deref(), agent.as_deref(), &message, timeout, out),
            ConversationCommand::History { thread_id, last } => {
                cmd_conversation_history(&thread_id, last)
            }
            ConversationCommand::List => cmd_conversation_list(out),
        },

        Command::Send { thread, message, timeout } => cmd_send(&thread, &message, timeout, out),

        Command::Watch {
            thread,
            session,
            debounce,
            context,
            timeout,
        } => cmd_watch(&thread, &session, debounce, context, timeout),

        Command::Pair { gateway } => cmd_pair(gateway.as_deref()),

        Command::Auth { command } => match command {
            AuthCommand::Status => cmd_auth_status(),
            AuthCommand::Reset => cmd_auth_reset(),
        },

        Command::Version => cmd_version(),

        #[cfg(feature = "tui")]
        Command::Tui { thread } => cmd_tui(thread),
    }
}

// ---------------------------------------------------------------------------
// chat
// ---------------------------------------------------------------------------

fn cmd_chat(
    agent_id: &str,
    message: &str,
    session: Option<&str>,
    timeout_secs: u64,
    out: OutputConfig,
) -> Result<Value, CmdError> {
    let token = load_gateway_token()?;
    let host = ws_host();
    let port = ws_port();
    let chat_timeout = Duration::from_secs(timeout_secs);

    let result = if out.stream {
        block_on_async(async {
            let mut client = WsClient::connect(&host, port, &token).await?;
            let result = client
                .agent_chat_streaming(agent_id, message, session, chat_timeout, |delta| {
                    eprint!("{delta}");
                    let _ = std::io::stderr().flush();
                })
                .await?;
            client.disconnect().await?;
            Ok(result)
        })
        .map_err(classify_box_error)?
    } else {
        block_on_async(async {
            let mut client = WsClient::connect(&host, port, &token).await?;
            let result = client
                .agent_chat_with_timeout(agent_id, message, session, chat_timeout)
                .await?;
            client.disconnect().await?;
            Ok(result)
        })
        .map_err(classify_box_error)?
    };

    // --bare: skip JSON entirely, print raw text and exit.
    if out.bare {
        println!("{}", result.text);
        process::exit(EXIT_OK);
    }

    // Apply --max-chars truncation.
    let text = result.text;
    if let Some(max) = out.max_chars
        && text.len() > max
    {
        let boundary = floor_char_boundary(&text, max);
        let truncated = &text[..boundary];
        return Ok(json!({
            "text": truncated,
            "run_id": result.run_id,
            "truncated": true,
            "full_chars": text.len(),
        }));
    }

    if out.full {
        Ok(json!({
            "run_id": result.run_id,
            "text": text,
            "agent": agent_id,
        }))
    } else {
        Ok(json!({
            "text": text,
            "run_id": result.run_id,
        }))
    }
}

/// Find the largest character boundary at or before `index` in `s`.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

fn cmd_spawn(agent_id: &str, message: &str) -> Result<Value, CmdError> {
    ssh::validate_id(agent_id).map_err(|e| CmdError::user(e.to_string()))?;
    let escaped = ssh::shell_escape(message);
    let cmd = format!("openclaw agent spawn --agent {agent_id} --message {escaped} --json");
    let host = ssh_host();
    let output = ssh::run_ssh_json(&host, &cmd).map_err(|e| CmdError::network(e.to_string()))?;
    Ok(output)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn cmd_status(out: OutputConfig) -> Result<Value, CmdError> {
    let host = ssh_host();
    let output = ssh::run_ssh_json(&host, "openclaw gateway status --json")
        .map_err(|e| CmdError::network(e.to_string()))?;

    if out.full {
        return Ok(output);
    }

    // Summary projection: extract only the fields agents need to verify health.
    // JSON pointer paths navigate the nested gateway status object.
    let mut summary = serde_json::Map::new();
    let up = output
        .pointer("/service/runtime/state")
        .and_then(|v| v.as_str())
        == Some("running");
    summary.insert("up".to_string(), json!(up));
    if let Some(port) = output.pointer("/gateway/port").and_then(|v| v.as_u64()) {
        summary.insert("port".to_string(), json!(port));
    }
    if let Some(pid) = output.pointer("/service/runtime/pid").and_then(|v| v.as_u64()) {
        summary.insert("pid".to_string(), json!(pid));
    }
    Ok(Value::Object(summary))
}

// ---------------------------------------------------------------------------
// agents
// ---------------------------------------------------------------------------

fn cmd_agents(out: OutputConfig) -> Result<Value, CmdError> {
    let host = ssh_host();
    let output = ssh::run_ssh_json(&host, "openclaw sessions --all-agents --json")
        .map_err(|e| CmdError::network(e.to_string()))?;

    if out.full {
        return Ok(output);
    }

    // Summary projection: count + essential identity fields per agent.
    let sessions = output.get("sessions").and_then(|v| v.as_array());
    let summary: Vec<Value> = sessions
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    json!({
                        "id": s.get("agentId"),
                        "model": s.get("model"),
                        "key": s.get("key"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({ "count": summary.len(), "agents": summary }))
}

// ---------------------------------------------------------------------------
// workspace list
// ---------------------------------------------------------------------------

fn cmd_workspace_list(agent_id: &str) -> Result<Value, CmdError> {
    ssh::validate_id(agent_id).map_err(|e| CmdError::user(e.to_string()))?;
    let cmd = format!("openclaw workspace list --id {agent_id} --json");
    let host = ssh_host();
    let output = ssh::run_ssh_json(&host, &cmd).map_err(|e| CmdError::network(e.to_string()))?;
    Ok(output)
}

// ---------------------------------------------------------------------------
// workspace read
// ---------------------------------------------------------------------------

fn cmd_workspace_read(agent_id: &str, filename: &str) -> Result<Value, CmdError> {
    ssh::validate_id(agent_id).map_err(|e| CmdError::user(e.to_string()))?;
    ssh::validate_filename(filename).map_err(|e| CmdError::user(e.to_string()))?;
    let cmd = format!("openclaw workspace read {agent_id} {filename}");
    let host = ssh_host();
    let content =
        ssh::run_ssh(&host, &cmd).map_err(|e| CmdError::network(e.to_string()))?;
    Ok(json!({
        "agent": agent_id,
        "filename": filename,
        "content": content,
    }))
}

// ---------------------------------------------------------------------------
// conversation new
// ---------------------------------------------------------------------------

fn cmd_conversation_new(agent_id: &str) -> Result<Value, CmdError> {
    let thread =
        conversation::create_thread(agent_id).map_err(|e| CmdError::user(e.to_string()))?;
    Ok(json!({
        "thread_id": thread.id,
        "agent_id": thread.agent_id,
        "session_key": thread.session_key,
        "created_at": thread.created_at,
    }))
}

// ---------------------------------------------------------------------------
// conversation send
// ---------------------------------------------------------------------------

fn cmd_conversation_send(
    thread_id: Option<&str>,
    agent_id: Option<&str>,
    message: &str,
    timeout_secs: u64,
    out: OutputConfig,
) -> Result<Value, CmdError> {
    // Resolve thread — either use the provided ID or find/create for agent.
    let thread = match (thread_id, agent_id) {
        (Some(tid), _) => {
            // Validate the thread exists by listing threads.
            let threads =
                conversation::list_threads().map_err(|e| CmdError::user(e.to_string()))?;
            threads
                .into_iter()
                .find(|t| t.id == tid)
                .map(|entry| openclaw_bridge::conversation::Thread {
                    id: entry.id.clone(),
                    agent_id: entry.agent_id.clone(),
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                    archived: entry.archived,
                    session_key: format!("ocb:{}", entry.id),
                })
                .ok_or_else(|| {
                    CmdError::user(format!("thread not found: {tid}"))
                        .with_code("THREAD_NOT_FOUND")
                })?
        }
        (None, Some(aid)) => {
            // Find the most recent active thread for this agent, or create one.
            let threads =
                conversation::list_threads().map_err(|e| CmdError::user(e.to_string()))?;
            match threads.into_iter().rfind(|t| t.agent_id == aid) {
                Some(entry) => openclaw_bridge::conversation::Thread {
                    id: entry.id.clone(),
                    agent_id: entry.agent_id.clone(),
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                    archived: entry.archived,
                    session_key: format!("ocb:{}", entry.id),
                },
                None => conversation::create_thread(aid)
                    .map_err(|e| CmdError::user(e.to_string()))?,
            }
        }
        (None, None) => {
            return Err(CmdError::user(
                "one of --thread or --agent is required for conversation send",
            )
            .with_code("MISSING_ARG"));
        }
    };

    // Persist the outbound user message (unprefixed, original text).
    conversation::append_message(&thread.id, MessageRole::User, message, None, Some("cli"))
        .map_err(|e| CmdError::user(e.to_string()))?;

    // Send to gateway with [Claude Code] prefix so the remote agent knows who is sending.
    // The JSONL stores the original unprefixed message (persisted above).
    let wire_message = format!("[Claude Code] {message}");
    let token = load_gateway_token()?;
    let host = ws_host();
    let port = ws_port();
    let chat_timeout = Duration::from_secs(timeout_secs);
    let session_key = thread.session_key.clone();
    let thread_id = thread.id.clone();
    let agent_id = thread.agent_id.clone();

    let result = if out.stream {
        block_on_async(async {
            let mut client = WsClient::connect(&host, port, &token).await?;
            let result = client
                .agent_chat_streaming(
                    &agent_id,
                    &wire_message,
                    Some(&session_key),
                    chat_timeout,
                    |delta| {
                        eprint!("{delta}");
                        let _ = std::io::stderr().flush();
                    },
                )
                .await?;
            client.disconnect().await?;
            Ok(result)
        })
        .map_err(classify_box_error)?
    } else {
        block_on_async(async {
            let mut client = WsClient::connect(&host, port, &token).await?;
            let result = client
                .agent_chat_with_timeout(&agent_id, &wire_message, Some(&session_key), chat_timeout)
                .await?;
            client.disconnect().await?;
            Ok(result)
        })
        .map_err(classify_box_error)?
    };

    // Persist the assistant response (source: None for remote agent).
    conversation::append_message(
        &thread_id,
        MessageRole::Assistant,
        &result.text,
        Some(&result.run_id),
        None,
    )
    .map_err(|e| CmdError::user(e.to_string()))?;

    // --bare: skip JSON, print raw text and exit.
    if out.bare {
        println!("{}", result.text);
        process::exit(EXIT_OK);
    }

    // Apply --max-chars truncation.
    let text = result.text;
    if let Some(max) = out.max_chars
        && text.len() > max
    {
        let boundary = floor_char_boundary(&text, max);
        let truncated = &text[..boundary];
        return Ok(json!({
            "thread_id": thread_id,
            "run_id": result.run_id,
            "text": truncated,
            "truncated": true,
            "full_chars": text.len(),
        }));
    }

    if out.full {
        Ok(json!({
            "thread_id": thread_id,
            "run_id": result.run_id,
            "agent": agent_id,
            "text": text,
        }))
    } else {
        Ok(json!({
            "text": text,
            "run_id": result.run_id,
        }))
    }
}

// ---------------------------------------------------------------------------
// conversation history
// ---------------------------------------------------------------------------

fn cmd_conversation_history(thread_ref: &str, last: usize) -> Result<Value, CmdError> {
    // Resolve prefix via lightweight index read (not full JSONL).
    let thread_id = match find_thread_by_prefix(thread_ref)
        .map_err(|e| CmdError::user(e.to_string()))?
    {
        Some(entry) => entry.id,
        None => {
            return Err(
                CmdError::user(format!("no thread matching: {thread_ref}"))
                    .with_code("THREAD_NOT_FOUND")
                    .with_hint("Use `ocb conversation list` to see available threads"),
            );
        }
    };

    // Offline command: works from local JSONL only, no network required.
    let all_messages = match conversation::read_thread(&thread_id) {
        Ok(msgs) => msgs,
        Err(openclaw_bridge::conversation::ConversationError::ThreadNotFound(_)) => {
            return Err(
                CmdError::user(format!("thread not found: {thread_id}"))
                    .with_code("THREAD_NOT_FOUND")
                    .with_hint("Use `ocb conversation list` to see available threads"),
            );
        }
        Err(e) => {
            // I/O or JSON error — still a local-only failure, not a network error.
            return Err(CmdError::user(e.to_string()).with_code("IO_ERROR"));
        }
    };

    let total = all_messages.len();
    // last == 0 means "all"; otherwise show the tail.
    let showing_messages = if last == 0 || last >= total {
        &all_messages[..]
    } else {
        &all_messages[total - last..]
    };

    let msgs: Vec<Value> = showing_messages
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "role": m.role,
                "content": m.content,
                "timestamp": m.timestamp,
                "run_id": m.run_id,
            })
        })
        .collect();
    Ok(json!({
        "thread_id": thread_id,
        "total_messages": total,
        "showing": showing_messages.len(),
        "messages": msgs,
    }))
}

// ---------------------------------------------------------------------------
// conversation list
// ---------------------------------------------------------------------------

fn cmd_conversation_list(out: OutputConfig) -> Result<Value, CmdError> {
    // Auto-archive stale threads before listing.
    // Errors are intentionally ignored — this is a best-effort maintenance op
    // and must never cause the list command to fail with a network error.
    let _ = conversation::auto_archive();

    // Offline command: if the conversation directory does not exist yet (or
    // the config dir is unavailable), return an empty list rather than an error.
    let threads = conversation::list_threads().unwrap_or_default();
    let entries: Vec<Value> = threads
        .iter()
        .map(|t| {
            if out.full {
                json!({
                    "id": t.id,
                    "agent_id": t.agent_id,
                    "created_at": t.created_at,
                    "updated_at": t.updated_at,
                    "message_count": t.message_count,
                    "archived": t.archived,
                })
            } else {
                // Summary: just the fields needed for routing and identification.
                json!({
                    "id": t.id,
                    "agent_id": t.agent_id,
                    "message_count": t.message_count,
                })
            }
        })
        .collect();
    Ok(json!({ "threads": entries }))
}

// ---------------------------------------------------------------------------
// send (shorthand for conversation send with prefix-matched thread ID)
// ---------------------------------------------------------------------------

/// Resolve a thread ID or prefix to a full thread ID, then delegate to
/// [`cmd_conversation_send`].
///
/// Prefix matching: if `thread_ref` is already an exact UUID match it is used
/// directly.  Otherwise the first active thread whose ID starts with
/// `thread_ref` is selected.  Errors when no match is found.
fn cmd_send(thread_ref: &str, message: &str, timeout_secs: u64, out: OutputConfig) -> Result<Value, CmdError> {
    // Try exact match first (avoids listing threads for the common case where
    // the full UUID is provided).
    let threads =
        conversation::list_threads().map_err(|e| CmdError::user(e.to_string()))?;

    let resolved_id = if let Some(exact) = threads.iter().find(|t| t.id == thread_ref) {
        exact.id.clone()
    } else {
        // Prefix match — find_thread_by_prefix re-reads the index but that is
        // acceptable: this path is only taken when the user supplies a prefix.
        match find_thread_by_prefix(thread_ref).map_err(|e| CmdError::user(e.to_string()))? {
            Some(entry) => entry.id.clone(),
            None => {
                return Err(
                    CmdError::user(format!(
                        "no thread found matching prefix '{}' — use `ocb conversation list` to see available threads",
                        thread_ref
                    ))
                    .with_code("THREAD_NOT_FOUND")
                    .with_hint("Use `ocb conversation list` to see available thread IDs"),
                );
            }
        }
    };

    cmd_conversation_send(Some(&resolved_id), None, message, timeout_secs, out)
}

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

fn cmd_watch(
    thread_id: &str,
    session_id: &str,
    debounce: u64,
    context: usize,
    timeout: u64,
) -> Result<Value, CmdError> {
    // Persist the session ID on the thread entry (best-effort; non-fatal).
    let _ = conversation::set_thread_session_id(thread_id, session_id);

    // The watcher loop is async and runs until interrupted.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::user(format!("failed to create runtime: {e}")))?;
    rt.block_on(watch::run_watch(thread_id, session_id, debounce, context, timeout))
        .map_err(|e| CmdError::network(e.to_string()))?;

    Ok(json!({"status": "watch_exited"}))
}

// ---------------------------------------------------------------------------
// pair
// ---------------------------------------------------------------------------

fn cmd_pair(gateway: Option<&str>) -> Result<Value, CmdError> {
    // Load or generate device identity so we always have a device_id to report.
    let identity = auth::load_device_identity()
        .map_err(|e| CmdError::user(format!("failed to load device identity: {e}")))?;
    let device_id = &identity.device_id;

    // Determine host/port from env, CLI override, or defaults.
    let (host, port) = if let Some(gw) = gateway {
        // Parse "host:port" or "host"
        if let Some((h, p)) = gw.rsplit_once(':') {
            let port: u16 = p
                .parse()
                .map_err(|_| CmdError::user(format!("invalid port in gateway URL: {p}")))?;
            (h.to_string(), port)
        } else {
            (gw.to_string(), ws_port())
        }
    } else {
        (ws_host(), ws_port())
    };

    let token = load_gateway_token()?;

    // Attempt connection — if it succeeds we're already paired.
    let connect_result = block_on_async(async {
        let mut client = WsClient::connect(&host, port, &token).await?;
        client.disconnect().await?;
        Ok(())
    });

    match connect_result {
        Ok(()) => Ok(json!({
            "status": "paired",
            "device_id": device_id,
            "gateway": format!("{}:{}", host, port),
            "message": "Device is already authenticated with the gateway.",
        })),
        Err(e) => {
            let msg = e.to_string();
            let lower = msg.to_lowercase();

            if lower.contains("not_paired")
                || lower.contains("not paired")
                || lower.contains("pending")
                || lower.contains("unauthorized")
            {
                // Device needs approval — print instructions.
                Ok(json!({
                    "status": "pending",
                    "device_id": device_id,
                    "gateway": format!("{}:{}", host, port),
                    "message": format!(
                        "Device is not yet approved. Approve device_id={device_id} on the OpenClaw gateway, then re-run `ocb pair` to confirm."
                    ),
                }))
            } else {
                // Genuine connection failure.
                Err(CmdError::network(format!(
                    "gateway connection failed: {msg}"
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// auth status
// ---------------------------------------------------------------------------

fn cmd_auth_status() -> Result<Value, CmdError> {
    let config_dir = openclaw_bridge::config_dir()
        .map_err(|e| CmdError::user(format!("cannot determine config dir: {e}")))?;
    let identity_path = config_dir.join("openclaw-bridge").join("openclaw-device.json");
    let token_path = config_dir.join("openclaw-bridge").join("openclaw-device-auth.json");

    let identity_exists = identity_path.exists();
    let token_exists = token_path.exists();

    let (device_id, identity_status) = if identity_exists {
        match auth::load_device_identity() {
            Ok(id) => (
                Some(id.device_id.clone()),
                "loaded",
            ),
            Err(_) => (None, "corrupted"),
        }
    } else {
        (None, "not_found")
    };

    let token_status = if token_exists {
        match auth::load_device_token() {
            Ok(Some(_)) => "present",
            Ok(None) => "empty",
            Err(_) => "corrupted",
        }
    } else {
        "not_found"
    };

    Ok(json!({
        "device_id": device_id,
        "identity_file": identity_path.to_string_lossy(),
        "identity_status": identity_status,
        "token_file": token_path.to_string_lossy(),
        "token_status": token_status,
    }))
}

// ---------------------------------------------------------------------------
// auth reset
// ---------------------------------------------------------------------------

fn cmd_auth_reset() -> Result<Value, CmdError> {
    let config_dir = openclaw_bridge::config_dir()
        .map_err(|e| CmdError::user(format!("cannot determine config dir: {e}")))?;
    let identity_path = config_dir.join("openclaw-bridge").join("openclaw-device.json");
    let token_path = config_dir.join("openclaw-bridge").join("openclaw-device-auth.json");

    let mut removed = Vec::new();

    if identity_path.exists() {
        std::fs::remove_file(&identity_path).map_err(|e| {
            CmdError::user(format!(
                "failed to remove identity file {}: {e}",
                identity_path.display()
            ))
        })?;
        removed.push(identity_path.to_string_lossy().into_owned());
    }

    if token_path.exists() {
        std::fs::remove_file(&token_path).map_err(|e| {
            CmdError::user(format!(
                "failed to remove token file {}: {e}",
                token_path.display()
            ))
        })?;
        removed.push(token_path.to_string_lossy().into_owned());
    }

    Ok(json!({
        "status": "reset",
        "removed": removed,
        "message": "Device identity and tokens cleared. Run `ocb pair` to re-authenticate.",
    }))
}

// ---------------------------------------------------------------------------
// tui
// ---------------------------------------------------------------------------

#[cfg(feature = "tui")]
fn cmd_tui(thread: Option<String>) -> Result<Value, CmdError> {
    // The TUI needs a tokio runtime. Use current_thread so that the async
    // future (and the crossterm terminal setup it calls) runs on the main OS
    // thread. A multi-thread runtime moves work off the calling thread, which
    // causes crossterm's enable_raw_mode() to fail with ENXIO (os error 6)
    // because the worker threads don't inherit the controlling TTY.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CmdError::network(format!("failed to create runtime: {e}")))?;
    rt.block_on(openclaw_bridge::tui::run_tui(thread))
        .map_err(|e| CmdError::network(e.to_string()))?;
    Ok(serde_json::json!({"status": "tui_exited"}))
}

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

fn cmd_version() -> Result<Value, CmdError> {
    Ok(json!({
        "name": "ocb",
        "version": env!("CARGO_PKG_VERSION"),
        "description": env!("CARGO_PKG_DESCRIPTION"),
    }))
}
