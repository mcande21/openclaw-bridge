//! MCP (Model Context Protocol) JSON-RPC 2.0 server over stdio with WebSocket bridge.
//!
//! This module implements the Phase 2 channel server:
//! - MCP handshake (`initialize` + `initialized`)
//! - `tools/list` — advertises `channel_history` and `reply`
//! - `tools/call` — dispatches to tool handlers
//! - `ping` — responds with `{}`
//! - Unknown methods — responds with JSON-RPC method-not-found error
//! - WebSocket bridge — streams OpenClaw agent events back to Claude Code
//!
//! ## Architecture
//!
//! After the MCP handshake, the server connects to the OpenClaw gateway over
//! WebSocket, splits the connection into read/write halves, and enters a
//! `tokio::select!` loop that drives both MCP stdin and WS reads concurrently.
//!
//! The `reply` tool is asynchronous: when invoked, the server sends an `agent`
//! request over WS and defers the MCP response until the agent completes. Only
//! one pending reply is allowed at a time.
//!
//! Unsolicited WS events (messages from Aria not triggered by a `reply` call)
//! are emitted as `notifications/claude/channel` to Claude Code and persisted
//! to JSONL.
//!
//! ## Protocol
//!
//! Line-delimited JSON-RPC 2.0 over stdin/stdout. Each JSON object is one
//! line, `\n` terminated. stderr is for diagnostics only.

pub mod tools;
pub mod transport;

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tokio::sync::mpsc as tokio_mpsc;

use crate::conversation::{self, ConversationError, MessageRole};
use crate::protocol::{AgentEventPayload, AgentParams, WsFrame, new_request_id};
use crate::ws::{WsReadHalf, WsWriteHalf, next_frame_read_half, send_frame_write_half};

use tools::ToolCallResult;
use transport::StdioTransport;

// ---------------------------------------------------------------------------
// Pending reply state
// ---------------------------------------------------------------------------

/// Tracks an in-flight `reply` tool call waiting for the WS agent response.
struct PendingReply {
    /// JSON-RPC request ID to respond to once the agent finishes.
    mcp_request_id: Value,
    /// UUID of the WS `agent` Req we sent.
    ws_request_id: String,
    /// run_id assigned by the gateway (set when first Res/Event arrives).
    run_id: Option<String>,
    /// Streaming text deltas keyed by seq for ordered assembly.
    deltas: BTreeMap<u64, String>,
    /// Final authoritative text from lifecycle end or completion Res.
    final_text: Option<String>,
    /// Whether the initial accepted Res has been received.
    accepted: bool,
}

impl PendingReply {
    fn new(mcp_request_id: Value, ws_request_id: String) -> Self {
        Self {
            mcp_request_id,
            ws_request_id,
            run_id: None,
            deltas: BTreeMap::new(),
            final_text: None,
            accepted: false,
        }
    }

    /// Assemble the final response text from deltas or authoritative text.
    fn assemble_text(&self) -> String {
        if let Some(ref t) = self.final_text
            && !t.is_empty()
        {
            return t.clone();
        }
        self.deltas.values().cloned().collect::<Vec<_>>().join("")
    }
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

/// Run the MCP server until stdin is closed or the WS connection drops.
///
/// Called from `main.rs` dispatch. Blocks until the client disconnects.
pub async fn run_mcp_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut transport = StdioTransport::new();

    // -----------------------------------------------------------------------
    // Step 1: Wait for `initialize` request.
    // -----------------------------------------------------------------------
    let init_req = wait_for_message(&mut transport).await?;
    let init_id = init_req.get("id").cloned().unwrap_or(Value::Null);
    let method = init_req
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if method != "initialize" {
        let err = json_rpc_error(
            &init_id,
            -32600,
            "Expected initialize as first message",
            None,
        );
        transport.write_message(&err).await?;
        return Err("MCP handshake failed: first message was not initialize".into());
    }

    // -----------------------------------------------------------------------
    // Step 2: Respond with server capabilities.
    // -----------------------------------------------------------------------
    let init_response = json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "result": {
            "protocolVersion": "2025-03-26",
            "capabilities": {
                "experimental": { "claude/channel": {} },
                "tools": {}
            },
            "serverInfo": {
                "name": "ocb",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    transport.write_message(&init_response).await?;

    // -----------------------------------------------------------------------
    // Step 3: Read `initialized` notification (no response needed).
    // -----------------------------------------------------------------------
    let initialized = wait_for_message(&mut transport).await?;
    let notif_method = initialized
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if notif_method != "initialized" {
        eprintln!(
            "[mcp] expected 'initialized' notification, got '{notif_method}' — proceeding anyway"
        );
    }

    // -----------------------------------------------------------------------
    // Step 4: Resolve thread ID and session key.
    // -----------------------------------------------------------------------
    let thread_id = resolve_thread_id()?;
    // The session key is always "ocb:<thread-id>" — same format create_thread uses.
    let session_key = format!("ocb:{thread_id}");
    eprintln!("[mcp] using thread: {thread_id}");

    // -----------------------------------------------------------------------
    // Step 5: Connect to OpenClaw gateway WebSocket.
    // -----------------------------------------------------------------------
    let agent_id = std::env::var("OCB_MCP_AGENT").unwrap_or_else(|_| "main".to_string());
    let agent_id = agent_id.trim().to_string();

    let ws_host = crate::resolve_ws_host();
    let ws_port = crate::WS_PORT;
    let gateway_token = match crate::load_gateway_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[mcp] WS not available: {e}");
            // Fall back to stub mode — no WS, reply tool will return an error.
            return run_without_ws(&mut transport, &thread_id).await;
        }
    };

    let client = match crate::ws::WsClient::connect(&ws_host, ws_port, &gateway_token).await {
        Ok(c) => {
            eprintln!("[mcp] connected to gateway at {ws_host}:{ws_port}");
            c
        }
        Err(e) => {
            eprintln!("[mcp] WS connect failed: {e}");
            return run_without_ws(&mut transport, &thread_id).await;
        }
    };

    let (mut ws_read, mut ws_write) = client.split();

    // -----------------------------------------------------------------------
    // Step 6: Main select loop.
    // -----------------------------------------------------------------------
    run_select_loop(
        &mut transport,
        &mut ws_read,
        &mut ws_write,
        &thread_id,
        &session_key,
        &agent_id,
    )
    .await
}

// ---------------------------------------------------------------------------
// Select loop
// ---------------------------------------------------------------------------

/// Drive the MCP server with a live WS connection.
async fn run_select_loop(
    transport: &mut StdioTransport,
    ws_read: &mut WsReadHalf,
    ws_write: &mut WsWriteHalf,
    thread_id: &str,
    session_key: &str,
    agent_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pending: Option<PendingReply> = None;

    // -----------------------------------------------------------------------
    // File watcher: watch the JSONL file for Cooper's TUI messages.
    //
    // notify callbacks run on a background OS thread, so we bridge to a
    // tokio mpsc channel that the select! loop can await on.
    // The watcher must stay alive for the duration of the loop.
    // -----------------------------------------------------------------------
    let (file_watcher_tx, mut file_watcher_rx) = tokio_mpsc::channel::<()>(32);
    let _watcher: Option<Box<dyn notify::Watcher>> =
        build_mcp_file_watcher(thread_id, file_watcher_tx);

    // Seed last_seen_count from the current JSONL length so we don't replay
    // history on startup.
    let mut last_seen_count: usize = conversation::read_thread(thread_id)
        .map(|msgs| msgs.len())
        .unwrap_or(0);

    loop {
        tokio::select! {
            // ------------------------------------------------------------------
            // Branch 1: MCP request from Claude Code (stdin)
            // ------------------------------------------------------------------
            mcp_result = transport.read_message() => {
                let msg = match mcp_result? {
                    Some(v) => v,
                    None => {
                        eprintln!("[mcp] stdin closed, shutting down");
                        break;
                    }
                };

                if msg.is_null() {
                    continue;
                }

                let req_id = msg.get("id").cloned();
                let method = msg
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let is_notification = req_id.is_none();

                let response: Option<Value> = match method.as_str() {
                    "tools/list" => {
                        if is_notification {
                            None
                        } else {
                            Some(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "tools": tools::tool_schemas()
                                }
                            }))
                        }
                    }

                    "tools/call" => {
                        if is_notification {
                            None
                        } else {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            let tool_name = params
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tool_input = params
                                .get("arguments")
                                .cloned()
                                .unwrap_or(json!({}));

                            dispatch_tool_ws(
                                &tool_name,
                                &tool_input,
                                thread_id,
                                req_id.as_ref().unwrap_or(&Value::Null),
                                &mut pending,
                                ws_write,
                                session_key,
                                agent_id,
                                &mut last_seen_count,
                            )
                            .await
                        }
                    }

                    "ping" => {
                        if is_notification {
                            None
                        } else {
                            Some(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {}
                            }))
                        }
                    }

                    other => {
                        if is_notification {
                            eprintln!("[mcp] unknown notification: {other}");
                            None
                        } else {
                            Some(json_rpc_error(
                                &req_id.unwrap_or(Value::Null),
                                -32601,
                                &format!("Method not found: {other}"),
                                None,
                            ))
                        }
                    }
                };

                if let Some(resp) = response {
                    transport.write_message(&resp).await?;
                }
            }

            // ------------------------------------------------------------------
            // Branch 2: WebSocket frame from OpenClaw
            // ------------------------------------------------------------------
            ws_result = next_frame_read_half(ws_read) => {
                match ws_result {
                    Ok(frame) => {
                        if let Some(resp) = handle_ws_frame(
                            frame,
                            &mut pending,
                            thread_id,
                            &mut last_seen_count,
                        ).await {
                            transport.write_message(&resp).await?;
                        }
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("WebSocket closed") || msg.contains("stream ended") {
                            eprintln!("[mcp] WS connection closed: {msg}");
                        } else {
                            eprintln!("[mcp] WS error: {msg}");
                        }
                        // If we had a pending reply, send an error to Claude Code.
                        if let Some(p) = pending.take() {
                            let err = json_rpc_error(
                                &p.mcp_request_id,
                                -32603,
                                &format!("WebSocket disconnected: {msg}"),
                                None,
                            );
                            transport.write_message(&err).await?;
                        }
                        break;
                    }
                }
            }

            // ------------------------------------------------------------------
            // Branch 3: JSONL file change — Cooper's TUI messages
            // ------------------------------------------------------------------
            Some(()) = file_watcher_rx.recv() => {
                // Drain any additional pending events (FSEvents double-fire on macOS).
                while file_watcher_rx.try_recv().is_ok() {}

                if let Some(notifications) =
                    poll_tui_messages(thread_id, &mut last_seen_count)
                {
                    for notification in notifications {
                        transport.write_message(&notification).await?;
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Fallback: run without WS (token missing or connect failed)
// ---------------------------------------------------------------------------

/// Run the MCP loop without a WebSocket connection.
///
/// `channel_history` still works. `reply` returns an error explaining why
/// the WS is unavailable. This preserves MCP usability for read-only access
/// when the gateway is down or unconfigured.
///
/// The file watcher is active even in this mode so Cooper's TUI messages are
/// still forwarded to Claude Code as `notifications/claude/channel`.
async fn run_without_ws(
    transport: &mut StdioTransport,
    thread_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!("[mcp] running in read-only mode (no WS)");

    let (file_watcher_tx, mut file_watcher_rx) = tokio_mpsc::channel::<()>(32);
    let _watcher: Option<Box<dyn notify::Watcher>> =
        build_mcp_file_watcher(thread_id, file_watcher_tx);

    let mut last_seen_count: usize = conversation::read_thread(thread_id)
        .map(|msgs| msgs.len())
        .unwrap_or(0);

    loop {
        tokio::select! {
            // ------------------------------------------------------------------
            // Branch 1: MCP request from Claude Code (stdin)
            // ------------------------------------------------------------------
            mcp_result = transport.read_message() => {
                let msg = match mcp_result? {
                    Some(v) => v,
                    None => {
                        eprintln!("[mcp] stdin closed, shutting down");
                        break;
                    }
                };

                if msg.is_null() {
                    continue;
                }

                let req_id = msg.get("id").cloned();
                let method = msg
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let is_notification = req_id.is_none();

                let response: Option<Value> = match method.as_str() {
                    "tools/list" => {
                        if is_notification {
                            None
                        } else {
                            Some(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "tools": tools::tool_schemas()
                                }
                            }))
                        }
                    }

                    "tools/call" => {
                        if is_notification {
                            None
                        } else {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            let tool_name = params
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tool_input = params
                                .get("arguments")
                                .cloned()
                                .unwrap_or(json!({}));
                            let id = req_id.as_ref().unwrap_or(&Value::Null).clone();

                            let result = match tool_name.as_str() {
                                "channel_history" => {
                                    tools::handle_channel_history(&tool_input, thread_id)
                                }
                                "channel_status" => {
                                    let agent_id = std::env::var("OCB_MCP_AGENT")
                                        .unwrap_or_else(|_| "main".to_string());
                                    tools::handle_channel_status(thread_id, agent_id.trim(), false)
                                }
                                "reply" => {
                                    json!({
                                        "isError": true,
                                        "content": [{
                                            "type": "text",
                                            "text": "WebSocket not connected. Set OPENCLAW_TOKEN and ensure the gateway is reachable."
                                        }]
                                    })
                                }
                                other => {
                                    eprintln!("[mcp] unknown tool: {other}");
                                    json!({
                                        "isError": true,
                                        "content": [{"type": "text", "text": format!("Unknown tool: {other}")}]
                                    })
                                }
                            };

                            Some(json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result
                            }))
                        }
                    }

                    "ping" => {
                        if is_notification {
                            None
                        } else {
                            Some(json!({ "jsonrpc": "2.0", "id": req_id, "result": {} }))
                        }
                    }

                    other => {
                        if is_notification {
                            eprintln!("[mcp] unknown notification: {other}");
                            None
                        } else {
                            Some(json_rpc_error(
                                &req_id.unwrap_or(Value::Null),
                                -32601,
                                &format!("Method not found: {other}"),
                                None,
                            ))
                        }
                    }
                };

                if let Some(resp) = response {
                    transport.write_message(&resp).await?;
                }
            }

            // ------------------------------------------------------------------
            // Branch 2: JSONL file change — Cooper's TUI messages
            // ------------------------------------------------------------------
            Some(()) = file_watcher_rx.recv() => {
                // Drain any additional pending events (FSEvents double-fire on macOS).
                while file_watcher_rx.try_recv().is_ok() {}

                if let Some(notifications) =
                    poll_tui_messages(thread_id, &mut last_seen_count)
                {
                    for notification in notifications {
                        transport.write_message(&notification).await?;
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tool dispatch (with WS)
// ---------------------------------------------------------------------------

/// Dispatch a `tools/call` request, potentially initiating a pending WS reply.
///
/// Returns `Some(response)` for immediate responses (including errors).
/// Returns `None` when a `PendingReply` was created — the MCP response will
/// be written later by [`handle_ws_frame`] when the agent finishes.
#[allow(clippy::too_many_arguments)]
async fn dispatch_tool_ws(
    tool_name: &str,
    tool_input: &Value,
    thread_id: &str,
    mcp_req_id: &Value,
    pending: &mut Option<PendingReply>,
    ws_write: &mut WsWriteHalf,
    session_key: &str,
    agent_id: &str,
    last_seen_count: &mut usize,
) -> Option<Value> {
    match tool_name {
        "channel_history" => {
            let result = tools::handle_channel_history(tool_input, thread_id);
            Some(json!({
                "jsonrpc": "2.0",
                "id": mcp_req_id,
                "result": result
            }))
        }

        "channel_status" => {
            let result = tools::handle_channel_status(thread_id, agent_id, true);
            Some(json!({
                "jsonrpc": "2.0",
                "id": mcp_req_id,
                "result": result
            }))
        }

        "reply" => {
            // Reject if another reply is already in flight.
            if pending.is_some() {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": mcp_req_id,
                    "result": {
                        "isError": true,
                        "content": [{
                            "type": "text",
                            "text": "A reply is already in progress. Wait for the current response before sending another."
                        }]
                    }
                }));
            }

            let result = tools::handle_reply(tool_input);
            match result {
                ToolCallResult::Immediate(v) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": mcp_req_id,
                    "result": v
                })),
                ToolCallResult::PendingReply { message } => {
                    // Persist outbound message to JSONL.
                    match conversation::append_message(
                        thread_id,
                        MessageRole::User,
                        &message,
                        None,
                        Some("mcp"),
                    ) {
                        Ok(_) => {
                            // Advance last_seen_count so the file watcher does not
                            // echo our own outbound message back as a TUI notification.
                            *last_seen_count += 1;
                        }
                        Err(e) => {
                            eprintln!("[mcp] failed to persist outbound message: {e}");
                        }
                    }

                    // Send WS agent request.
                    let ws_req_id = new_request_id();
                    let frame = WsFrame::agent_request(
                        &ws_req_id,
                        AgentParams {
                            message: message.clone(),
                            agent_id: Some(agent_id.to_string()),
                            idempotency_key: new_request_id(),
                            session_key: Some(session_key.to_string()),
                            thinking: None,
                            timeout: None,
                        },
                    );

                    match send_frame_write_half(ws_write, &frame).await {
                        Ok(()) => {
                            eprintln!("[mcp] sent agent request {ws_req_id}");
                            *pending = Some(PendingReply::new(mcp_req_id.clone(), ws_req_id));
                            // Return None — response will arrive via WS branch.
                            None
                        }
                        Err(e) => {
                            eprintln!("[mcp] WS send failed: {e}");
                            Some(json!({
                                "jsonrpc": "2.0",
                                "id": mcp_req_id,
                                "result": {
                                    "isError": true,
                                    "content": [{
                                        "type": "text",
                                        "text": format!("Failed to send message to gateway: {e}")
                                    }]
                                }
                            }))
                        }
                    }
                }
            }
        }

        other => {
            eprintln!("[mcp] unknown tool: {other}");
            Some(json!({
                "jsonrpc": "2.0",
                "id": mcp_req_id,
                "result": {
                    "isError": true,
                    "content": [{"type": "text", "text": format!("Unknown tool: {other}")}]
                }
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// WS frame handler
// ---------------------------------------------------------------------------

/// Process an incoming WS frame.
///
/// Returns `Some(value)` when something should be written to stdout (either
/// completing a pending reply or emitting an unsolicited channel notification).
/// Returns `None` for frames that require no MCP output.
async fn handle_ws_frame(
    frame: WsFrame,
    pending: &mut Option<PendingReply>,
    thread_id: &str,
    last_seen_count: &mut usize,
) -> Option<Value> {
    match frame {
        // ------------------------------------------------------------------
        // Agent streaming event
        // ------------------------------------------------------------------
        WsFrame::Event { event, payload, .. } if event == "agent" => {
            let agent_event = match serde_json::from_value::<AgentEventPayload>(payload) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("[mcp/ws] failed to parse agent event: {err}");
                    return None;
                }
            };

            // Check if this event belongs to our pending reply.
            //
            // We only correlate by run_id — never adopt blindly. The gateway
            // sets run_id in the accepted Res (handled in the Res branch below).
            // Until we receive that Res, p.run_id is None and any incoming Event
            // is unsolicited (e.g. Aria responding to Cooper's TUI message that
            // arrived on the shared WS connection). Adopting the first Event's
            // run_id would corrupt pending state and leave it stuck.
            if let Some(ref mut p) = *pending {
                let matches = match &p.run_id {
                    Some(rid) => *rid == agent_event.run_id,
                    None => false, // run_id not yet confirmed via accepted Res — treat as unsolicited
                };

                if matches {
                    let stream = agent_event.stream.as_deref().unwrap_or("");
                    match stream {
                        "assistant" => {
                            if let Some(delta) =
                                agent_event.data.get("delta").and_then(Value::as_str)
                            {
                                p.deltas.insert(agent_event.seq, delta.to_string());
                            }
                            if let Some(text) =
                                agent_event.data.get("text").and_then(Value::as_str)
                                && !text.is_empty()
                            {
                                p.final_text = Some(text.to_string());
                            }
                        }
                        "lifecycle" => {
                            let phase = agent_event
                                .data
                                .get("phase")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if phase == "end" {
                                // Agent turn complete — assemble and send MCP response.
                                if let Some(final_text) =
                                    agent_event.data.get("text").and_then(Value::as_str)
                                    && !final_text.is_empty()
                                {
                                    p.final_text = Some(final_text.to_string());
                                }
                                return complete_pending_reply(pending, thread_id, last_seen_count).await;
                            }
                        }
                        _ => {}
                    }
                    return None;
                }
                // Doesn't match our run_id — fall through to unsolicited handling.
            }

            // Unsolicited agent event (not from a pending reply).
            emit_channel_notification(&agent_event, thread_id).await
        }

        // ------------------------------------------------------------------
        // Agent request accepted (initial Res with runId)
        // ------------------------------------------------------------------
        WsFrame::Res {
            ref id,
            ok,
            ref payload,
            ref error,
        } => {
            if let Some(ref mut p) = *pending {
                if *id == p.ws_request_id && !p.accepted {
                    if !ok {
                        // Gateway rejected the request.
                        let err_msg = error
                            .as_ref()
                            .map(|e| format!("{}: {}", e.code, e.message))
                            .unwrap_or_else(|| "unknown error".to_string());
                        eprintln!("[mcp/ws] agent request rejected: {err_msg}");
                        let p = pending.take().unwrap();
                        return Some(json!({
                            "jsonrpc": "2.0",
                            "id": p.mcp_request_id,
                            "result": {
                                "isError": true,
                                "content": [{"type": "text", "text": format!("Gateway error: {err_msg}")}]
                            }
                        }));
                    }

                    // Extract runId from accepted response.
                    if let Some(ref p_val) = *payload {
                        if let Some(run_id) = p_val.get("runId").and_then(Value::as_str) {
                            p.run_id = Some(run_id.to_string());
                        }
                        // Some gateways include the final text in the accepted Res.
                        if let Some(text) = p_val.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            p.final_text = Some(text.to_string());
                        }
                    }
                    p.accepted = true;

                    // If text arrived with accepted Res, complete immediately.
                    if p.final_text.is_some() {
                        return complete_pending_reply(pending, thread_id, last_seen_count).await;
                    }
                    return None;
                }

                // Completion Res (second Res for same request, after streaming).
                if *id == p.ws_request_id && p.accepted {
                    if ok
                        && let Some(ref p_val) = *payload
                        && let Some(text) = p_val.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        p.final_text = Some(text.to_string());
                    }
                    // Completion Res arrived — but we wait for lifecycle end.
                    // If lifecycle end never fires, we'll complete here anyway.
                    if p.final_text.is_some() {
                        return complete_pending_reply(pending, thread_id, last_seen_count).await;
                    }
                    return None;
                }
            }

            // Stale or unrelated Res — ignore.
            None
        }

        // Keepalive ticks — silent.
        WsFrame::Event { event, .. } if event == "tick" => None,

        // Other events — silent.
        WsFrame::Event { .. } | WsFrame::Req { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Helper: complete a pending reply
// ---------------------------------------------------------------------------

/// Assemble the final MCP response for a completed pending reply, persist
/// the assistant message to JSONL, clear the pending state, and return the
/// JSON-RPC response value.
async fn complete_pending_reply(
    pending: &mut Option<PendingReply>,
    thread_id: &str,
    last_seen_count: &mut usize,
) -> Option<Value> {
    let p = pending.take()?;
    let text = p.assemble_text();
    let run_id = p.run_id.as_deref();

    eprintln!("[mcp] reply complete (run_id={:?})", run_id);

    // Persist assistant response to JSONL.
    match conversation::append_message(
        thread_id,
        MessageRole::Assistant,
        &text,
        run_id,
        None,
    ) {
        Ok(_) => {
            // Advance last_seen_count so the file watcher does not re-emit
            // our own assistant response as a TUI notification.
            *last_seen_count += 1;
        }
        Err(e) => {
            eprintln!("[mcp] failed to persist assistant response: {e}");
        }
    }

    Some(json!({
        "jsonrpc": "2.0",
        "id": p.mcp_request_id,
        "result": {
            "content": [{"type": "text", "text": text}]
        }
    }))
}

// ---------------------------------------------------------------------------
// Helper: emit unsolicited channel notification
// ---------------------------------------------------------------------------

/// Emit a `notifications/claude/channel` notification for an unsolicited WS
/// event and persist it to JSONL.
async fn emit_channel_notification(
    event: &AgentEventPayload,
    thread_id: &str,
) -> Option<Value> {
    let stream = event.stream.as_deref().unwrap_or("");

    // Only forward assistant text content.
    let text = match stream {
        "assistant" => event.data.get("delta").and_then(Value::as_str)?,
        _ => return None,
    };

    // Persist unsolicited message.
    if let Err(e) = conversation::append_message(
        thread_id,
        MessageRole::Assistant,
        text,
        Some(&event.run_id),
        Some("ws"),
    ) {
        eprintln!("[mcp] failed to persist unsolicited message: {e}");
    }

    let ts = chrono::Utc::now().to_rfc3339();
    Some(json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": text,
            "meta": {
                "user": "aria",
                "agent": "main",
                "run_id": event.run_id,
                "ts": ts
            }
        }
    }))
}

// ---------------------------------------------------------------------------
// File watcher helpers
// ---------------------------------------------------------------------------

/// Create a [`notify::RecommendedWatcher`] watching the thread JSONL file.
///
/// Sends `()` on `tx` whenever the file changes. Returns `None` on any
/// failure — the caller falls back gracefully (no file watcher branch fires).
///
/// The returned `Box<dyn notify::Watcher>` must be kept alive for the
/// duration of the select loop; dropping it stops the watch.
fn build_mcp_file_watcher(
    thread_id: &str,
    tx: tokio_mpsc::Sender<()>,
) -> Option<Box<dyn notify::Watcher>> {
    use notify::Watcher as _;

    let jsonl_path = match conversation::thread_file_path(thread_id) {
        Ok(Some(p)) => p,
        _ => return None,
    };

    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                // UnboundedSender::send is sync; channel-full means the
                // consumer is behind — that is fine, we just skip the tick.
                let _ = tx.try_send(());
            }
        })
        .ok()?;

    watcher
        .watch(&jsonl_path, notify::RecursiveMode::NonRecursive)
        .ok()?;

    Some(Box::new(watcher))
}

/// Read new messages from the thread JSONL and return a notification for each
/// `source: "tui"` message that arrived since `last_seen_count`.
///
/// Advances `last_seen_count` to the current message count on return.
/// Returns `None` when there is nothing to emit (no new messages, or new
/// messages are not from the TUI).
fn poll_tui_messages(
    thread_id: &str,
    last_seen_count: &mut usize,
) -> Option<Vec<Value>> {
    let messages = conversation::read_thread(thread_id).ok()?;
    if messages.len() <= *last_seen_count {
        return None;
    }

    let mut notifications = Vec::new();
    for msg in &messages[*last_seen_count..] {
        if msg.source.as_deref() == Some("tui") {
            let ts = msg.timestamp.to_rfc3339();
            notifications.push(json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": {
                    "content": msg.content,
                    "meta": {
                        "user": "cooper",
                        "source": "tui",
                        "ts": ts
                    }
                }
            }));
        }
    }

    *last_seen_count = messages.len();

    if notifications.is_empty() { None } else { Some(notifications) }
}

// ---------------------------------------------------------------------------
// Thread resolution
// ---------------------------------------------------------------------------

/// Resolve the thread ID the MCP server will use for this session.
///
/// Resolution order:
/// 1. `OCB_MCP_THREAD` env var — use as-is (explicit override).
/// 2. Always create a fresh thread for the agent named by `OCB_MCP_AGENT`
///    (default `"main"`). Each MCP server session gets its own thread so that
///    Claude Code sessions remain isolated from TUI and Discord conversations.
fn resolve_thread_id() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // 1. Explicit override.
    if let Ok(tid) = std::env::var("OCB_MCP_THREAD") {
        let tid = tid.trim().to_string();
        if !tid.is_empty() {
            return Ok(tid);
        }
    }

    // 2. Always create a new thread for this session.
    let agent_id = std::env::var("OCB_MCP_AGENT").unwrap_or_else(|_| "main".to_string());
    let agent_id = agent_id.trim().to_string();

    let thread = conversation::create_thread(&agent_id).map_err(|e: ConversationError| {
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;
    eprintln!("[mcp] created session thread: {}", thread.id);
    Ok(thread.id)
}

// ---------------------------------------------------------------------------
// JSON-RPC helpers
// ---------------------------------------------------------------------------

/// Build a JSON-RPC 2.0 error response.
fn json_rpc_error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message
    });
    if let Some(d) = data {
        error["data"] = d;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error
    })
}

/// Read messages until we get a non-null one.
///
/// Skips blank lines (returned as `Value::Null` by the transport).
async fn wait_for_message(
    transport: &mut StdioTransport,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        match transport.read_message().await? {
            None => {
                return Err("stdin closed before handshake completed".into());
            }
            Some(v) if v.is_null() => {
                continue;
            }
            Some(v) => return Ok(v),
        }
    }
}
