//! MCP tool schemas and request handlers.
//!
//! Phase 2 tools:
//! - `channel_history` — reads last N messages from the local JSONL thread.
//! - `reply` — sends a message to Aria via WebSocket, waits for streaming response.
//!
//! The `reply` tool is deferred: it persists the outbound message, sends a WS
//! request, and returns `ToolCallResult::PendingReply`. The select loop in
//! `mod.rs` drives the WS and emits the MCP response once the agent completes.

use serde_json::{Value, json};

use crate::conversation;

// ---------------------------------------------------------------------------
// Tool schema registry
// ---------------------------------------------------------------------------

/// Return the JSON schema objects for all tools advertised to Claude Code.
///
/// Called in response to `tools/list` requests.
pub fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "channel_history",
            "description": "Retrieve recent messages from the conversation thread with Aria",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "last": {
                        "type": "integer",
                        "description": "Number of recent messages to retrieve (default: 10)"
                    }
                }
            }
        }),
        json!({
            "name": "reply",
            "description": "Send a message to Aria in the current conversation thread",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to Aria"
                    }
                },
                "required": ["message"]
            }
        }),
    ]
}

// ---------------------------------------------------------------------------
// Tool call result
// ---------------------------------------------------------------------------

/// Result type for tool dispatches that need deferred WS completion.
///
/// `Immediate` variants are sent directly as the MCP response. `PendingReply`
/// tells the select loop to hold off — the actual response arrives later via
/// the WebSocket branch.
pub enum ToolCallResult {
    /// Respond immediately with this JSON value.
    Immediate(Value),
    /// Defer response: send WS request for this message, wait for WS response.
    PendingReply {
        /// The message text to send.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

/// Handle a `channel_history` tool call.
///
/// Reads the last `params["last"]` (default 10) messages from the thread JSONL
/// and returns them as a JSON array in the MCP tool result format.
pub fn handle_channel_history(params: &Value, thread_id: &str) -> Value {
    let last = params
        .get("last")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(10);

    match conversation::read_thread_tail(thread_id, last) {
        Ok(messages) => {
            let items: Vec<Value> = messages
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "thread_id": m.thread_id,
                        "role": m.role,
                        "content": m.content,
                        "timestamp": m.timestamp,
                        "run_id": m.run_id,
                        "source": m.source,
                    })
                })
                .collect();

            json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&items)
                            .unwrap_or_else(|_| "[]".to_string())
                    }
                ]
            })
        }
        Err(e) => {
            eprintln!("[mcp/tools] channel_history error: {e}");
            json!({
                "isError": true,
                "content": [
                    {
                        "type": "text",
                        "text": format!("Failed to read thread history: {e}")
                    }
                ]
            })
        }
    }
}

/// Handle a `reply` tool call.
///
/// Validates params and returns a `ToolCallResult::PendingReply` so that
/// `mod.rs` can send the WS request and await the streaming response before
/// writing the MCP response back to Claude Code.
pub fn handle_reply(params: &Value) -> ToolCallResult {
    let message = match params.get("message").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        Some(_) => {
            return ToolCallResult::Immediate(json!({
                "isError": true,
                "content": [{"type": "text", "text": "reply: message cannot be empty"}]
            }));
        }
        None => {
            return ToolCallResult::Immediate(json!({
                "isError": true,
                "content": [{"type": "text", "text": "reply: missing required parameter 'message'"}]
            }));
        }
    };

    ToolCallResult::PendingReply { message }
}
