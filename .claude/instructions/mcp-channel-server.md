# MCP Channel Server Design

> Converged from Shepard-Garrus /converse (3 rounds, 2026-03-24).
> Informed by Liara's channel architecture deep dive.

## Overview

`ocb mcp` — a long-lived MCP server mode that bridges Claude Code to OpenClaw
via the experimental `claude/channel` protocol. Replaces polling-based communication
(`/loop`, `ocb watch`) with event-driven push notifications.

## Architecture

```
Aria (OpenClaw WS) ──► ocb mcp (stdio) ──► Claude Code session
                   ◄── reply tool ◄──────
```

- Claude Code spawns `ocb mcp` as a subprocess
- Communicates over stdio (line-delimited JSON-RPC 2.0)
- MCP server maintains persistent WebSocket to OpenClaw gateway
- Unsolicited messages from Aria push into session as `<channel>` events
- Shepard replies via MCP `reply` tool

## Protocol

### Capability Declaration

```json
{
  "capabilities": {
    "experimental": { "claude/channel": {} },
    "tools": {}
  },
  "serverInfo": { "name": "ocb", "version": "0.1.0" }
}
```

### Push Notifications (Server → Claude Code)

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "content": "message text",
    "meta": { "user": "aria", "agent": "main", "ts": "2026-03-24T10:00:00Z" }
  }
}
```

Claude sees: `<channel source="ocb" user="aria" agent="main">message text</channel>`

### Tools

| Tool | Input | Description |
|------|-------|-------------|
| `reply` | `message: string` | Send to Aria via WS, return her response |
| `channel_history` | `last: integer (default 10)` | Read recent messages from JSONL |

## Module Structure

```
src/mcp/
├── mod.rs        — server core, select loop, lifecycle
├── transport.rs  — stdin/stdout line-delimited JSON-RPC
└── tools.rs      — reply + channel_history handlers
```

Feature gate: `mcp` in Cargo.toml (depends on `cli`)

## Select Loop

```rust
tokio::select! {
    // Branch 1: MCP request from Claude Code
    line = stdin_reader.read_line() => { /* parse JSON-RPC, dispatch */ }

    // Branch 2: WS frame from OpenClaw
    frame = ws_read.next() => {
        // If matches pending reply: buffer deltas, emit response on completion
        // If unsolicited: emit notifications/claude/channel + persist JSONL
    }

    // Branch 3: Shutdown
    _ = shutdown => { break }
}
```

## Configuration

- `OPENCLAW_HOST` / `OPENCLAW_WS_HOST` — gateway address (from env/zshrc)
- `OPENCLAW_TOKEN` — gateway auth (from env or config file)
- `OCB_MCP_THREAD` — pin to specific thread (optional, auto-resolves)
- `OCB_MCP_AGENT` — target agent (default: `main`)
- Thread auto-resolve: most recent active thread for target agent

## Registration

```bash
claude mcp add --scope user ocb -- ocb mcp
```

During research preview:
```bash
claude --dangerously-load-development-channels server:ocb
```

## Implementation Phases

### Phase 1: MCP Skeleton + channel_history
- Feature gate, module structure, JSON-RPC handshake
- `channel_history` tool (read-only, no WS)
- Validates protocol works with Claude Code

### Phase 2: WS Bridge + reply + push notifications
- Connect to OpenClaw after `initialized`
- `reply` tool with pending request tracking
- Unsolicited messages as channel notifications
- JSONL persistence for all events
- WS reconnection with backoff

### Phase 3: Polish
- `channel_status` tool
- Thread management
- Connection health reporting

## Key Design Decisions

- **No Discord in v1** — MCP channel solves Shepard↔Aria. Cooper uses Anthropic Discord plugin.
- **MCP and CLI coexist** — separate execution modes, no shared WS connection
- **JSONL stays** — channel events are ephemeral in context; JSONL provides durable history
- **Raw serde_json** — no MCP SDK crate dep. JSON-RPC is simple enough to hand-roll.
- **One reply at a time** — pending request tracking, no concurrent replies
- **stdout is sacred** — only JSON-RPC on stdout, all diagnostics to stderr

## Gateway Behavior (Confirmed with Aria)

The OpenClaw gateway broadcasts to all connected WS clients on the same thread.
The MCP server will see Cooper's messages, Aria's responses, and any agent activity —
all as push events. Three-party visibility is native.
