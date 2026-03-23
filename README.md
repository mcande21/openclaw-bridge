# openclaw-bridge

A CLI bridge that connects [Claude Code](https://docs.anthropic.com/en/docs/claude-code) to [OpenClaw](https://openclaw.ai) gateways. Your local AI agent communicates with your remote AI gateway over a persistent, authenticated WebSocket connection.

## What It Does

`openclaw-bridge` (binary: `ocb`) handles device authentication (Ed25519), session continuity, and conversation persistence so Claude Code agents can have stateful back-and-forth conversations with a remote OpenClaw gateway.

```
Your Machine                          Your VPS
+------------------+   WebSocket    +--------------+
|   Claude Code    |<-------------->|   OpenClaw   |
|                  |   ocb bridge   |   Gateway    |
|                  |   Ed25519 auth |              |
|                  |   session_key  |              |
+------------------+                +--------------+
```

Designed for AI agent consumption — all output is compact JSON unless otherwise specified.

**AI Agents:** To set up this bridge, read [INSTALL.md](INSTALL.md).

## Key Features

- **Device pairing** — Ed25519 key pair generated locally on first use, paired with your gateway once
- **Session continuity** — `session_key` maintains agent context across separate WebSocket connections
- **Conversation threads** — JSONL persistence for full conversation history on both sides
- **Three-party conversations** — human (TUI), Claude Code (CLI), and remote agent all in one thread
- **TUI viewer** — optional terminal UI for watching conversations in real-time
- **Streaming** — live response deltas via WebSocket events
- **SSH + WebSocket** — gateway status and agent spawning via SSH; chat via WebSocket

## Install

### From source (requires Rust toolchain)

```bash
cargo install --git https://github.com/mcande21/openclaw-bridge --features cli
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/mcande21/openclaw-bridge/releases) for your platform.

## Quick Start

```bash
# Set your gateway token
export OPENCLAW_TOKEN="your-gateway-auth-token"
export OPENCLAW_HOST="your-gateway-host"

# Pair this device (first time only)
ocb pair
# Approve on your VPS: openclaw devices approve <id>

# Send a message
ocb chat --agent main -m "What's the status?"

# Create a persistent conversation thread
ocb conversation new --agent main
ocb conversation send --thread <id> -m "Remember the value 42"
ocb conversation send --thread <id> -m "What value did I give you?"
# -> "42."

# Check gateway health
ocb status

# Watch TUI (optional, second terminal)
ocb tui
```

## Commands

| Command | Purpose | Transport |
|---------|---------|-----------|
| `ocb chat --agent <id> -m "..."` | Send message, get response | WebSocket |
| `ocb spawn --agent <id> -m "..."` | Dispatch autonomous task | SSH |
| `ocb status` | Gateway health | SSH |
| `ocb agents` | List active sessions | SSH |
| `ocb conversation new --agent <id>` | Create conversation thread | Local |
| `ocb conversation send -m "..."` | Chat with persistence | WS + Local |
| `ocb conversation history <id>` | Read message history | Local |
| `ocb conversation list` | List all threads | Local |
| `ocb send <thread-prefix> -m "..."` | Send by thread prefix | WS + Local |
| `ocb pair` | Pair device with gateway | WebSocket |
| `ocb auth status` | Show identity and token state | Local |
| `ocb auth reset` | Clear identity and re-generate | Local |
| `ocb tui` | Launch TUI viewer | WebSocket |
| `ocb version` | Show version | Local |

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `OPENCLAW_TOKEN` | Gateway auth token | Required |
| `OPENCLAW_HOST` | Gateway host | `localhost` (set to your gateway's IP or hostname for remote connections) |
| `OPENCLAW_PORT` | Gateway WebSocket port | `18789` |
| `OPENCLAW_WS_HOST` | WS-specific host override | `OPENCLAW_HOST` |
| `OPENCLAW_SSH_HOST` | SSH host for SSH commands | `openclaw` |

## Output Flags

All commands produce compact JSON by default (minimal context window usage).

| Flag | Effect |
|------|--------|
| `--pretty` | Indented JSON for human reading |
| `--bare` | Raw text only (no JSON envelope) |
| `--max-chars N` | Truncate response text to N characters |
| `--full` | Complete unfiltered output |
| `--stream` | Print streaming deltas to stderr as they arrive |
| `-v` | Verbose diagnostic output |

## Configuration Files

```
~/.config/openclaw-bridge/
├── gateway-token              # Gateway auth token (user-provided, 0600)
├── openclaw-device.json       # Ed25519 device identity (generated locally)
├── openclaw-device-auth.json  # Device token from gateway after pairing
└── conversations/
    ├── threads.json           # Thread index
    └── <uuid>.jsonl           # Conversation messages
```

## How It Works

1. **Device identity** — First run generates an Ed25519 key pair. Device ID = SHA-256(public_key). Stored at `~/.config/openclaw-bridge/openclaw-device.json`.

2. **Pairing** — `ocb pair` sends the public key to the gateway. An admin approves the request on the VPS. The gateway issues a device token, which `ocb` captures automatically for future connections.

3. **Session continuity** — Conversation threads get a `session_key` (`ocb:<thread-uuid>`) sent on every WebSocket call. The gateway maintains agent context server-side so conversations resume naturally.

4. **Three-party conversations** — The TUI lets the human operator type messages while Claude Code is also connected. Messages are prefixed with `[User]` (TUI) or `[Claude Code]` (CLI) so the remote agent can distinguish who is talking.

## License

MIT
