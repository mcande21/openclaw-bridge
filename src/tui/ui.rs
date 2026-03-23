//! Ratatui renderer for the TUI conversation viewer.
//!
//! The `draw` function is called on every frame tick and renders the full
//! terminal UI from the current [`App`] state.
//!
//! Layout (top to bottom):
//!
//! ```text
//! ┌─ OpenClaw — <agent> [<thread>] ──────────────── <status> ─┐
//! │                                                             │
//! │   Message pane (scrollable)                                 │
//! │                                                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │ > input field                                               │
//! ├─────────────────────────────────────────────────────────────┤
//! │ [N] scroll ↑↓  [i] insert  [Enter] send  [q] quit          │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use super::app::{App, ConnectionStatus, InputMode, MessageRole};

// ---------------------------------------------------------------------------
// Colour palette
// ---------------------------------------------------------------------------

const COLOR_OPERATOR: Color = Color::Green;
const COLOR_USER: Color = Color::Cyan;
const COLOR_ASSISTANT: Color = Color::Magenta;
const COLOR_SYSTEM: Color = Color::Yellow;
const COLOR_TIMESTAMP: Color = Color::DarkGray;
const COLOR_STATUS_OK: Color = Color::Green;
const COLOR_STATUS_ERR: Color = Color::Red;
const COLOR_STATUS_WARN: Color = Color::Yellow;
const COLOR_INPUT_NORMAL: Color = Color::DarkGray;
const COLOR_INPUT_INSERT: Color = Color::Cyan;
const COLOR_STREAM_ACTIVE: Color = Color::Magenta;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Render the full TUI frame from the current application state.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Compute the input box height dynamically based on text length, capped
    // at 4 lines of content (6 rows total including borders).
    let input_lines = if app.input.is_empty() {
        1usize
    } else {
        let chars = app.input.chars().count();
        let width = area.width.saturating_sub(2) as usize; // subtract borders
        if width == 0 {
            1
        } else {
            (chars / width + 1).min(4)
        }
    };
    let input_height = input_lines as u16 + 2; // +2 for borders

    // Three vertical sections: messages | input | help bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),                 // message pane — fills remaining space
            Constraint::Length(input_height),   // input box (dynamic)
            Constraint::Length(1),              // help/status bar
        ])
        .split(area);

    draw_messages(frame, app, chunks[0]);
    draw_input(frame, app, chunks[1]);
    draw_help(frame, app, chunks[2]);
}

// ---------------------------------------------------------------------------
// Message pane
// ---------------------------------------------------------------------------

fn draw_messages(frame: &mut Frame, app: &App, area: Rect) {
    // Build title with agent and thread info.
    let agent_str = app
        .agent_id
        .as_deref()
        .map(|a| format!(" {a}"))
        .unwrap_or_default();
    let thread_str = app
        .thread_id
        .as_deref()
        .map(|t| format!(" [{t}]"))
        .unwrap_or_default();
    let status_str = format_connection_status(&app.status);
    let title = format!("OpenClaw{agent_str}{thread_str}");

    // Available content width (subtract 2 for left/right borders).
    let content_width = area.width.saturating_sub(2);

    // Collect rendered lines from messages.
    let mut items: Vec<ListItem> = app
        .messages
        .iter()
        .flat_map(|msg| render_message(msg, content_width))
        .collect();

    // If there's an active stream, append a streaming placeholder.
    if app.stream.is_active() {
        let preview = if app.stream.text.is_empty() {
            "…".to_string()
        } else {
            app.stream.text.clone()
        };
        // Wrap the preview to the available width minus the "[streaming] " prefix (14 chars).
        let stream_prefix = "  [streaming] ";
        let prefix_len = stream_prefix.chars().count();
        let preview_width = (content_width as usize).saturating_sub(prefix_len);
        let truncated = last_n_chars(&preview, preview_width.max(1));
        let line = Line::from(vec![
            Span::styled(stream_prefix, Style::default().fg(COLOR_STREAM_ACTIVE)),
            Span::raw(truncated),
        ]);
        items.push(ListItem::new(line));
    }

    // Build the block title with status on the right.
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                format!(" {title} "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_top(
            Line::from(Span::styled(
                format!(" {status_str} "),
                style_for_status(&app.status),
            ))
            .alignment(Alignment::Right),
        )
        .borders(Borders::ALL);

    // Compute how many lines fit and apply scroll offset.
    let inner_height = area.height.saturating_sub(2) as usize; // subtract borders
    let total_lines = items.len();

    // Clamp scroll: cannot scroll past the top.
    let scroll_offset = (app.scroll_offset as usize).min(total_lines.saturating_sub(1));

    // Which items to show: take a window ending at `total - scroll_offset`.
    let end = total_lines.saturating_sub(scroll_offset);
    let start = end.saturating_sub(inner_height);
    let visible: Vec<ListItem> = items
        .into_iter()
        .skip(start)
        .take(inner_height)
        .collect();

    let list = List::new(visible).block(block);
    frame.render_widget(list, area);
}

/// Render a single [`ChatMessage`] into one or more [`ListItem`]s.
///
/// `max_width` is the full inner width of the message pane (borders already
/// subtracted by the caller). Content is indented by 2 spaces, so the
/// effective wrap width is `max_width - 2`.
fn render_message(msg: &super::app::ChatMessage, max_width: u16) -> Vec<ListItem<'static>> {
    let ts = msg.timestamp.format("%H:%M:%S").to_string();

    let (role_label, role_color) = match msg.role {
        MessageRole::Operator => ("You", COLOR_OPERATOR),
        MessageRole::User => ("Claude Code", COLOR_USER),
        MessageRole::Assistant => ("Agent", COLOR_ASSISTANT),
        MessageRole::System => ("[system]", COLOR_SYSTEM),
    };

    // Header line: "[HH:MM:SS] Role"
    let header = Line::from(vec![
        Span::styled(format!("[{ts}] "), Style::default().fg(COLOR_TIMESTAMP)),
        Span::styled(
            role_label,
            Style::default()
                .fg(role_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    // Content lines — word-wrap each newline-delimited line to fit the pane.
    let indent = 2usize; // "  " prefix width
    let content_width = (max_width as usize).saturating_sub(indent);

    let content_lines: Vec<ListItem<'static>> = msg
        .content
        .lines()
        .flat_map(|line| wrap_line(line, content_width))
        .map(|wrapped| {
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::raw(wrapped),
            ]))
        })
        .collect();

    let mut result = vec![ListItem::new(header)];
    result.extend(content_lines);
    // Blank separator after each message.
    result.push(ListItem::new(Line::from("")));
    result
}

fn format_connection_status(status: &ConnectionStatus) -> String {
    match status {
        ConnectionStatus::Connecting => "connecting…".to_string(),
        ConnectionStatus::Connected => "● connected".to_string(),
        ConnectionStatus::Disconnected(_) => "○ disconnected".to_string(),
        ConnectionStatus::Error(_) => "✕ error".to_string(),
    }
}

fn style_for_status(status: &ConnectionStatus) -> Style {
    match status {
        ConnectionStatus::Connected => Style::default().fg(COLOR_STATUS_OK),
        ConnectionStatus::Disconnected(_) | ConnectionStatus::Connecting => {
            Style::default().fg(COLOR_STATUS_WARN)
        }
        ConnectionStatus::Error(_) => Style::default().fg(COLOR_STATUS_ERR),
    }
}

// ---------------------------------------------------------------------------
// Input box
// ---------------------------------------------------------------------------

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let (border_style, title_text) = match app.input_mode {
        InputMode::Insert => (
            Style::default().fg(COLOR_INPUT_INSERT),
            " Message (Enter=send, Esc=normal) ",
        ),
        InputMode::Normal => (
            Style::default().fg(COLOR_INPUT_NORMAL),
            " Message [i=insert, q=quit] ",
        ),
    };

    let block = Block::default()
        .title(title_text)
        .borders(Borders::ALL)
        .border_style(border_style);

    // Manually wrap input text at character boundaries so the display
    // matches the cursor calculation exactly (no word-wrap mismatch).
    let inner_width = area.width.saturating_sub(2) as usize;
    let wrapped_input = if inner_width == 0 {
        app.input.clone()
    } else {
        let chars: Vec<char> = app.input.chars().collect();
        let mut lines: Vec<String> = Vec::new();
        for chunk in chars.chunks(inner_width) {
            lines.push(chunk.iter().collect());
        }
        if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n")
        }
    };

    let paragraph = Paragraph::new(wrapped_input).block(block);
    frame.render_widget(paragraph, area);

    // Set cursor position when in insert mode.
    if app.input_mode == InputMode::Insert {
        let char_pos = app.input[..app.input_cursor].chars().count();
        let (cursor_line, cursor_col) = if inner_width == 0 {
            (0usize, 0usize)
        } else {
            (char_pos / inner_width, char_pos % inner_width)
        };
        frame.set_cursor_position((
            area.x + 1 + cursor_col as u16,
            area.y + 1 + cursor_line as u16,
        ));
    }
}

// ---------------------------------------------------------------------------
// Help / status bar
// ---------------------------------------------------------------------------

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let text = if let Some(ref msg) = app.status_message {
        // Transient status message overrides the help bar.
        Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::White),
        ))
    } else {
        match app.input_mode {
            InputMode::Normal => Line::from(vec![
                Span::styled(" [i]", Style::default().fg(Color::Cyan)),
                Span::raw(" insert  "),
                Span::styled("[↑↓/PgUp/PgDn]", Style::default().fg(Color::Cyan)),
                Span::raw(" scroll  "),
                Span::styled("[G]", Style::default().fg(Color::Cyan)),
                Span::raw(" bottom  "),
                Span::styled("[q]", Style::default().fg(Color::Cyan)),
                Span::raw(" quit"),
            ]),
            InputMode::Insert => Line::from(vec![
                Span::styled("[Enter]", Style::default().fg(Color::Cyan)),
                Span::raw(" send  "),
                Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
                Span::raw(" normal mode  "),
                Span::styled("[↑↓]", Style::default().fg(Color::Cyan)),
                Span::raw(" scroll"),
            ]),
        }
    };

    let widget = Paragraph::new(text);
    frame.render_widget(widget, area);
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Return up to the last `n` characters of `s` (by Unicode scalar, not bytes).
fn last_n_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

/// Word-wrap `line` into chunks no wider than `max_width` characters.
///
/// Words longer than `max_width` are broken at char boundaries. An empty
/// `line` produces a single empty string so the blank line is preserved.
/// If `max_width` is 0, the line is returned as-is (no wrapping possible).
fn wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![line.to_string()];
    }

    // Empty line — preserve blank separator.
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in line.split_whitespace() {
        // If the word itself exceeds max_width, break it mid-char.
        if word.chars().count() > max_width {
            // Flush whatever is in `current` first.
            if !current.is_empty() {
                result.push(current.clone());
                current.clear();
            }
            // Chunk the long word at char boundaries.
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == max_width {
                    result.push(chunk.clone());
                    chunk.clear();
                }
                chunk.push(ch);
            }
            // Remainder of the word goes into `current` to be joined with
            // any following words on the same wrapped line.
            current = chunk;
        } else if current.is_empty() {
            current = word.to_string();
        } else if current.chars().count() + 1 + word.chars().count() <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            result.push(current.clone());
            current = word.to_string();
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    if result.is_empty() {
        result.push(String::new());
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::wrap_line;

    #[test]
    fn empty_line_returns_single_empty_string() {
        assert_eq!(wrap_line("", 40), vec![""]);
    }

    #[test]
    fn short_line_not_wrapped() {
        assert_eq!(wrap_line("hello world", 40), vec!["hello world"]);
    }

    #[test]
    fn line_wraps_at_word_boundary() {
        let result = wrap_line("one two three four five", 10);
        // "one two" = 7, "three" = 5, "four" = 4, "five" = 4
        assert_eq!(result, vec!["one two", "three", "four five"]);
    }

    #[test]
    fn long_word_breaks_mid_char() {
        // 15-char word, width 5 → 3 full chunks of 5
        let word = "abcdefghijklmno";
        let result = wrap_line(word, 5);
        assert_eq!(result, vec!["abcde", "fghij", "klmno"]);
    }

    #[test]
    fn long_word_with_remainder_breaks_correctly() {
        // 13-char word, width 5 → 2 full chunks + remainder
        let word = "abcdefghijklm";
        let result = wrap_line(word, 5);
        assert_eq!(result, vec!["abcde", "fghij", "klm"]);
    }

    #[test]
    fn max_width_zero_returns_line_as_is() {
        let line = "any text here";
        assert_eq!(wrap_line(line, 0), vec!["any text here"]);
    }

    #[test]
    fn single_word_fits_exactly() {
        assert_eq!(wrap_line("hello", 5), vec!["hello"]);
    }

    #[test]
    fn url_after_word_breaks_long_url_mid_char() {
        let line = "see https://example.com/very/long/path/that/exceeds/width end";
        let result = wrap_line(line, 20);
        // "see" fits. URL is > 20 chars so it breaks mid-char. "end" follows.
        assert!(result.len() > 1);
        // All chunks must be <= 20 chars.
        for chunk in &result {
            assert!(chunk.chars().count() <= 20, "chunk too wide: {chunk:?}");
        }
    }
}
