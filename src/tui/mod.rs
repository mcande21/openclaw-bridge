//! TUI conversation viewer for the OpenClaw bridge.
//!
//! Entry point: [`run_tui`]. Initialises the terminal, spawns the WebSocket
//! background task, then drives the ratatui event loop until the user quits.
//!
//! ## Keyboard shortcuts
//!
//! | Key | Mode | Action |
//! |-----|------|--------|
//! | `i` | Normal | Enter insert mode |
//! | `Esc` | Insert | Return to normal mode |
//! | `Enter` | Insert | Send message |
//! | `q` | Normal | Quit |
//! | `↑` / `k` | Normal | Scroll up 1 line |
//! | `↓` / `j` | Normal | Scroll down 1 line |
//! | `PgUp` | Normal | Scroll up 10 lines |
//! | `PgDn` | Normal | Scroll down 10 lines |
//! | `G` / `End` | Normal | Scroll to bottom |
//! | `←` `→` `Home` `End` | Insert | Move cursor |
//! | `Backspace` | Insert | Delete char before cursor |

pub mod app;
mod ui;
mod ws_task;

use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use chrono;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::conversation;

use app::{App, ChatMessage, ConnectionStatus, InputMode, WsEvent};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the TUI conversation viewer.
///
/// Optionally resumes a specific conversation `thread_id`. If `None`, the
/// most recent active thread is used (if one exists).
///
/// This function blocks until the user quits the TUI. It must be called
/// from within a tokio runtime context (the WS task uses `tokio::spawn`).
pub async fn run_tui(thread_id: Option<String>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve or create the thread to display.
    let thread = resolve_thread(thread_id.as_deref())?;

    // Load existing conversation history from JSONL.
    let history = load_history(thread.as_ref())?;

    // Bootstrap app state.
    let mut app = App::new();
    if let Some(ref t) = thread {
        app.agent_id = Some(t.agent_id.clone());
        app.thread_id = Some(t.id.clone());
    }

    // Seed the message pane with history.
    app.last_loaded_count = history.len();
    app.messages.extend(history);

    // Channel for TUI → WS task outgoing messages.
    let (msg_tx, msg_rx) = mpsc::channel::<String>(64);

    // Thread context for the WS task (agent RPC parameters).
    let ws_agent_id = thread.as_ref().map(|t| t.agent_id.clone());
    let ws_session_key = thread
        .as_ref()
        .map(|t| format!("ocb:{}", t.id));

    // Spawn the WS background task.
    let (ws_tx, mut ws_rx) = mpsc::channel::<WsEvent>(256);
    let _ws_handle = ws_task::spawn_ws_task(ws_tx, msg_rx, ws_agent_id, ws_session_key);

    // Initialise terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(100);

    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut ws_rx,
        &msg_tx,
        tick_rate,
    )
    .await;

    // Restore terminal regardless of outcome.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

// ---------------------------------------------------------------------------
// Thread resolution
// ---------------------------------------------------------------------------

/// A minimal thread descriptor used internally by the TUI.
struct ThreadInfo {
    pub id: String,
    pub agent_id: String,
}

/// Resolve which thread to display.
///
/// - If `thread_id` is `Some`, find that specific thread.
/// - If `None`, use the most recently active thread.
/// - Returns `Ok(None)` if no threads exist yet (TUI shows empty state).
fn resolve_thread(thread_id: Option<&str>) -> Result<Option<ThreadInfo>, Box<dyn std::error::Error + Send + Sync>> {
    match thread_id {
        Some(id) => {
            let threads = conversation::list_threads().unwrap_or_default();
            let entry = threads.into_iter().find(|t| t.id == id).ok_or_else(|| {
                format!("thread not found: {id}")
            })?;
            Ok(Some(ThreadInfo {
                id: entry.id,
                agent_id: entry.agent_id,
            }))
        }
        None => {
            let threads = conversation::list_threads().unwrap_or_default();
            Ok(threads.last().map(|t| ThreadInfo {
                id: t.id.clone(),
                agent_id: t.agent_id.clone(),
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// History loading
// ---------------------------------------------------------------------------

/// Map a stored [`conversation::Message`] to a [`ChatMessage`] for display.
///
/// Distinguishes the human user (TUI) from Claude Code (CLI) by inspecting
/// the `source` field written at persist time. Both are stored as `User` in
/// JSONL but rendered differently in the TUI.
fn map_conversation_message(m: &conversation::Message) -> ChatMessage {
    let ts_local: chrono::DateTime<chrono::Local> = m.timestamp.into();
    let role = match m.role {
        conversation::MessageRole::User => {
            if m.source.as_deref() == Some("tui") {
                app::MessageRole::Operator
            } else {
                app::MessageRole::User
            }
        }
        conversation::MessageRole::Assistant => app::MessageRole::Assistant,
    };
    ChatMessage {
        role,
        content: m.content.clone(),
        timestamp: ts_local,
        run_id: m.run_id.clone(),
    }
}

/// Load existing conversation messages from the JSONL thread file.
fn load_history(thread: Option<&ThreadInfo>) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error + Send + Sync>> {
    let t = match thread {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let messages = match conversation::read_thread(&t.id) {
        Ok(msgs) => msgs,
        Err(conversation::ConversationError::ThreadNotFound(_)) => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to load thread {}: {e}", t.id).into()),
    };

    Ok(messages.iter().map(map_conversation_message).collect())
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    ws_rx: &mut mpsc::Receiver<WsEvent>,
    msg_tx: &mpsc::Sender<String>,
    tick_rate: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Accumulated deltas keyed by seq for the in-flight turn.
    let mut delta_map: BTreeMap<u64, String> = BTreeMap::new();

    // Set up a file watcher so the TUI reacts to JSONL changes near-instantly
    // instead of polling on a 1-second tick. Uses std::sync::mpsc because
    // the notify callback runs on a background OS thread (not tokio).
    //
    // Falls back to tick-based polling if the watcher cannot be initialised
    // (e.g. unsupported filesystem, permission error). In fallback mode the
    // behaviour is identical to the old implementation.
    let (notify_tx, notify_rx): (std_mpsc::Sender<()>, std_mpsc::Receiver<()>) =
        std_mpsc::channel();

    // `_watcher` must be kept alive for the duration of the event loop.
    // Assigning to `_` would drop it immediately, silencing the watcher.
    let _watcher: Option<Box<dyn notify::Watcher>> =
        build_file_watcher(app, notify_tx);

    // Tick counter used only in fallback mode (watcher unavailable).
    let mut poll_tick: u8 = 0;
    let using_watcher = _watcher.is_some();

    loop {
        // Draw the current frame.
        terminal.draw(|f| ui::draw(f, app))?;

        if app.should_quit {
            break;
        }

        // Drain all pending WS events (non-blocking).
        loop {
            match ws_rx.try_recv() {
                Ok(event) => handle_ws_event(app, event, &mut delta_map),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    app.status = ConnectionStatus::Disconnected("WS task exited".to_string());
                    break;
                }
            }
        }

        // Yield to the tokio scheduler so the WS background task can run.
        // In a current_thread runtime, spawned tasks only execute when the
        // driving future awaits. Without this, the WS task is starved and
        // never connects.
        tokio::time::sleep(tick_rate).await;

        // Drain all pending terminal input (non-blocking).
        while crossterm::event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                handle_key(app, key, msg_tx);
            }
        }

        app.tick();

        if using_watcher {
            // Drain all file-change notifications — each one triggers a JSONL reload.
            // Multiple rapid writes coalesce into a single reload since we process
            // all pending events before looping again.
            while let Ok(()) = notify_rx.try_recv() {
                poll_thread_file(app);
            }
        } else {
            // Fallback: tick-based polling every ~1 second (10 × 100 ms ticks).
            poll_tick = poll_tick.wrapping_add(1);
            if poll_tick.is_multiple_of(10) {
                poll_thread_file(app);
            }
        }
    }

    Ok(())
}

/// Attempt to create a [`notify::RecommendedWatcher`] watching the thread
/// JSONL file. Returns `Some(watcher)` on success, `None` on any failure.
///
/// Failures are intentionally silent — a watcher error must never crash the
/// TUI. The caller falls back to tick-based polling when `None` is returned.
fn build_file_watcher(
    app: &App,
    tx: std_mpsc::Sender<()>,
) -> Option<Box<dyn notify::Watcher>> {
    use notify::Watcher as _;

    let thread_id = app.thread_id.as_deref()?;

    // Resolve the JSONL path. If we can't determine it (e.g. thread not in
    // the index yet), return None and fall back to tick-based polling.
    let jsonl_path = match conversation::thread_file_path(thread_id) {
        Ok(Some(p)) => p,
        _ => return None,
    };

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()?;

    watcher
        .watch(&jsonl_path, notify::RecursiveMode::NonRecursive)
        .ok()?;

    Some(Box::new(watcher))
}

/// Re-read the thread JSONL and append any messages that appeared since we
/// last loaded. Silently ignores errors (thread may not exist yet).
fn poll_thread_file(app: &mut App) {
    let thread_id = match &app.thread_id {
        Some(id) => id.clone(),
        None => return,
    };

    let messages = match conversation::read_thread(&thread_id) {
        Ok(msgs) => msgs,
        Err(_) => return,
    };

    if messages.len() <= app.last_loaded_count {
        return;
    }

    // Append only the new tail, deduplicating by run_id.
    // When Claude Code sends via CLI, the remote agent's response arrives in
    // the TUI via BOTH the WS broadcast (streaming) AND the JSONL file
    // (persisted by the CLI). Skip messages whose run_id already exists.
    for m in messages.iter().skip(app.last_loaded_count) {
        if let Some(ref rid) = m.run_id {
            let already_displayed = app
                .messages
                .iter()
                .any(|existing| existing.run_id.as_deref() == Some(rid.as_str()));
            if already_displayed {
                continue;
            }
        }
        app.push_message(map_conversation_message(m));
    }
    app.last_loaded_count = messages.len();
}

// ---------------------------------------------------------------------------
// WS event handling
// ---------------------------------------------------------------------------

fn handle_ws_event(
    app: &mut App,
    event: WsEvent,
    delta_map: &mut BTreeMap<u64, String>,
) {
    match event {
        WsEvent::Connected => {
            app.status = ConnectionStatus::Connected;
            app.push_message(ChatMessage::system("Connected to gateway."));
        }

        WsEvent::Delta { run_id, seq, text } => {
            // Start accumulator if this is the first delta for a new run.
            if app.stream.run_id.as_deref() != Some(&run_id) {
                app.stream.run_id = Some(run_id.clone());
                app.stream.text.clear();
                app.stream.complete = false;
                delta_map.clear();
            }
            // Fast path: TCP delivers frames in order so append directly.
            // delta_map is still maintained so TurnComplete can fall back to
            // seq-ordered assembly if final_text is absent.
            app.stream.text.push_str(&text);
            delta_map.insert(seq, text);
        }

        WsEvent::TurnComplete { run_id, final_text } => {
            let text = final_text
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    delta_map.values().cloned().collect::<Vec<_>>().join("")
                });

            app.push_message(ChatMessage::assistant(text.clone(), Some(run_id.clone())));

            // Persist the remote agent's response to JSONL so the full
            // conversation is durable and visible to other processes.
            if let Some(ref thread_id) = app.thread_id {
                match conversation::append_message(
                    thread_id,
                    conversation::MessageRole::Assistant,
                    &text,
                    Some(&run_id),
                    None, // source: None for remote agent (assistant) messages
                ) {
                    Ok(_) => {
                        // Advance the count so poll_thread_file does not
                        // re-add this message on the next file poll cycle.
                        app.last_loaded_count += 1;
                    }
                    Err(e) => {
                        app.set_status_message(format!("Save failed: {e}"), 30);
                    }
                }
            }

            app.stream.reset();
            delta_map.clear();
        }

        WsEvent::Disconnected(reason) => {
            app.status = ConnectionStatus::Disconnected(reason.clone());
            app.push_message(ChatMessage::system(format!("Disconnected: {reason}")));
        }

        WsEvent::Error(e) => {
            app.status = ConnectionStatus::Error(e.clone());
            app.push_message(ChatMessage::system(format!("Error: {e}")));
        }
    }
}

// ---------------------------------------------------------------------------
// Key event handling
// ---------------------------------------------------------------------------

fn handle_key(app: &mut App, key: KeyEvent, msg_tx: &mpsc::Sender<String>) {
    match app.input_mode {
        InputMode::Normal => handle_key_normal(app, key),
        InputMode::Insert => handle_key_insert(app, key, msg_tx),
    }
}

fn handle_key_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        // Quit
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.should_quit = true;
        }
        // Ctrl-C always quits
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }

        // Enter insert mode
        KeyCode::Char('i') => {
            app.input_mode = InputMode::Insert;
        }

        // Scrolling
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_up(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down(1);
        }
        KeyCode::PageUp => {
            app.scroll_up(10);
        }
        KeyCode::PageDown => {
            app.scroll_down(10);
        }
        KeyCode::Char('G') | KeyCode::End => {
            app.scroll_to_bottom();
        }

        _ => {}
    }
}

fn handle_key_insert(app: &mut App, key: KeyEvent, msg_tx: &mpsc::Sender<String>) {
    match key.code {
        // Exit insert mode
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
        }

        // Ctrl-C quits even from insert mode
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }

        // Send message
        KeyCode::Enter => {
            let text = app.take_input();
            if !text.trim().is_empty() {
                // Reject sends before the gateway is ready.
                if !matches!(app.status, ConnectionStatus::Connected) {
                    app.set_status_message("Not connected — waiting for gateway", 30);
                    // Put the text back so the user doesn't lose their draft.
                    app.input = text;
                    app.input_cursor = app.input.len();
                    return;
                }

                app.push_message(ChatMessage::operator(text.clone()));
                // Persist the operator message to the JSONL thread so the
                // full conversation history is visible to other processes.
                // Stored as MessageRole::User in JSONL (only User/Assistant
                // exist there); the TUI identifies its own messages by the
                // Operator role added above.
                if let Some(ref thread_id) = app.thread_id {
                    match conversation::append_message(
                        thread_id,
                        conversation::MessageRole::User,
                        &text,
                        None,
                        Some("tui"),
                    ) {
                        Ok(_) => {
                            // Advance the loaded count so poll_thread_file does
                            // not re-display the message we just pushed.
                            app.last_loaded_count += 1;
                        }
                        Err(e) => {
                            app.set_status_message(format!("Save failed: {e}"), 30);
                            // Do NOT increment last_loaded_count — file didn't change.
                        }
                    }
                }
                // Enqueue for the WS task. The task picks this up and sends
                // the agent RPC; the response streams back via WsEvents.
                match msg_tx.try_send(text) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        app.set_status_message("Message queue full — try again", 30);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        app.set_status_message("Gateway connection lost", 30);
                    }
                }
            }
        }

        // Cursor movement
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home => app.cursor_home(),
        KeyCode::End => app.cursor_end(),

        // Backspace
        KeyCode::Backspace => app.backspace(),

        // Delete (forward delete) — not yet implemented, treat as noop
        KeyCode::Delete => {}

        // Scroll even in insert mode
        KeyCode::Up => app.scroll_up(1),
        KeyCode::Down => app.scroll_down(1),
        KeyCode::PageUp => app.scroll_up(10),
        KeyCode::PageDown => app.scroll_down(10),

        // Regular character input
        KeyCode::Char(c) => app.insert_char(c),

        _ => {}
    }
}
