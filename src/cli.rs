//! Clap CLI definitions for the `ocb` binary.
//!
//! All subcommands are designed for non-interactive use by Claude Code agents.
//! Output is JSON on stdout; errors are JSON on stderr.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ocb",
    about = "OpenClaw Bridge — connect Claude Code to OpenClaw gateways",
    version
)]
pub struct Cli {
    /// Enable verbose diagnostic output (suppressed by default for clean JSON output)
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Pretty-print JSON output (default: compact for AI consumption)
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Return full unfiltered output (default: summary projection)
    #[arg(long, global = true)]
    pub full: bool,

    /// Return bare text (no JSON envelope) for chat responses
    #[arg(long, global = true)]
    pub bare: bool,

    /// Maximum characters in response text (truncate with signal)
    #[arg(long, global = true)]
    pub max_chars: Option<usize>,

    /// Stream agent response deltas to stderr as they arrive
    #[arg(long, global = true)]
    pub stream: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Send a message to an agent and wait for the response
    Chat {
        /// Agent ID to send the message to
        #[arg(long)]
        agent: String,

        /// Message to send
        #[arg(short = 'm', long)]
        message: String,

        /// Session key for conversational context (e.g. "ocb:<thread-id>")
        #[arg(long)]
        session: Option<String>,

        /// Timeout in seconds (default: 120)
        #[arg(long, default_value = "120")]
        timeout: u64,
    },

    /// Spawn an agent with a task via SSH (fire-and-forget)
    Spawn {
        /// Agent ID to spawn
        #[arg(long)]
        agent: String,

        /// Task message
        #[arg(short = 'm', long)]
        message: String,
    },

    /// Check gateway status
    Status,

    /// List active agent sessions
    Agents,

    /// Workspace file operations
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },

    /// Conversation thread operations
    Conversation {
        #[command(subcommand)]
        command: ConversationCommand,
    },

    /// Pair this device with an OpenClaw gateway
    Pair {
        /// Gateway WebSocket URL (overrides OPENCLAW_HOST/OPENCLAW_PORT)
        #[arg(long)]
        gateway: Option<String>,
    },

    /// Device auth status and management
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Send a message to a conversation thread (shorthand for `conversation send`)
    ///
    /// The THREAD argument accepts a prefix — it matches the first thread whose
    /// ID starts with that string.  Example: `ocb send f10ec -m "hello"`
    Send {
        /// Thread ID or prefix (matched against the first thread starting with this value)
        thread: String,

        /// Message to send
        #[arg(short = 'm', long)]
        message: String,

        /// Response timeout in seconds (default: 120)
        #[arg(long, default_value = "120")]
        timeout: u64,
    },

    /// Watch a conversation thread and trigger Claude Code responses
    Watch {
        /// Thread ID to watch
        #[arg(long)]
        thread: String,
        /// Claude Code session ID to resume
        #[arg(long)]
        session: String,
        /// Seconds to wait between triggers (default: 5)
        #[arg(long, default_value = "5")]
        debounce: u64,
        /// Number of recent messages to include in the prompt (default: 5)
        #[arg(long, default_value = "5")]
        context: usize,
        /// Maximum seconds to wait for a claude process before abandoning it (default: 300)
        #[arg(long, default_value = "300")]
        timeout: u64,
    },

    /// Show version information
    Version,

    /// Launch the conversation TUI viewer
    #[cfg(feature = "tui")]
    Tui {
        /// Resume a specific conversation thread
        #[arg(long)]
        thread: Option<String>,
    },

    /// Start the MCP channel server (JSON-RPC 2.0 over stdio)
    ///
    /// Implements the Model Context Protocol so Claude Code can use `channel_history`
    /// and `reply` tools to interact with the Aria conversation thread.
    ///
    /// Thread resolution: OCB_MCP_THREAD > most recent thread for OCB_MCP_AGENT (default: main)
    #[cfg(feature = "mcp")]
    Mcp,
}

#[derive(Subcommand)]
pub enum WorkspaceCommand {
    /// List files in an agent's workspace
    List {
        /// Agent ID whose workspace to list
        #[arg(long)]
        agent: String,
    },

    /// Read a file from an agent's workspace
    Read {
        /// Agent ID whose workspace to read from
        #[arg(long)]
        agent: String,

        /// Filename to read
        filename: String,
    },
}

#[derive(Subcommand)]
pub enum ConversationCommand {
    /// Create a new conversation thread
    New {
        /// Agent ID to create the thread for
        #[arg(long)]
        agent: String,
    },

    /// Send a message in a conversation thread
    Send {
        /// Thread ID to send to (mutually exclusive with --agent)
        #[arg(long, conflicts_with = "agent")]
        thread: Option<String>,

        /// Agent ID — creates or resumes a thread (mutually exclusive with --thread)
        #[arg(long, conflicts_with = "thread")]
        agent: Option<String>,

        /// Message to send
        #[arg(short = 'm', long)]
        message: String,

        /// Response timeout in seconds (default: 120)
        #[arg(long, default_value = "120")]
        timeout: u64,
    },

    /// Show message history for a thread
    History {
        /// Thread ID
        thread_id: String,
        /// Number of recent messages to show (default: 10, 0 for all)
        #[arg(long, default_value = "10")]
        last: usize,
    },

    /// List all conversation threads
    List,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Show device identity and token state
    Status,

    /// Delete identity and token files (forces re-pairing)
    Reset,
}
