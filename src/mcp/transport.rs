//! Stdio JSON-RPC 2.0 transport for the MCP server.
//!
//! Line-delimited JSON over stdin/stdout:
//! - One JSON object per line, `\n` terminated.
//! - stdin  = requests from Claude Code
//! - stdout = responses + notifications
//! - stderr = diagnostics only (use [`eprintln!`])
//!
//! **stdout is sacred** — no output except well-formed JSON-RPC lines.

use std::io;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Async transport over stdin/stdout.
///
/// Created once at server startup. Reading is async; writing serialises to
/// JSON then flushes so each message is delivered atomically.
pub struct StdioTransport {
    reader: BufReader<tokio::io::Stdin>,
    stdout: tokio::io::Stdout,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    /// Create a new transport bound to the process's stdin/stdout.
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            stdout: tokio::io::stdout(),
        }
    }

    /// Read one line from stdin and parse it as a JSON value.
    ///
    /// Returns `Ok(None)` when stdin is closed (EOF), signalling the server
    /// loop to exit cleanly.
    pub async fn read_message(&mut self) -> io::Result<Option<Value>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF — client closed stdin.
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Empty line — skip, not an error.
            return Ok(Some(Value::Null));
        }
        match serde_json::from_str(trimmed) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                eprintln!("[mcp/transport] json parse error: {e} (line: {trimmed})");
                // Return null so the caller can respond with a parse error if desired.
                Ok(Some(Value::Null))
            }
        }
    }

    /// Serialize `msg` to compact JSON and write it to stdout followed by `\n`.
    ///
    /// Flushes after every write so Claude Code receives the message immediately.
    pub async fn write_message(&mut self, msg: &Value) -> io::Result<()> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stdout.write_all(line.as_bytes()).await?;
        self.stdout.flush().await?;
        Ok(())
    }
}
