//! Conversation thread persistence for local-to-gateway dialogues.
//!
//! Threads are stored as JSONL files (one JSON `Message` per line) under
//! `~/.config/openclaw-bridge/conversations/`. An index file `threads.json`
//! tracks metadata for all threads including archived ones.
//!
//! ## File layout
//!
//! ```text
//! ~/.config/openclaw-bridge/conversations/
//!   threads.json                   # Index of all threads
//!   <thread-uuid>.jsonl            # Active thread messages
//!   archived/
//!     <thread-uuid>.jsonl          # Archived thread messages
//! ```
//!
//! Auto-archive fires when a thread has had no activity for 48 hours.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ConversationError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("thread not found: {0}")]
    ThreadNotFound(String),
    #[error("cannot determine config directory")]
    NoConfigDir,
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A local-to-gateway conversation thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived: bool,
    /// Session key sent to the OpenClaw gateway to maintain conversational
    /// context across calls. Format: `ocb:<thread-uuid>`.
    #[serde(default)]
    pub session_key: String,
}

/// A single message in a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Who sent this message: `"tui"` (user via TUI), `"cli"` (Claude Code via ocb), or `None`
    /// (remote agent/assistant). Old JSONL files without this field deserialize with
    /// `source: None` (backward-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Sender role for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Full thread index (deserialized from `threads.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ThreadIndex {
    pub threads: Vec<ThreadEntry>,
}

/// Per-thread metadata entry stored in `threads.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEntry {
    pub id: String,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub archived: bool,
    /// Claude Code session ID used by the watcher to resume context.
    /// Set by `ocb watch` when it starts monitoring the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Directory resolution
// ---------------------------------------------------------------------------

/// Return the conversations directory (default location), creating it (and the
/// `archived/` subdirectory) if they do not exist.
pub fn conversations_dir() -> Result<PathBuf, ConversationError> {
    let base = default_config_dir()?;
    let dir = base.join("openclaw-bridge").join("conversations");
    ensure_dirs(&dir)?;
    Ok(dir)
}

/// Ensure the conversations directory and its `archived/` child exist.
fn ensure_dirs(dir: &Path) -> Result<(), ConversationError> {
    fs::create_dir_all(dir)?;
    fs::create_dir_all(dir.join("archived"))?;
    Ok(())
}

/// Resolve the platform config base directory.
///
/// Delegates to the shared [`crate::config_dir`] so that both
/// `conversation.rs` and `auth.rs` resolve to the same path regardless of
/// platform or `XDG_CONFIG_HOME` overrides.
fn default_config_dir() -> Result<PathBuf, ConversationError> {
    crate::config_dir().map_err(|_| ConversationError::NoConfigDir)
}

// ---------------------------------------------------------------------------
// Index I/O
// ---------------------------------------------------------------------------

fn index_path(dir: &Path) -> PathBuf {
    dir.join("threads.json")
}

fn read_index(dir: &Path) -> Result<ThreadIndex, ConversationError> {
    let path = index_path(dir);
    let contents = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ThreadIndex::default()),
        Err(e) => return Err(e.into()),
    };
    Ok(serde_json::from_str(&contents)?)
}

fn write_index(dir: &Path, index: &ThreadIndex) -> Result<(), ConversationError> {
    let path = index_path(dir);
    let json = serde_json::to_string_pretty(index)?;
    // Atomic write: temp file → rename.
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Thread file path helpers
// ---------------------------------------------------------------------------

fn active_path(dir: &Path, thread_id: &str) -> PathBuf {
    dir.join(format!("{thread_id}.jsonl"))
}

fn archived_path(dir: &Path, thread_id: &str) -> PathBuf {
    dir.join("archived").join(format!("{thread_id}.jsonl"))
}

// ---------------------------------------------------------------------------
// Core logic (takes explicit dir — used by public API and tests alike)
// ---------------------------------------------------------------------------

fn create_thread_in(dir: &Path, agent_id: &str) -> Result<Thread, ConversationError> {
    ensure_dirs(dir)?;
    let mut index = read_index(dir)?;

    let now = Utc::now();
    let id = Uuid::new_v4().to_string();
    let session_key = format!("ocb:{id}");
    let thread = Thread {
        id,
        agent_id: agent_id.to_string(),
        created_at: now,
        updated_at: now,
        archived: false,
        session_key,
    };

    // Create an empty JSONL file.
    File::create(active_path(dir, &thread.id))?;

    index.threads.push(ThreadEntry {
        id: thread.id.clone(),
        agent_id: thread.agent_id.clone(),
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        message_count: 0,
        archived: false,
        claude_session_id: None,
    });

    write_index(dir, &index)?;
    Ok(thread)
}

fn append_message_in(
    dir: &Path,
    thread_id: &str,
    role: MessageRole,
    content: &str,
    run_id: Option<&str>,
    source: Option<&str>,
) -> Result<Message, ConversationError> {
    let mut index = read_index(dir)?;

    let entry = index
        .threads
        .iter_mut()
        .find(|e| e.id == thread_id)
        .ok_or_else(|| ConversationError::ThreadNotFound(thread_id.to_string()))?;

    // Dedup: if a run_id is provided, check if it already exists in the thread.
    // This prevents double-writes when both the CLI and TUI persist the same
    // assistant response (one via cmd_conversation_send, one via TUI WS broadcast).
    if let Some(rid) = run_id {
        let check_path = if entry.archived {
            archived_path(dir, thread_id)
        } else {
            active_path(dir, thread_id)
        };
        if check_path.exists()
            && let Ok(existing) = read_jsonl(&check_path)
                && let Some(existing_msg) = existing.into_iter().find(|m| m.run_id.as_deref() == Some(rid)) {
                    return Ok(existing_msg);
                }
    }

    let now = Utc::now();
    let message = Message {
        id: Uuid::new_v4().to_string(),
        thread_id: thread_id.to_string(),
        role,
        content: content.to_string(),
        timestamp: now,
        run_id: run_id.map(str::to_string),
        metadata: None,
        source: source.map(str::to_string),
    };

    let file_path = if entry.archived {
        archived_path(dir, thread_id)
    } else {
        active_path(dir, thread_id)
    };

    let mut file = OpenOptions::new().append(true).open(&file_path)?;
    let line = serde_json::to_string(&message)?;
    writeln!(file, "{line}")?;

    entry.message_count += 1;
    entry.updated_at = now;
    write_index(dir, &index)?;

    Ok(message)
}

fn read_thread_in(dir: &Path, thread_id: &str) -> Result<Vec<Message>, ConversationError> {
    let index = read_index(dir)?;

    let entry = index
        .threads
        .iter()
        .find(|e| e.id == thread_id)
        .ok_or_else(|| ConversationError::ThreadNotFound(thread_id.to_string()))?;

    let file_path = if entry.archived {
        archived_path(dir, thread_id)
    } else {
        active_path(dir, thread_id)
    };

    read_jsonl(&file_path)
}

fn list_threads_in(
    dir: &Path,
    include_archived: bool,
) -> Result<Vec<ThreadEntry>, ConversationError> {
    let index = read_index(dir)?;
    Ok(index
        .threads
        .into_iter()
        .filter(|e| include_archived || !e.archived)
        .collect())
}

fn auto_archive_in(dir: &Path) -> Result<Vec<String>, ConversationError> {
    let mut index = read_index(dir)?;
    let cutoff = Utc::now() - Duration::hours(48);
    let mut archived_ids = Vec::new();

    for entry in index.threads.iter_mut() {
        if !entry.archived && entry.updated_at < cutoff {
            let src = active_path(dir, &entry.id);
            let dst = archived_path(dir, &entry.id);
            match fs::rename(&src, &dst) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            entry.archived = true;
            archived_ids.push(entry.id.clone());
        }
    }

    if !archived_ids.is_empty() {
        write_index(dir, &index)?;
    }

    Ok(archived_ids)
}

#[allow(dead_code)]
fn resume_thread_in(dir: &Path, thread_id: &str) -> Result<(), ConversationError> {
    let mut index = read_index(dir)?;

    let entry = index
        .threads
        .iter_mut()
        .find(|e| e.id == thread_id)
        .ok_or_else(|| ConversationError::ThreadNotFound(thread_id.to_string()))?;

    if !entry.archived {
        return Ok(()); // already active — no-op
    }

    let src = archived_path(dir, thread_id);
    let dst = active_path(dir, thread_id);
    if src.exists() {
        fs::rename(&src, &dst)?;
    }

    entry.archived = false;
    write_index(dir, &index)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API (uses default conversations_dir)
// ---------------------------------------------------------------------------

/// Create a new thread for the given agent, persist it to the index, and
/// return the new `Thread`.
pub fn create_thread(agent_id: &str) -> Result<Thread, ConversationError> {
    let dir = conversations_dir()?;
    create_thread_in(&dir, agent_id)
}

/// Append a message to the thread's JSONL file, update the index metadata,
/// and return the persisted `Message`.
///
/// `source` identifies who sent the message: `Some("tui")` for the user typing
/// in the TUI, `Some("cli")` for Claude Code via `ocb conversation send`, or
/// `None` for the remote agent (assistant) responses.
pub fn append_message(
    thread_id: &str,
    role: MessageRole,
    content: &str,
    run_id: Option<&str>,
    source: Option<&str>,
) -> Result<Message, ConversationError> {
    let dir = conversations_dir()?;
    append_message_in(&dir, thread_id, role, content, run_id, source)
}

/// Read all messages from a thread's JSONL file in order.
pub fn read_thread(thread_id: &str) -> Result<Vec<Message>, ConversationError> {
    let dir = conversations_dir()?;
    read_thread_in(&dir, thread_id)
}

/// Read the last `n` messages from a thread.
pub fn read_thread_tail(thread_id: &str, n: usize) -> Result<Vec<Message>, ConversationError> {
    let all = read_thread(thread_id)?;
    let skip = all.len().saturating_sub(n);
    Ok(all.into_iter().skip(skip).collect())
}

/// List all active (non-archived) threads from the index.
pub fn list_threads() -> Result<Vec<ThreadEntry>, ConversationError> {
    let dir = conversations_dir()?;
    list_threads_in(&dir, false)
}

/// List all threads including archived ones.
#[allow(dead_code)]
pub fn list_all_threads() -> Result<Vec<ThreadEntry>, ConversationError> {
    let dir = conversations_dir()?;
    list_threads_in(&dir, true)
}

/// Archive any thread whose last activity was more than 48 hours ago.
///
/// Returns the list of thread IDs that were archived.
pub fn auto_archive() -> Result<Vec<String>, ConversationError> {
    let dir = conversations_dir()?;
    auto_archive_in(&dir)
}

/// Move an archived thread back to active.
#[allow(dead_code)]
pub fn resume_thread(thread_id: &str) -> Result<(), ConversationError> {
    let dir = conversations_dir()?;
    resume_thread_in(&dir, thread_id)
}

/// Return the filesystem path to the JSONL file for a given thread ID.
///
/// Returns `None` if the thread is not found in the index (e.g. never existed
/// or already purged). Uses the active path unless the entry is archived.
///
/// Exposed primarily for the TUI file watcher so it can set up an inotify/
/// FSEvents watch on the exact file rather than the whole directory.
pub fn thread_file_path(thread_id: &str) -> Result<Option<PathBuf>, ConversationError> {
    let dir = conversations_dir()?;
    let index = read_index(&dir)?;
    let entry = match index.threads.iter().find(|e| e.id == thread_id) {
        Some(e) => e,
        None => return Ok(None),
    };
    let path = if entry.archived {
        archived_path(&dir, thread_id)
    } else {
        active_path(&dir, thread_id)
    };
    Ok(Some(path))
}

/// Find the first active thread whose ID starts with `prefix`.
///
/// Returns `None` when no thread matches or when `prefix` is empty.
/// Matches are made against active (non-archived) threads only, sorted by
/// `created_at` ascending (oldest first), which means the first match is
/// deterministic for any unique prefix.
pub fn find_thread_by_prefix(prefix: &str) -> Result<Option<ThreadEntry>, ConversationError> {
    if prefix.is_empty() {
        return Ok(None);
    }
    let dir = conversations_dir()?;
    let mut threads = list_threads_in(&dir, false)?;
    // Sort by created_at so the match is stable across index reorders.
    threads.sort_by_key(|t| t.created_at);
    Ok(threads.into_iter().find(|t| t.id.starts_with(prefix)))
}

/// Store a Claude Code session ID on the thread entry so the watcher can
/// resume it across restarts.
///
/// Reads the index, finds the entry, updates `claude_session_id`, and writes
/// back atomically. Returns [`ConversationError::ThreadNotFound`] if the thread
/// does not exist in the index.
pub fn set_thread_session_id(
    thread_id: &str,
    session_id: &str,
) -> Result<(), ConversationError> {
    let dir = conversations_dir()?;
    set_thread_session_id_in(&dir, thread_id, session_id)
}

fn set_thread_session_id_in(
    dir: &Path,
    thread_id: &str,
    session_id: &str,
) -> Result<(), ConversationError> {
    let mut index = read_index(dir)?;
    let entry = index
        .threads
        .iter_mut()
        .find(|e| e.id == thread_id)
        .ok_or_else(|| ConversationError::ThreadNotFound(thread_id.to_string()))?;
    entry.claude_session_id = Some(session_id.to_string());
    write_index(dir, &index)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn read_jsonl(path: &Path) -> Result<Vec<Message>, ConversationError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Skip malformed lines (e.g. partial writes from concurrent access)
        // rather than returning an error that would crash the TUI file poller.
        match serde_json::from_str::<Message>(&line) {
            Ok(msg) => messages.push(msg),
            Err(_) => continue,
        }
    }
    Ok(messages)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a fresh temp directory with the expected subdirectory structure.
    fn make_dir() -> TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        ensure_dirs(tmp.path()).expect("ensure_dirs");
        tmp
    }

    #[test]
    fn create_thread_updates_index() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "main").expect("create_thread");
        assert!(!thread.id.is_empty());
        assert_eq!(thread.agent_id, "main");
        assert!(!thread.archived);

        let threads = list_threads_in(dir, false).expect("list_threads");
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, thread.id);
        assert_eq!(threads[0].agent_id, "main");
        assert_eq!(threads[0].message_count, 0);
        assert!(!threads[0].archived);
    }

    #[test]
    fn append_and_read_messages() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "edi").expect("create_thread");

        append_message_in(dir, &thread.id, MessageRole::User, "Hello agent", None, None)
            .expect("append user");
        append_message_in(
            dir,
            &thread.id,
            MessageRole::Assistant,
            "Hello, how can I help?",
            Some("run-001"),
            None,
        )
        .expect("append assistant");

        let msgs = read_thread_in(dir, &thread.id).expect("read_thread");
        assert_eq!(msgs.len(), 2);

        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content, "Hello agent");
        assert!(msgs[0].run_id.is_none());

        assert_eq!(msgs[1].role, MessageRole::Assistant);
        assert_eq!(msgs[1].content, "Hello, how can I help?");
        assert_eq!(msgs[1].run_id.as_deref(), Some("run-001"));

        // Index message_count updated.
        let threads = list_threads_in(dir, false).expect("list_threads");
        assert_eq!(threads[0].message_count, 2);
    }

    #[test]
    fn read_thread_tail_returns_last_n() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "garrus").expect("create_thread");

        for i in 0..5u32 {
            append_message_in(
                dir,
                &thread.id,
                MessageRole::User,
                &format!("msg {i}"),
                None,
                None,
            )
            .expect("append");
        }

        let all = read_thread_in(dir, &thread.id).expect("read_thread");
        let skip = all.len().saturating_sub(3);
        let tail: Vec<Message> = all.into_iter().skip(skip).collect();

        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].content, "msg 2");
        assert_eq!(tail[1].content, "msg 3");
        assert_eq!(tail[2].content, "msg 4");

        // Requesting more than available returns all.
        let all2 = read_thread_in(dir, &thread.id).expect("read_thread all");
        let skip2 = all2.len().saturating_sub(100);
        let all_tail: Vec<Message> = all2.into_iter().skip(skip2).collect();
        assert_eq!(all_tail.len(), 5);
    }

    #[test]
    fn auto_archive_old_threads() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "liara").expect("create_thread");

        // Backdate updated_at to 49 hours ago.
        let mut index = read_index(dir).expect("read_index");
        let entry = index
            .threads
            .iter_mut()
            .find(|e| e.id == thread.id)
            .expect("entry");
        entry.updated_at = Utc::now() - Duration::hours(49);
        write_index(dir, &index).expect("write_index");

        let archived = auto_archive_in(dir).expect("auto_archive");
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0], thread.id);

        // Should be absent from active list.
        let active = list_threads_in(dir, false).expect("list_threads");
        assert!(active.is_empty());

        // Should appear in all_threads as archived.
        let all = list_threads_in(dir, true).expect("list_all_threads");
        assert_eq!(all.len(), 1);
        assert!(all[0].archived);

        // File moved to archived/.
        assert!(
            archived_path(dir, &thread.id).exists(),
            "archived file should exist"
        );
        assert!(
            !active_path(dir, &thread.id).exists(),
            "active file should be gone"
        );
    }

    #[test]
    fn resume_archived_thread() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "mordin").expect("create_thread");

        // Backdate and archive.
        let mut index = read_index(dir).expect("read_index");
        let entry = index
            .threads
            .iter_mut()
            .find(|e| e.id == thread.id)
            .expect("entry");
        entry.updated_at = Utc::now() - Duration::hours(72);
        write_index(dir, &index).expect("write_index");
        auto_archive_in(dir).expect("auto_archive");

        // Resume.
        resume_thread_in(dir, &thread.id).expect("resume_thread");

        // Should be active again.
        let active = list_threads_in(dir, false).expect("list_threads");
        assert_eq!(active.len(), 1);
        assert!(!active[0].archived);

        // File back in active location.
        assert!(active_path(dir, &thread.id).exists());
        assert!(!archived_path(dir, &thread.id).exists());
    }

    #[test]
    fn read_thread_not_found() {
        let tmp = make_dir();
        let dir = tmp.path();

        let result = read_thread_in(dir, "00000000-0000-0000-0000-000000000000");
        assert!(matches!(result, Err(ConversationError::ThreadNotFound(_))));
    }

    #[test]
    fn auto_archive_skips_recent_threads() {
        let tmp = make_dir();
        let dir = tmp.path();

        // Fresh thread — updated_at is now, well within 48h.
        let _thread = create_thread_in(dir, "tali").expect("create_thread");
        let archived = auto_archive_in(dir).expect("auto_archive");
        assert!(archived.is_empty(), "recent thread should not be archived");
    }

    #[test]
    fn set_session_id_persists_and_roundtrips() {
        let tmp = make_dir();
        let dir = tmp.path();

        let thread = create_thread_in(dir, "agent").expect("create_thread");

        // No session ID by default.
        let threads = list_threads_in(dir, false).expect("list_threads");
        assert!(threads[0].claude_session_id.is_none());

        // Setting it persists.
        set_thread_session_id_in(dir, &thread.id, "sess-abc123").expect("set_session_id");

        let threads2 = list_threads_in(dir, false).expect("list_threads after set");
        assert_eq!(threads2[0].claude_session_id.as_deref(), Some("sess-abc123"));

        // Overwriting works.
        set_thread_session_id_in(dir, &thread.id, "sess-xyz999").expect("set_session_id overwrite");
        let threads3 = list_threads_in(dir, false).expect("list_threads after overwrite");
        assert_eq!(threads3[0].claude_session_id.as_deref(), Some("sess-xyz999"));
    }

    #[test]
    fn set_session_id_not_found() {
        let tmp = make_dir();
        let dir = tmp.path();

        let result =
            set_thread_session_id_in(dir, "00000000-0000-0000-0000-000000000000", "sess");
        assert!(matches!(result, Err(ConversationError::ThreadNotFound(_))));
    }
}
