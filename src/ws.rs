//! Async WebSocket client for the OpenClaw gateway.
//!
//! Handles the full connection lifecycle:
//! 1. TCP + WebSocket upgrade to `ws://<host>:<port>/ws`
//! 2. Challenge/response authentication (Ed25519 device signature)
//! 3. RPC request/response dispatch
//! 4. Agent streaming (text_delta collection)
//!
//! # Authentication flow
//!
//! ```text
//! client                        gateway
//!   |                              |
//!   |---- connect WS ------------->|
//!   |<--- Event{connect.challenge} |  (nonce)
//!   |---- Req{connect, auth}------>|  (signed payload + device attestation)
//!   |<--- Res{ok, hello-ok}--------|
//!   |        (authenticated)       |
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::sink::SinkExt;
use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use serde_json::Value;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::auth;
use crate::protocol::{
    AgentEventPayload, AgentParams, AuthParams, ChallengePayload, ClientInfo, ConnectParams,
    DeviceParams, WsFrame, new_request_id,
};

// ---------------------------------------------------------------------------
// Client identity constants
// ---------------------------------------------------------------------------

/// Client identifier sent in connect requests and signature payloads.
/// Gateway validates this against a known set — "gateway-client" is accepted.
const CLIENT_ID: &str = "gateway-client";

/// Client mode sent in connect requests and signature payloads.
/// Gateway validates this against a known set — "backend" is accepted.
const CLIENT_MODE: &str = "backend";

/// Client role sent in connect requests and signature payloads.
const CLIENT_ROLE: &str = "operator";

/// Client scope sent in connect requests and signature payloads.
const CLIENT_SCOPE: &str = "operator.admin";

// ---------------------------------------------------------------------------
// Timeout constants
// ---------------------------------------------------------------------------

/// Default timeout for `agent_chat` (interactive conversation).
pub const DEFAULT_CHAT_TIMEOUT: Duration = Duration::from_secs(120);

/// Extended timeout for `agent_spawn` (long-running agent tasks).
#[allow(dead_code)]
pub const DEFAULT_SPAWN_TIMEOUT: Duration = Duration::from_secs(300);

/// Timeout for the WebSocket handshake (challenge + hello-ok round-trip).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

type WsError = Box<dyn std::error::Error + Send + Sync + 'static>;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result returned by [`WsClient::agent_chat`].
pub struct AgentChatResult {
    /// The run ID assigned by the gateway for this request.
    pub run_id: String,
    /// Final authoritative response text from the `Res` payload (preferred).
    /// Falls back to deltas joined in seq order if not present.
    pub text: String,
    /// Individual streaming text chunks, in arrival seq order (for observability).
    #[allow(dead_code)]
    pub deltas: Vec<String>,
    /// Usage metadata from the final `Res` payload, if present.
    #[allow(dead_code)]
    pub usage: Option<Value>,
}

// ---------------------------------------------------------------------------
// WsClient
// ---------------------------------------------------------------------------

/// Authenticated WebSocket connection to the OpenClaw gateway.
pub struct WsClient {
    /// The host we connected to (for informational purposes).
    #[allow(dead_code)]
    pub host: String,
    /// The port we connected to.
    #[allow(dead_code)]
    pub port: u16,
    /// The gateway token used for authentication.
    #[allow(dead_code)]
    pub gateway_token: String,
    /// The underlying WebSocket stream.
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

// ---------------------------------------------------------------------------
// Split stream type aliases (used by the TUI background task)
// ---------------------------------------------------------------------------

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Read half produced by [`WsClient::split`].
pub type WsReadHalf = SplitStream<WsStream>;

/// Write half produced by [`WsClient::split`].
pub type WsWriteHalf = SplitSink<WsStream, Message>;

impl WsClient {
    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Split the authenticated WebSocket into independent read and write halves.
    ///
    /// Used by the TUI background task so that reads and writes can be driven
    /// concurrently inside a `tokio::select!` loop without requiring `&mut self`
    /// for both operations at once.
    ///
    /// Consumes `self` — the caller takes ownership of both halves.
    ///
    /// Returns `(read_half, write_half)`.
    pub fn split(self) -> (WsReadHalf, WsWriteHalf) {
        use futures_util::stream::StreamExt as _;
        let (sink, stream) = self.ws.split();
        (stream, sink)
    }

    /// Connect to the OpenClaw gateway WebSocket and authenticate.
    ///
    /// Steps:
    /// 1. Open WebSocket to `ws://<host>:<port>/ws`
    /// 2. Receive `connect.challenge` event and extract nonce
    /// 3. Load or generate device identity (local file, no SSH required)
    /// 4. Build + sign authentication payload
    /// 5. Send `connect` request with full auth + device attestation
    /// 6. Validate `hello-ok` response
    pub async fn connect(
        host: &str,
        port: u16,
        gateway_token: &str,
    ) -> Result<WsClient, WsError> {
        let url = format!("ws://{}:{}/ws", host, port);

        // Step 1: Open WebSocket connection
        let (ws, _response) = connect_async(&url)
            .await
            .map_err(|e| format!("WebSocket connect to {url} failed: {e}"))?;

        // Steps 2-6: Full handshake wrapped in a 30-second timeout.
        // ws is moved into the async block and returned on success so the
        // authenticated stream can be stored in WsClient.
        // If the gateway stalls at any point we surface a clear error rather
        // than hanging forever.
        let ws = timeout(HANDSHAKE_TIMEOUT, async move {
            let mut ws = ws;

            // Step 2: Read connect.challenge event
            let nonce = read_challenge(&mut ws).await?;

            // Step 3: Load or generate device identity and device token
            let identity = auth::load_device_identity()
                .map_err(|e| format!("failed to load device identity: {e}"))?;
            let device_token = auth::load_device_token()
                .map_err(|e| format!("failed to load device token: {e}"))?;

            // Step 4: Build and sign authentication payload
            let signed_at_ms = current_time_ms()?;
            let payload = auth::build_signature_payload(
                &identity.device_id,
                CLIENT_ID,
                CLIENT_MODE,
                CLIENT_ROLE,
                CLIENT_SCOPE,
                signed_at_ms,
                gateway_token,
                &nonce,
                std::env::consts::OS,
                "",
            );
            let signature = auth::sign_payload(&identity.signing_key, &payload);
            let public_key = auth::public_key_base64url(identity);

            // Step 5: Send connect request
            let connect_id = new_request_id();
            let connect_frame = WsFrame::connect_request(
                &connect_id,
                ConnectParams {
                    min_protocol: 3,
                    max_protocol: 3,
                    client: ClientInfo {
                        id: CLIENT_ID.to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        platform: std::env::consts::OS.to_string(),
                        mode: CLIENT_MODE.to_string(),
                    },
                    role: CLIENT_ROLE.to_string(),
                    scopes: vec![CLIENT_SCOPE.to_string()],
                    auth: AuthParams {
                        token: Some(gateway_token.to_string()),
                        device_token,
                    },
                    device: Some(DeviceParams {
                        id: identity.device_id.clone(),
                        public_key,
                        signature,
                        signed_at: signed_at_ms,
                        nonce: nonce.clone(),
                    }),
                    locale: None,
                    user_agent: Some(format!("{}/{}", CLIENT_ID, env!("CARGO_PKG_VERSION"))),
                },
            );

            send_frame(&mut ws, &connect_frame).await?;

            // Step 6: Read hello-ok response
            validate_hello_ok(&mut ws, &connect_id).await?;

            Ok::<_, WsError>(ws)
        })
        .await
        .map_err(|_| -> WsError { "OpenClaw gateway handshake timed out after 10m".into() })??;

        Ok(WsClient {
            host: host.to_string(),
            port,
            gateway_token: gateway_token.to_string(),
            ws,
        })
    }

    /// Send a raw RPC request and wait for the matching response.
    ///
    /// Incoming frames that do not match `method`'s response id are logged and
    /// discarded (broadcast events, etc.). Returns the `payload` field of the
    /// matching `Res` frame on success.
    #[allow(dead_code)]
    pub async fn send_request(&mut self, method: &str, params: Value) -> Result<Value, WsError> {
        let req_id = new_request_id();
        let frame = WsFrame::Req {
            id: req_id.clone(),
            method: method.to_string(),
            params,
        };

        send_frame(&mut self.ws, &frame).await?;

        loop {
            let raw = recv_text_frame(&mut self.ws).await?;
            let ws_frame: WsFrame = serde_json::from_str(&raw)
                .map_err(|e| format!("failed to parse server frame: {e}\nraw: {raw}"))?;

            match ws_frame {
                WsFrame::Res {
                    id,
                    ok,
                    payload,
                    error,
                } if id == req_id => {
                    if !ok {
                        let msg = error
                            .map(|e| format!("{}: {}", e.code, e.message))
                            .unwrap_or_else(|| "unknown error".to_string());
                        return Err(format!("RPC error for method '{method}': {msg}").into());
                    }
                    return Ok(payload.unwrap_or(Value::Null));
                }
                WsFrame::Event { event, .. } => {
                    crate::verbose!(
                        "[ws] unexpected event '{event}' while waiting for {method} response — ignoring"
                    );
                }
                WsFrame::Res { id, .. } => {
                    crate::verbose!("[ws] stale Res id={id} while waiting for {req_id} — ignoring");
                }
                WsFrame::Req { .. } => {
                    crate::verbose!("[ws] unexpected Req frame from server — ignoring");
                }
            }
        }
    }

    /// Send a message to an agent and collect the streaming response.
    ///
    /// Uses a 120-second timeout. For long-running spawn tasks, use
    /// [`agent_chat_with_timeout`][Self::agent_chat_with_timeout] with
    /// [`DEFAULT_SPAWN_TIMEOUT`] or a custom duration.
    ///
    /// Collects `agent` event `text_delta` chunks keyed by `seq` until the
    /// final `Res` frame arrives. The authoritative response text comes from
    /// the `Res` payload `text` field; deltas are returned for streaming
    /// observability only.
    ///
    /// Pass `session_key: Some("ocb:<thread-id>")` to maintain
    /// conversational context across calls. Pass `None` for stateless
    /// single-turn calls.
    pub async fn agent_chat(
        &mut self,
        agent_id: &str,
        message: &str,
        session_key: Option<&str>,
    ) -> Result<AgentChatResult, WsError> {
        self.agent_chat_with_timeout(agent_id, message, session_key, DEFAULT_CHAT_TIMEOUT)
            .await
    }

    /// Send a message to an agent with an explicit timeout.
    ///
    /// Use [`DEFAULT_SPAWN_TIMEOUT`] (300 s) for long-running spawn tasks,
    /// or supply a custom duration.
    ///
    /// Pass `session_key: Some("ocb:<thread-id>")` to maintain
    /// conversational context across calls. Pass `None` for stateless
    /// single-turn calls.
    pub async fn agent_chat_with_timeout(
        &mut self,
        agent_id: &str,
        message: &str,
        session_key: Option<&str>,
        chat_timeout: Duration,
    ) -> Result<AgentChatResult, WsError> {
        timeout(
            chat_timeout,
            self.agent_chat_inner(agent_id, message, session_key),
        )
        .await
        .map_err(|_| -> WsError {
            format!(
                "agent_chat timed out after {}s waiting for agent '{agent_id}' to respond",
                chat_timeout.as_secs()
            )
            .into()
        })?
    }

    /// Send a message to an agent and stream response deltas to a callback.
    ///
    /// Identical to [`agent_chat_with_timeout`][Self::agent_chat_with_timeout]
    /// but calls `on_delta` with each `text_delta` string as it arrives.
    /// Useful for printing live output to stderr while still returning the
    /// full [`AgentChatResult`] on completion.
    ///
    /// The `on_delta` callback is called synchronously inside the async loop
    /// (no extra tasks spawned). Blocking inside `on_delta` will stall the
    /// receive loop, so keep it fast (e.g. `eprint!` + `flush`).
    pub async fn agent_chat_streaming<F>(
        &mut self,
        agent_id: &str,
        message: &str,
        session_key: Option<&str>,
        chat_timeout: Duration,
        on_delta: F,
    ) -> Result<AgentChatResult, WsError>
    where
        F: FnMut(&str),
    {
        timeout(
            chat_timeout,
            self.agent_chat_inner_streaming(agent_id, message, session_key, on_delta),
        )
        .await
        .map_err(|_| -> WsError {
            format!(
                "agent_chat timed out after {}s waiting for agent '{agent_id}' to respond",
                chat_timeout.as_secs()
            )
            .into()
        })?
    }

    /// Inner streaming implementation — same as `agent_chat_inner` but calls
    /// `on_delta` for each assistant text delta before inserting into the map.
    async fn agent_chat_inner_streaming<F>(
        &mut self,
        agent_id: &str,
        message: &str,
        session_key: Option<&str>,
        mut on_delta: F,
    ) -> Result<AgentChatResult, WsError>
    where
        F: FnMut(&str),
    {
        let req_id = new_request_id();
        let idempotency_key = new_request_id();

        let frame = WsFrame::agent_request(
            &req_id,
            AgentParams {
                message: message.to_string(),
                agent_id: Some(agent_id.to_string()),
                idempotency_key,
                session_key: session_key.map(str::to_string),
                thinking: None,
                timeout: None,
            },
        );

        send_frame(&mut self.ws, &frame).await?;

        let mut run_id: Option<String> = None;
        let mut delta_map: BTreeMap<u64, String> = BTreeMap::new();
        let mut final_text: Option<String> = None;
        let mut usage: Option<Value> = None;
        let mut accepted = false;
        let mut completion_res_received = false;
        let mut event_count: u64 = 0;
        const MAX_EVENTS: u64 = 50_000;
        const MAX_EVENTS_AFTER_COMPLETION: u64 = 500;

        loop {
            event_count += 1;
            if event_count > MAX_EVENTS {
                return Err(format!(
                    "agent_chat exceeded {MAX_EVENTS} events without completion (run_id={run_id:?}) — aborting to prevent unbounded growth"
                ).into());
            }
            if completion_res_received && event_count > MAX_EVENTS_AFTER_COMPLETION {
                crate::verbose!(
                    "[ws] completion Res received but lifecycle end never fired after {MAX_EVENTS_AFTER_COMPLETION} more frames — exiting loop"
                );
                break;
            }

            let raw = recv_text_frame(&mut self.ws).await.map_err(|e| {
                let msg = e.to_string();
                if msg.starts_with("WebSocket closed") {
                    format!("connection dropped during agent execution (run_id={run_id:?}): {msg}")
                } else {
                    msg
                }
            })?;

            let ws_frame: WsFrame = serde_json::from_str(&raw)
                .map_err(|e| format!("failed to parse server frame: {e}\nraw: {raw}"))?;

            match ws_frame {
                WsFrame::Res {
                    id,
                    ok,
                    payload,
                    error,
                } if id == req_id && !accepted => {
                    if !ok {
                        let err = error.as_ref();
                        let code = err.map(|e| e.code.as_str()).unwrap_or("UNKNOWN");
                        let msg = err.map(|e| e.message.as_str()).unwrap_or("unknown error");

                        if matches!(
                            code,
                            "AUTH_FAILED" | "AUTH_EXPIRED" | "UNAUTHORIZED" | "FORBIDDEN"
                        ) {
                            return Err(format!(
                                "agent RPC auth error ({code}): {msg} — \
                                 run `ocb auth reset` and re-pair"
                            )
                            .into());
                        }

                        return Err(format!("agent RPC error ({code}): {msg}").into());
                    }

                    if let Some(ref p) = payload {
                        if let Some(id_val) = p.get("runId").and_then(Value::as_str) {
                            run_id = Some(id_val.to_string());
                        }
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            final_text = Some(t.to_string());
                        }
                        usage = p.get("usage").cloned();
                    }
                    accepted = true;

                    if final_text.is_some() {
                        break;
                    }
                    continue;
                }

                WsFrame::Event { event, payload, .. } if event == "agent" => {
                    match serde_json::from_value::<AgentEventPayload>(payload.clone()) {
                        Ok(agent_event) => {
                            let our_run = match &run_id {
                                Some(id) => id.clone(),
                                None => {
                                    run_id = Some(agent_event.run_id.clone());
                                    agent_event.run_id.clone()
                                }
                            };

                            if agent_event.run_id != our_run {
                                continue;
                            }

                            let stream = agent_event.stream.as_deref().unwrap_or("");

                            match stream {
                                "assistant" => {
                                    if let Some(text) =
                                        agent_event.data.get("delta").and_then(Value::as_str)
                                    {
                                        on_delta(text);
                                        delta_map.insert(agent_event.seq, text.to_string());
                                    }
                                    if let Some(text) =
                                        agent_event.data.get("text").and_then(Value::as_str)
                                    {
                                        final_text = Some(text.to_string());
                                    }
                                }
                                "lifecycle" => {
                                    let phase = agent_event
                                        .data
                                        .get("phase")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    if phase == "end" {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            crate::verbose!("[ws] failed to parse agent event payload: {e} — ignoring");
                        }
                    }
                }

                WsFrame::Event { event, .. } if event == "tick" => {}

                WsFrame::Event { event, .. }
                    if matches!(event.as_str(), "presence" | "health" | "status") =>
                {
                    crate::verbose!("[ws] skipping '{event}' event during agent_chat_streaming");
                }

                WsFrame::Event { event, .. } => {
                    crate::verbose!("[ws] unhandled event '{event}' during agent_chat_streaming — ignoring");
                }

                WsFrame::Res {
                    id, ok, payload, ..
                } if id == req_id && accepted => {
                    completion_res_received = true;
                    if ok && let Some(ref p) = payload {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            final_text = Some(t.to_string());
                        }
                        if usage.is_none() {
                            usage = p.get("usage").cloned();
                        }
                    }
                }

                WsFrame::Res { id, .. } => {
                    crate::verbose!("[ws] unexpected Res id={id} during agent_chat_streaming — ignoring");
                }

                WsFrame::Req { .. } => {
                    crate::verbose!("[ws] unexpected Req frame from server — ignoring");
                }
            }
        }

        let deltas: Vec<String> = delta_map.into_values().filter(|s| !s.is_empty()).collect();
        let text = final_text.unwrap_or_else(|| deltas.join(""));

        Ok(AgentChatResult {
            run_id: run_id.unwrap_or_default(),
            text,
            deltas,
            usage,
        })
    }

    /// Inner implementation — runs without a timeout wrapper.
    async fn agent_chat_inner(
        &mut self,
        agent_id: &str,
        message: &str,
        session_key: Option<&str>,
    ) -> Result<AgentChatResult, WsError> {
        let req_id = new_request_id();
        let idempotency_key = new_request_id();

        let frame = WsFrame::agent_request(
            &req_id,
            AgentParams {
                message: message.to_string(),
                agent_id: Some(agent_id.to_string()),
                idempotency_key,
                session_key: session_key.map(str::to_string),
                thinking: None,
                timeout: None,
            },
        );

        send_frame(&mut self.ws, &frame).await?;

        // The `agent` RPC is async: the Res comes back immediately with
        // status "accepted" and a runId. The actual response streams via
        // broadcast `agent` events. We break on the `done` event, not on
        // the Res.
        let mut run_id: Option<String> = None;
        let mut delta_map: BTreeMap<u64, String> = BTreeMap::new();
        let mut final_text: Option<String> = None;
        let mut usage: Option<Value> = None;
        let mut accepted = false;
        let mut completion_res_received = false;
        let mut event_count: u64 = 0;
        const MAX_EVENTS: u64 = 50_000; // Safety limit to prevent unbounded growth
        // After completion Res arrives, allow up to this many more frames before giving up.
        // This handles the case where lifecycle "end" never fires (unexpected gateway behavior).
        const MAX_EVENTS_AFTER_COMPLETION: u64 = 500;

        loop {
            event_count += 1;
            if event_count > MAX_EVENTS {
                return Err(format!(
                    "agent_chat exceeded {MAX_EVENTS} events without completion (run_id={run_id:?}) — aborting to prevent unbounded growth"
                ).into());
            }
            // If completion Res already arrived but lifecycle end hasn't fired, drain for a
            // bounded number of additional frames then give up waiting.
            if completion_res_received && event_count > MAX_EVENTS_AFTER_COMPLETION {
                crate::verbose!(
                    "[ws] completion Res received but lifecycle end never fired after {MAX_EVENTS_AFTER_COMPLETION} more frames — exiting loop"
                );
                break;
            }

            let raw = recv_text_frame(&mut self.ws).await.map_err(|e| {
                let msg = e.to_string();
                if msg.starts_with("WebSocket closed") {
                    format!("connection dropped during agent execution (run_id={run_id:?}): {msg}")
                } else {
                    msg
                }
            })?;

            let ws_frame: WsFrame = serde_json::from_str(&raw)
                .map_err(|e| format!("failed to parse server frame: {e}\nraw: {raw}"))?;

            match ws_frame {
                // Initial acknowledgment — extract runId, keep listening
                WsFrame::Res {
                    id,
                    ok,
                    payload,
                    error,
                } if id == req_id && !accepted => {
                    if !ok {
                        let err = error.as_ref();
                        let code = err.map(|e| e.code.as_str()).unwrap_or("UNKNOWN");
                        let msg = err.map(|e| e.message.as_str()).unwrap_or("unknown error");

                        if matches!(
                            code,
                            "AUTH_FAILED" | "AUTH_EXPIRED" | "UNAUTHORIZED" | "FORBIDDEN"
                        ) {
                            return Err(format!(
                                "agent RPC auth error ({code}): {msg} — \
                                 run `ocb auth reset` and re-pair"
                            )
                            .into());
                        }

                        return Err(format!("agent RPC error ({code}): {msg}").into());
                    }

                    // Extract runId from the accepted response
                    if let Some(ref p) = payload {
                        if let Some(id_val) = p.get("runId").and_then(Value::as_str) {
                            run_id = Some(id_val.to_string());
                        }
                        // Some gateway versions may include final text directly
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            final_text = Some(t.to_string());
                        }
                        usage = p.get("usage").cloned();
                    }
                    accepted = true;

                    // If the Res already contains final text (synchronous mode),
                    // we're done. Otherwise keep listening for streaming events.
                    if final_text.is_some() {
                        break;
                    }
                    continue;
                }

                // Streaming agent events — collect deltas, break on lifecycle end
                WsFrame::Event { event, payload, .. } if event == "agent" => {
                    match serde_json::from_value::<AgentEventPayload>(payload.clone()) {
                        Ok(agent_event) => {
                            // Filter by runId — gateway broadcasts ALL agent events
                            let our_run = match &run_id {
                                Some(id) => id.clone(),
                                None => {
                                    run_id = Some(agent_event.run_id.clone());
                                    agent_event.run_id.clone()
                                }
                            };

                            if agent_event.run_id != our_run {
                                continue;
                            }

                            let stream = agent_event.stream.as_deref().unwrap_or("");

                            match stream {
                                // Assistant text deltas — the actual response content
                                "assistant" => {
                                    if let Some(text) =
                                        agent_event.data.get("delta").and_then(Value::as_str)
                                    {
                                        delta_map.insert(agent_event.seq, text.to_string());
                                    }
                                    // data.text has the cumulative text so far — use the
                                    // last one as final_text fallback
                                    if let Some(text) =
                                        agent_event.data.get("text").and_then(Value::as_str)
                                    {
                                        final_text = Some(text.to_string());
                                    }
                                }
                                // Lifecycle: start/end of agent execution
                                "lifecycle" => {
                                    let phase = agent_event
                                        .data
                                        .get("phase")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    if phase == "end" {
                                        break;
                                    }
                                    // phase "start" — just continue
                                }
                                // Other streams (tool calls, thinking, etc.) — skip
                                _ => {}
                            }
                        }
                        Err(e) => {
                            crate::verbose!("[ws] failed to parse agent event payload: {e} — ignoring");
                        }
                    }
                }

                // Keepalive ticks — ignore silently
                WsFrame::Event { event, .. } if event == "tick" => {}

                // Presence/health events — log at trace level and skip
                WsFrame::Event { event, .. }
                    if matches!(event.as_str(), "presence" | "health" | "status") =>
                {
                    crate::verbose!("[ws] skipping '{event}' event during agent_chat");
                }

                // Other events — log but don't crash
                WsFrame::Event { event, .. } => {
                    crate::verbose!("[ws] unhandled event '{event}' during agent_chat — ignoring");
                }

                // Completion Res — same request ID, may arrive before or after streaming events.
                // Store any text/usage from it, but do NOT break yet — wait for lifecycle "end"
                // to ensure all assistant deltas have been collected first.
                WsFrame::Res {
                    id, ok, payload, ..
                } if id == req_id && accepted => {
                    completion_res_received = true;
                    if ok && let Some(ref p) = payload {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            final_text = Some(t.to_string());
                        }
                        if usage.is_none() {
                            usage = p.get("usage").cloned();
                        }
                    }
                    // Do NOT break here — lifecycle "end" event is the authoritative termination signal.
                    // The completion Res may arrive before assistant text_delta events due to gateway
                    // event ordering. Continue the loop until lifecycle end fires.
                }

                // Other responses
                WsFrame::Res { id, .. } => {
                    crate::verbose!("[ws] unexpected Res id={id} during agent_chat — ignoring");
                }

                WsFrame::Req { .. } => {
                    crate::verbose!("[ws] unexpected Req frame from server — ignoring");
                }
            }
        }

        // Assemble deltas in seq order for the observability slice
        let deltas: Vec<String> = delta_map.into_values().filter(|s| !s.is_empty()).collect();

        // Use the authoritative Res payload text if available; otherwise fall
        // back to the delta stream (some gateway versions may omit the text
        // field in the Res payload for streaming-only responses).
        let text = final_text.unwrap_or_else(|| deltas.join(""));

        Ok(AgentChatResult {
            run_id: run_id.unwrap_or_default(),
            text,
            deltas,
            usage,
        })
    }

    /// Read the next frame from the WebSocket, parse it as a WsFrame.
    /// Used by the TUI background task.
    pub async fn next_frame(&mut self) -> Result<WsFrame, WsError> {
        let raw = recv_text_frame(&mut self.ws).await?;
        serde_json::from_str(&raw).map_err(|e| format!("failed to parse frame: {e}").into())
    }

    /// Close the WebSocket connection gracefully.
    pub async fn disconnect(&mut self) -> Result<(), WsError> {
        self.ws
            .close(None)
            .await
            .map_err(|e| format!("WebSocket close failed: {e}").into())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read frames until a `connect.challenge` event arrives, then return its nonce.
async fn read_challenge(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<String, WsError> {
    loop {
        let raw = recv_text_frame(ws).await?;
        let frame: WsFrame = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse challenge frame: {e}\nraw: {raw}"))?;

        match frame {
            WsFrame::Event { event, payload, .. } if event == "connect.challenge" => {
                let challenge: ChallengePayload = serde_json::from_value(payload)
                    .map_err(|e| format!("failed to parse connect.challenge payload: {e}"))?;
                return Ok(challenge.nonce);
            }
            WsFrame::Event { event, .. } => {
                crate::verbose!("[ws] unexpected event '{event}' before connect.challenge — ignoring");
            }
            other => {
                crate::verbose!("[ws] unexpected frame before connect.challenge: {other:?}");
            }
        }
    }
}

/// Read frames until the connect `Res` arrives and validate it is `hello-ok`.
async fn validate_hello_ok(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    connect_id: &str,
) -> Result<(), WsError> {
    loop {
        let raw = recv_text_frame(ws).await?;
        let frame: WsFrame = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse hello-ok frame: {e}\nraw: {raw}"))?;

        match frame {
            WsFrame::Res {
                id,
                ok,
                payload,
                error,
            } if id == connect_id => {
                if !ok {
                    let msg = error
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "auth rejected".to_string());
                    return Err(format!("connect rejected: {msg}").into());
                }

                // Validate payload type is "hello-ok"
                let payload_type = payload
                    .as_ref()
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");

                if payload_type != "hello-ok" {
                    return Err(format!(
                        "unexpected connect response type '{payload_type}', expected 'hello-ok'"
                    )
                    .into());
                }

                // Extract and persist device token if the gateway issued one.
                // This happens after pairing approval or on first authenticated connect.
                if let Some(ref p) = payload
                    && let Some(device_token) = p
                        .get("auth")
                        .and_then(|a| a.get("deviceToken"))
                        .and_then(Value::as_str)
                {
                    let role = p
                        .get("auth")
                        .and_then(|a| a.get("role"))
                        .and_then(Value::as_str)
                        .unwrap_or("operator");
                    let scopes: Vec<String> = p
                        .get("auth")
                        .and_then(|a| a.get("scopes"))
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    persist_device_token(device_token, role, &scopes);
                }

                return Ok(());
            }
            WsFrame::Event { event, .. } => {
                crate::verbose!("[ws] event '{event}' while waiting for hello-ok — ignoring");
            }
            WsFrame::Res { id, .. } => {
                crate::verbose!("[ws] stale Res id={id} while waiting for connect Res — ignoring");
            }
            WsFrame::Req { .. } => {
                crate::verbose!("[ws] unexpected Req frame from server — ignoring");
            }
        }
    }
}

/// Receive the next text message from any stream that yields tungstenite
/// `Message` items.
///
/// Skips ping/pong/binary/raw frames. Returns an error if the connection
/// closes before a text frame arrives.
///
/// Used by both the non-split `WsStream` path (`recv_text_frame`) and the
/// split `WsReadHalf` path (`next_frame_read_half`).
async fn recv_text_from_stream<S>(stream: &mut S) -> Result<String, WsError>
where
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                // tungstenite handles pong automatically; just continue
                continue;
            }
            Some(Ok(Message::Close(frame))) => {
                let reason = frame
                    .map(|f| format!("{}: {}", f.code, f.reason))
                    .unwrap_or_else(|| "no reason".to_string());
                return Err(format!("WebSocket closed: {reason}").into());
            }
            Some(Ok(Message::Binary(bytes))) => {
                crate::verbose!(
                    "[ws] unexpected binary frame ({} bytes) — ignoring",
                    bytes.len()
                );
                continue;
            }
            Some(Ok(Message::Frame(_))) => {
                // Raw frames are an internal tungstenite detail; skip
                continue;
            }
            Some(Err(e)) => {
                return Err(format!("WebSocket receive error: {e}").into());
            }
            None => {
                return Err("WebSocket stream ended unexpectedly".into());
            }
        }
    }
}

/// Receive the next text frame from the non-split WebSocket stream.
async fn recv_text_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<String, WsError> {
    recv_text_from_stream(ws).await
}

/// Read the next [`WsFrame`] from a split read half.
///
/// Used by the TUI background task which holds a `WsReadHalf` directly.
pub async fn next_frame_read_half(stream: &mut WsReadHalf) -> Result<WsFrame, WsError> {
    let raw = recv_text_from_stream(stream).await?;
    serde_json::from_str(&raw).map_err(|e| format!("failed to parse frame: {e}").into())
}

/// Serialize a [`WsFrame`] and send it via a split write half.
///
/// Used by the TUI background task which holds a `WsWriteHalf` directly.
pub async fn send_frame_write_half(
    sink: &mut WsWriteHalf,
    frame: &WsFrame,
) -> Result<(), WsError> {
    let json =
        serde_json::to_string(frame).map_err(|e| format!("failed to serialize WsFrame: {e}"))?;
    sink.send(Message::Text(json.into()))
        .await
        .map_err(|e| format!("WebSocket send failed: {e}").into())
}

/// Serialize a [`WsFrame`] and send it as a text WebSocket message.
async fn send_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    frame: &WsFrame,
) -> Result<(), WsError> {
    let json =
        serde_json::to_string(frame).map_err(|e| format!("failed to serialize WsFrame: {e}"))?;
    ws.send(Message::Text(json.into()))
        .await
        .map_err(|e| format!("WebSocket send failed: {e}").into())
}

/// Return the current time as milliseconds since the Unix epoch.
fn current_time_ms() -> Result<u64, WsError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|e| format!("system clock error: {e}").into())
}

/// Persist a device token received in a hello-ok response to the local auth file.
///
/// Uses an atomic write (temp file → rename) with 0600 permissions.
/// Writes in the format expected by [`crate::auth::load_device_token`].
///
/// On failure, logs a warning but does not abort the connection — the token
/// will be re-issued on the next authenticated connect.
fn persist_device_token(token: &str, role: &str, scopes: &[String]) {
    let updated_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let config_dir = match crate::config_dir() {
        Ok(d) => d,
        Err(e) => {
            crate::verbose!("[ws] warning: cannot determine config dir for device token persistence: {e}");
            return;
        }
    };

    let auth_path = config_dir.join("openclaw-bridge").join("openclaw-device-auth.json");

    // Load existing file to preserve deviceId and other role tokens.
    let mut existing: serde_json::Value = if auth_path.exists() {
        match fs::read_to_string(&auth_path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or(serde_json::json!({
                "version": 1,
                "deviceId": "",
                "tokens": {}
            })),
            Err(_) => serde_json::json!({
                "version": 1,
                "deviceId": "",
                "tokens": {}
            }),
        }
    } else {
        serde_json::json!({
            "version": 1,
            "deviceId": "",
            "tokens": {}
        })
    };

    // Update the token entry for this role.
    existing["tokens"][role] = serde_json::json!({
        "token": token,
        "role": role,
        "scopes": scopes,
        "updatedAtMs": updated_at_ms
    });

    let raw = match serde_json::to_string_pretty(&existing) {
        Ok(s) => s,
        Err(e) => {
            crate::verbose!("[ws] warning: failed to serialize device token: {e}");
            return;
        }
    };

    // Ensure parent directory exists.
    if let Some(parent) = auth_path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        crate::verbose!("[ws] warning: failed to create config dir for device token: {e}");
        return;
    }

    // Atomic write: temp file → set 0600 → rename.
    let tmp_path = auth_path.with_extension("tmp");
    if let Err(e) = fs::write(&tmp_path, &raw) {
        crate::verbose!("[ws] warning: failed to write temp device token file: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)) {
            crate::verbose!("[ws] warning: failed to set device token file permissions: {e}");
            let _ = fs::remove_file(&tmp_path);
            return;
        }
    }

    if let Err(e) = fs::rename(&tmp_path, &auth_path) {
        crate::verbose!("[ws] warning: failed to rename device token file: {e}");
        let _ = fs::remove_file(&tmp_path);
        return;
    }

    crate::verbose!("[ws] device token received and persisted (role={role})");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- current_time_ms ---

    #[test]
    fn current_time_ms_returns_reasonable_value() {
        let ms = current_time_ms().expect("current_time_ms should succeed");
        // 2024-01-01 in ms = 1_704_067_200_000
        assert!(
            ms > 1_704_067_200_000,
            "timestamp should be after 2024-01-01"
        );
    }

    // --- AgentChatResult ---

    #[test]
    fn agent_chat_result_text_is_authoritative() {
        // When final_text is set from Res payload, it should be returned as-is
        // even if it differs from the deltas join (e.g. gateway did post-processing).
        let result = AgentChatResult {
            run_id: "run-1".to_string(),
            text: "final authoritative text".to_string(),
            deltas: vec!["chunk1 ".to_string(), "chunk2".to_string()],
            usage: None,
        };
        assert_eq!(result.text, "final authoritative text");
    }

    #[test]
    fn agent_chat_result_fallback_to_deltas_joined() {
        // Simulates the fallback path where Res has no text field
        let deltas = vec!["hello ".to_string(), "world".to_string()];
        let text = None::<String>.unwrap_or_else(|| deltas.join(""));
        assert_eq!(text, "hello world");
    }
}
