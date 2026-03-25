# openclaw-bridge

CLI bridge connecting local [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions to [OpenClaw](https://openclaw.ai) gateways. Binary name: `ocb`.

`ocb` connects your local Claude Code sessions to an [OpenClaw](https://openclaw.ai) gateway running on a remote server. This lets you coordinate AI agents across machines — run an agent on a VPS that persists 24/7, and talk to it from your laptop through your terminal, Claude Code, or both at once.

```
Your Machine                          Your VPS
+------------------+   WebSocket    +--------------+
|   Claude Code    |<-------------->|   OpenClaw   |
|                  |   ocb bridge   |   Gateway    |
|                  |   Ed25519 auth |              |
+------------------+   session_key  +--------------+
```

## Prerequisites

- [Tailscale](https://tailscale.com) installed on both your local machine and gateway server (recommended for secure, zero-config networking)
- An [OpenClaw](https://openclaw.ai) gateway running on your server ([setup guide](GATEWAY.md))
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (for MCP integration)

## Features

**CLI mode** — short-lived commands for one-shot messages, conversation management, and gateway operations. Compact JSON output by default, designed for agent consumption.

**TUI mode** — interactive ratatui terminal UI for real-time conversations. Watch messages arrive live, type responses, and coordinate three-party sessions (you + Claude Code + remote agent) in a single thread.

**MCP Channel Server** — long-lived MCP server that pushes OpenClaw messages directly into Claude Code sessions as `<channel>` events. Exposes `reply` and `channel_history` tools. Research preview.

## Quick Start

```bash
# Install (macOS)
brew tap mcande21/tap && brew install openclaw-bridge

# Configure
export OPENCLAW_HOST="your-gateway-host"
ocb pair
# Approve on VPS: openclaw devices approve <id>

# Verify
ocb status

# Send a message
ocb chat --agent main -m "What's the status?"
```

See [INSTALL.md](INSTALL.md) and [SETUP.md](SETUP.md) for full setup instructions.

## Installation

| Method | Command |
|--------|---------|
| Homebrew (macOS) | `brew tap mcande21/tap && brew install openclaw-bridge` |
| Pre-built binary | Download from [GitHub Releases](https://github.com/mcande21/openclaw-bridge/releases) |
| Source (contributors) | `cargo install --path . --features cli,tui,mcp` |

See [INSTALL.md](INSTALL.md) for platform-specific binary instructions and feature flags.

## Usage

### CLI

```bash
# One-shot message
ocb chat --agent main -m "Summarize current state"

# Persistent conversation thread
ocb conversation new --agent main
ocb send <thread-prefix> -m "Remember the value 42"
ocb send <thread-prefix> -m "What value did I give you?"

# Fire-and-forget task dispatch
ocb spawn --agent main --task "Run the nightly report"

# Gateway health
ocb status
ocb agents
```

### TUI

```bash
# Open interactive terminal UI on a thread
ocb tui --thread <thread-id>
```

Messages are color-coded: green (you), cyan (Claude Code), magenta (remote agent).

### Watch mode

```bash
# Stream incoming messages on a thread
ocb watch --thread <thread-id> --session <session-id>
```

## MCP Channel Server

`ocb mcp` starts a long-lived MCP server that bridges Claude Code to OpenClaw. Each Claude Code session gets a fresh conversation thread. Aria's responses arrive as `<channel>` events inside your session.

**Register with Claude Code:**

```bash
claude mcp add -s user ocb -- ocb mcp
```

**Activate in a session:**

```bash
claude --dangerously-load-development-channels server:ocb
```

This is a research preview. The `--dangerously-load-development-channels` flag is required.

> **Note:** MCP support ships in v0.2.0. Pre-built binaries from v0.1.0 do not include `ocb mcp`.

## Commands

| Command | Purpose | Transport |
|---------|---------|-----------|
| `ocb chat --agent <id> -m "..."` | One-shot message, wait for response | WebSocket |
| `ocb send <thread-prefix> -m "..."` | Send to thread by prefix | WS + Local |
| `ocb spawn --agent <id> --task "..."` | Fire-and-forget agent dispatch | SSH |
| `ocb watch --thread <id> --session <id>` | Stream incoming messages | WebSocket |
| `ocb status` | Gateway health check | SSH |
| `ocb agents` | List active agent sessions | SSH |
| `ocb conversation new --agent <id>` | Create conversation thread | Local |
| `ocb conversation send --thread <id> -m "..."` | Send to thread by ID | WS + Local |
| `ocb conversation history --thread <id>` | Read thread history | Local |
| `ocb conversation list` | List all threads | Local |
| `ocb workspace list <agent>` | List agent workspace files | SSH |
| `ocb workspace read <agent> <file>` | Read workspace file | SSH |
| `ocb tui --thread <id>` | Interactive terminal UI | WebSocket |
| `ocb mcp` | Start MCP channel server | WebSocket |
| `ocb pair` | Pair device with gateway | WebSocket |
| `ocb auth` | Device auth status | Local |

## Configuration

**Required:**

| Variable | Purpose |
|----------|---------|
| `OPENCLAW_HOST` | Gateway host (Tailscale IP, hostname, or domain) |

**Optional:**

| Variable | Purpose | Default |
|----------|---------|---------|
| `OPENCLAW_PORT` | Gateway WebSocket port | `18789` |
| `OPENCLAW_TOKEN` | Gateway auth token (alternative to token file) | See `~/.config/openclaw-bridge/gateway-token` |
| `OPENCLAW_WS_HOST` | WebSocket host override | `OPENCLAW_HOST` |
| `OPENCLAW_SSH_HOST` | SSH host override | `openclaw` |

> **Security note:** `ocb` connects over `ws://` (plaintext WebSocket). Tailscale encrypts all traffic between your devices automatically — no TLS configuration needed. If you're not using Tailscale, do not expose your gateway to the public internet.

**Config directory:**

```
~/.config/openclaw-bridge/
├── gateway-token              # Gateway auth token (0600)
├── openclaw-device.json       # Ed25519 device identity (auto-generated)
├── openclaw-device-auth.json  # Device token issued after pairing
└── conversations/
    ├── threads.json           # Thread index
    └── <uuid>.jsonl           # Conversation messages
```

**Output flags** (all commands):

| Flag | Effect |
|------|--------|
| `--pretty` | Indented JSON for human reading |
| `--bare` | Raw text only (no JSON envelope) |
| `--max-chars N` | Truncate response text to N characters |
| `--full` | Complete unfiltered output |
| `--stream` | Print streaming deltas to stderr |
| `-v` | Verbose diagnostic output |

## License

MIT
