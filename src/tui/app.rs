//! TUI application state model.
//!
//! Holds all mutable state for the conversation viewer: messages, input buffer,
//! scroll position, status line, and connection status.

use chrono::{DateTime, Local};

// ---------------------------------------------------------------------------
// Connection status
// ---------------------------------------------------------------------------

/// Connection lifecycle state for the WebSocket reader task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Attempting to connect to the gateway.
    Connecting,
    /// Authenticated and receiving frames.
    Connected,
    /// WebSocket disconnected (reason included if available).
    Disconnected(String),
    /// Connection attempt failed with error message.
    Error(String),
}

impl std::fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionStatus::Connecting => write!(f, "connecting…"),
            ConnectionStatus::Connected => write!(f, "connected"),
            ConnectionStatus::Disconnected(reason) => {
                if reason.is_empty() {
                    write!(f, "disconnected")
                } else {
                    write!(f, "disconnected: {reason}")
                }
            }
            ConnectionStatus::Error(e) => write!(f, "error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Message model
// ---------------------------------------------------------------------------

/// Role of a message in the conversation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    /// The human operator typing directly in the TUI.
    Operator,
    /// Outbound message sent by Claude Code via `ocb conversation send`.
    User,
    /// Inbound response from the remote agent.
    Assistant,
    /// System-level messages (connection events, errors, status).
    System,
}

/// A single displayed message in the TUI conversation view.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Local>,
    /// Optional run ID from the gateway — shown in verbose mode.
    pub run_id: Option<String>,
}

impl ChatMessage {
    /// Create a message from the human operator typing in the TUI.
    pub fn operator(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Operator,
            content: content.into(),
            timestamp: Local::now(),
            run_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            timestamp: Local::now(),
            run_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>, run_id: Option<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            timestamp: Local::now(),
            run_id,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            timestamp: Local::now(),
            run_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming accumulator
// ---------------------------------------------------------------------------

/// Accumulates streaming text_delta events for the currently-in-flight
/// assistant turn. Cleared when the turn completes.
#[derive(Debug, Default, Clone)]
pub struct StreamAccumulator {
    /// The run ID of the in-flight request.
    pub run_id: Option<String>,
    /// Accumulated text so far (delta chunks appended in seq order).
    pub text: String,
    /// True once the lifecycle "end" event fires.
    pub complete: bool,
}

impl StreamAccumulator {
    pub fn is_active(&self) -> bool {
        self.run_id.is_some() && !self.complete
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ---------------------------------------------------------------------------
// Input mode
// ---------------------------------------------------------------------------

/// Whether the input box is focused for typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    /// Normal mode — keyboard shortcuts active (scroll, quit, etc.).
    Normal,
    /// Insert mode — input field captures all keystrokes.
    Insert,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Top-level TUI application state.
///
/// The main event loop holds a single `App` and passes `&mut App` to the
/// renderer and event handlers on each tick.
pub struct App {
    /// All messages displayed in the conversation pane.
    pub messages: Vec<ChatMessage>,

    /// Current streaming accumulator (active while a response is in flight).
    pub stream: StreamAccumulator,

    /// Text currently in the input field.
    pub input: String,

    /// Cursor position within the input field (byte offset).
    pub input_cursor: usize,

    /// Current input mode.
    pub input_mode: InputMode,

    /// Vertical scroll offset for the message pane (lines from bottom).
    /// 0 = pinned to bottom (auto-scroll). Positive = scrolled up.
    pub scroll_offset: u16,

    /// Whether the message pane is pinned to the bottom.
    pub pinned_to_bottom: bool,

    /// Connection status — updated by the WS task via channel.
    pub status: ConnectionStatus,

    /// Status bar message — transient, shown for a few ticks then cleared.
    pub status_message: Option<String>,

    /// Countdown ticks until `status_message` is cleared (0 = clear now).
    pub status_message_ticks: u8,

    /// Whether the app should exit on the next event loop iteration.
    pub should_quit: bool,

    /// The agent ID currently in view (shown in title bar).
    pub agent_id: Option<String>,

    /// The thread ID currently in view (shown in title bar).
    pub thread_id: Option<String>,

    /// Number of messages loaded from the JSONL file so far.
    ///
    /// The TUI polls the file every N ticks and appends any new messages
    /// (written by other processes such as `ocb conversation send`).
    pub last_loaded_count: usize,
}

impl App {
    /// Create a new, empty App state.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            stream: StreamAccumulator::default(),
            input: String::new(),
            input_cursor: 0,
            input_mode: InputMode::Normal,
            scroll_offset: 0,
            pinned_to_bottom: true,
            status: ConnectionStatus::Connecting,
            status_message: None,
            status_message_ticks: 0,
            should_quit: false,
            agent_id: None,
            thread_id: None,
            last_loaded_count: 0,
        }
    }

    /// Push a message into the conversation view.
    ///
    /// If the view is pinned to the bottom, scroll_offset stays at 0.
    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        if self.pinned_to_bottom {
            self.scroll_offset = 0;
        }
    }

    /// Set a transient status bar message that auto-clears after `ticks` ticks.
    pub fn set_status_message(&mut self, msg: impl Into<String>, ticks: u8) {
        self.status_message = Some(msg.into());
        self.status_message_ticks = ticks;
    }

    /// Advance one tick: decrement status message counter and clear if expired.
    pub fn tick(&mut self) {
        if self.status_message_ticks > 0 {
            self.status_message_ticks -= 1;
            if self.status_message_ticks == 0 {
                self.status_message = None;
            }
        }
    }

    /// Insert a character at the current cursor position.
    pub fn insert_char(&mut self, c: char) {
        let byte_idx = self.input_cursor;
        self.input.insert(byte_idx, c);
        self.input_cursor += c.len_utf8();
    }

    /// Delete the character immediately before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        // Find the previous char boundary.
        let mut start = self.input_cursor - 1;
        while start > 0 && !self.input.is_char_boundary(start) {
            start -= 1;
        }
        self.input.drain(start..self.input_cursor);
        self.input_cursor = start;
    }

    /// Move cursor one character to the left.
    pub fn cursor_left(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let mut pos = self.input_cursor - 1;
        while pos > 0 && !self.input.is_char_boundary(pos) {
            pos -= 1;
        }
        self.input_cursor = pos;
    }

    /// Move cursor one character to the right.
    pub fn cursor_right(&mut self) {
        if self.input_cursor >= self.input.len() {
            return;
        }
        let mut pos = self.input_cursor + 1;
        while pos < self.input.len() && !self.input.is_char_boundary(pos) {
            pos += 1;
        }
        self.input_cursor = pos;
    }

    /// Move cursor to the start of the input.
    pub fn cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    /// Move cursor to the end of the input.
    pub fn cursor_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    /// Take the current input contents, clear the buffer, return the text.
    pub fn take_input(&mut self) -> String {
        let text = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        text
    }

    /// Scroll the message pane up by `n` lines.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.pinned_to_bottom = false;
    }

    /// Scroll the message pane down by `n` lines.
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset == 0 {
            self.pinned_to_bottom = true;
        }
    }

    /// Jump to the bottom of the message pane.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.pinned_to_bottom = true;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Events sent from the WS background task to the main loop
// ---------------------------------------------------------------------------

/// Events produced by the [`super::ws_task`] background task.
///
/// Sent over a `tokio::sync::mpsc` channel to the main TUI event loop.
#[derive(Debug)]
pub enum WsEvent {
    /// WS connection established and authenticated.
    Connected,
    /// A streaming text delta arrived.
    Delta {
        run_id: String,
        seq: u64,
        text: String,
    },
    /// The streaming turn completed — final text from the Res payload.
    TurnComplete {
        run_id: String,
        /// Final authoritative text (may be empty if only deltas are available).
        final_text: Option<String>,
    },
    /// WS disconnected (may reconnect).
    Disconnected(String),
    /// Fatal connection error.
    Error(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace() {
        let mut app = App::new();
        app.insert_char('H');
        app.insert_char('i');
        assert_eq!(app.input, "Hi");
        assert_eq!(app.input_cursor, 2);
        app.backspace();
        assert_eq!(app.input, "H");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn cursor_movement() {
        let mut app = App::new();
        app.insert_char('a');
        app.insert_char('b');
        app.cursor_left();
        assert_eq!(app.input_cursor, 1);
        app.cursor_right();
        assert_eq!(app.input_cursor, 2);
        app.cursor_home();
        assert_eq!(app.input_cursor, 0);
        app.cursor_end();
        assert_eq!(app.input_cursor, 2);
    }

    #[test]
    fn take_input_clears_buffer() {
        let mut app = App::new();
        app.insert_char('x');
        let taken = app.take_input();
        assert_eq!(taken, "x");
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn scroll_pin_behavior() {
        let mut app = App::new();
        assert!(app.pinned_to_bottom);
        app.scroll_up(3);
        assert!(!app.pinned_to_bottom);
        assert_eq!(app.scroll_offset, 3);
        app.scroll_down(3);
        assert!(app.pinned_to_bottom);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn status_message_ticks_down() {
        let mut app = App::new();
        app.set_status_message("hello", 2);
        assert!(app.status_message.is_some());
        app.tick();
        assert!(app.status_message.is_some());
        app.tick();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn push_message_pins_to_bottom() {
        let mut app = App::new();
        app.scroll_up(5);
        assert!(!app.pinned_to_bottom);
        // push_message does NOT re-pin if user has scrolled up.
        // (The test confirms the current scroll stays.)
        app.push_message(ChatMessage::system("test"));
        // still not pinned because user manually scrolled
        assert!(!app.pinned_to_bottom);
        app.scroll_to_bottom();
        app.push_message(ChatMessage::system("test2"));
        assert!(app.pinned_to_bottom);
    }
}
