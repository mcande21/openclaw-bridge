//! WebSocket background task for the TUI conversation viewer.
//!
//! Spawned as a `tokio::task` by [`super::mod::run_tui`]. Connects to the
//! OpenClaw gateway, authenticates, and forwards [`WsEvent`]s to the main
//! TUI loop via an `mpsc` sender.
//!
//! The task also accepts outgoing messages via `msg_rx`. When a message
//! arrives on that channel the task sends an `agent` RPC to the gateway
//! and streams the response back as normal [`WsEvent`]s.
//!
//! The task exits when the sender is dropped (TUI closed) or when the
//! WebSocket connection terminates.

use tokio::sync::mpsc;

use crate::protocol::{AgentEventPayload, AgentParams, WsFrame, new_request_id};
use crate::ws::{WsClient, next_frame_read_half, send_frame_write_half};

use super::app::WsEvent;

// ---------------------------------------------------------------------------
// WS reader/writer task
// ---------------------------------------------------------------------------

/// Spawn the WebSocket background task.
///
/// Connects to the OpenClaw gateway and:
/// - Streams incoming [`WsEvent`]s through `event_tx` until the connection
///   drops or the receiver is closed.
/// - Accepts outgoing messages via `msg_rx`. Each message is sent as an
///   `agent` RPC using `agent_id` and `session_key`.
///
/// Returns the `JoinHandle` for the spawned task. The caller should drop
/// or abort this handle when the TUI exits.
pub fn spawn_ws_task(
    event_tx: mpsc::Sender<WsEvent>,
    msg_rx: mpsc::Receiver<String>,
    agent_id: Option<String>,
    session_key: Option<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_ws_task(event_tx, msg_rx, agent_id, session_key).await;
    })
}

/// Inner async function that drives the WS read/write loop.
async fn run_ws_task(
    tx: mpsc::Sender<WsEvent>,
    mut msg_rx: mpsc::Receiver<String>,
    agent_id: Option<String>,
    session_key: Option<String>,
) {
    let token = match crate::load_gateway_token() {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(WsEvent::Error(e)).await;
            return;
        }
    };

    let ws_host = crate::resolve_ws_host();
    let ws_port = crate::WS_PORT;

    // Connect and authenticate.
    let client = match WsClient::connect(&ws_host, ws_port, &token).await {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send(WsEvent::Error(format!("connect failed: {e}")))
                .await;
            return;
        }
    };

    // Notify the TUI that we're connected.
    if tx.send(WsEvent::Connected).await.is_err() {
        return;
    }

    // Split the authenticated stream so we can read and write concurrently.
    let (mut read_half, mut write_half) = client.split();

    // Main select loop: drive both incoming frames and outgoing messages.
    loop {
        tokio::select! {
            // --- Incoming frame from gateway ---
            frame_result = next_frame_read_half(&mut read_half) => {
                match frame_result {
                    Ok(frame) => {
                        if tx.is_closed() {
                            break;
                        }
                        handle_frame(frame, &tx).await;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("WebSocket closed") || msg.contains("stream ended") {
                            let _ = tx.send(WsEvent::Disconnected(msg)).await;
                        } else {
                            let _ = tx.send(WsEvent::Error(msg)).await;
                        }
                        break;
                    }
                }
            }

            // --- Outgoing message from TUI ---
            maybe_msg = msg_rx.recv() => {
                match maybe_msg {
                    Some(message) => {
                        let aid = agent_id.as_deref().unwrap_or("main");
                        let req_id = new_request_id();
                        // Prefix the wire message so the remote agent knows who is sending.
                        // JSONL stores the original unprefixed text (written in
                        // handle_key_insert before the message is queued here).
                        let wire_message = format!("[User] {message}");
                        let frame = WsFrame::agent_request(
                            &req_id,
                            AgentParams {
                                message: wire_message,
                                agent_id: Some(aid.to_string()),
                                idempotency_key: new_request_id(),
                                session_key: session_key.clone(),
                                thinking: None,
                                timeout: None,
                            },
                        );
                        match send_frame_write_half(&mut write_half, &frame).await {
                            Ok(()) => {
                                crate::verbose!("[tui/ws] sent agent message to {}", aid);
                            }
                            Err(e) => {
                                crate::verbose!("[tui/ws] send error: {}", e);
                                let _ = tx
                                    .send(WsEvent::Error(format!("send failed: {e}")))
                                    .await;
                                break;
                            }
                        }
                        // Response streams back via agent events — no blocking wait needed.
                    }
                    None => {
                        // Channel closed: TUI is shutting down.
                        break;
                    }
                }
            }
        }
    }
}

/// Dispatch a single incoming WsFrame into WsEvents.
async fn handle_frame(frame: WsFrame, tx: &mpsc::Sender<WsEvent>) {
    match frame {
        // Agent streaming events — the bulk of live output.
        WsFrame::Event { event, payload, .. } if event == "agent" => {
            match serde_json::from_value::<AgentEventPayload>(payload) {
                Ok(agent_event) => {
                    let stream = agent_event.stream.as_deref().unwrap_or("");
                    match stream {
                        "assistant" => {
                            // Text delta — forward to TUI.
                            if let Some(delta) = agent_event
                                .data
                                .get("delta")
                                .and_then(serde_json::Value::as_str)
                            {
                                let _ = tx
                                    .send(WsEvent::Delta {
                                        run_id: agent_event.run_id,
                                        seq: agent_event.seq,
                                        text: delta.to_string(),
                                    })
                                    .await;
                            }
                        }
                        "lifecycle" => {
                            let phase = agent_event
                                .data
                                .get("phase")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            if phase == "end" {
                                // Turn complete — extract final text if present.
                                let final_text = agent_event
                                    .data
                                    .get("text")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string);
                                let _ = tx
                                    .send(WsEvent::TurnComplete {
                                        run_id: agent_event.run_id,
                                        final_text,
                                    })
                                    .await;
                            }
                        }
                        _ => {
                            // Other streams (tool_call, thinking) — silently skip for now.
                        }
                    }
                }
                Err(_) => {
                    // Malformed payload — ignore.
                }
            }
        }

        // Completion response — may carry the final authoritative text.
        WsFrame::Res {
            ok,
            payload: Some(ref p),
            ..
        } if ok => {
            if let Some(run_id) = p.get("runId").and_then(serde_json::Value::as_str) {
                let final_text = p
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
                if final_text.is_some() {
                    let _ = tx
                        .send(WsEvent::TurnComplete {
                            run_id: run_id.to_string(),
                            final_text,
                        })
                        .await;
                }
            }
        }

        // Keepalive ticks — ignore silently.
        WsFrame::Event { event, .. } if event == "tick" => {}

        // All other frames — silently skip.
        _ => {}
    }
}
