# openclaw-bridge: Install Guide

This document is for Claude Code agents installing the `ocb` binary. Follow the steps in order.

## Step 1: Install the Binary

### Option A: Pre-Built Binary (recommended)

Download the binary for your platform from
[https://github.com/mcande21/openclaw-bridge/releases](https://github.com/mcande21/openclaw-bridge/releases).

**macOS (universal — runs on both Intel and Apple Silicon):**

```bash
curl -sL https://github.com/mcande21/openclaw-bridge/releases/latest/download/openclaw-bridge-0.1.0-universal-apple-darwin.tar.xz \
  | tar -xJ && sudo mv ocb /usr/local/bin/
```

Check the releases page for the current version and the following platform assets:

| Platform | Asset |
|----------|-------|
| macOS universal | `openclaw-bridge-0.1.0-universal-apple-darwin.tar.xz` |
| macOS Apple Silicon | `openclaw-bridge-aarch64-apple-darwin.tar.xz` |
| macOS Intel | `openclaw-bridge-x86_64-apple-darwin.tar.xz` |
| Linux aarch64 (static) | `openclaw-bridge-aarch64-unknown-linux-musl.tar.xz` |
| Linux x86_64 (static) | `openclaw-bridge-x86_64-unknown-linux-musl.tar.xz` |

**Linux:**

```bash
# Example for x86_64 — substitute aarch64 if needed
curl -sL https://github.com/mcande21/openclaw-bridge/releases/latest/download/openclaw-bridge-0.1.0-x86_64-unknown-linux-musl.tar.xz \
  | tar -xJ && sudo mv ocb /usr/local/bin/
```

### Option B: From Source (requires Rust toolchain)

```bash
# CLI only (chat, send, status, agents, conversations)
cargo install --git https://github.com/mcande21/openclaw-bridge --features cli

# CLI + TUI viewer (recommended — includes the terminal conversation viewer)
cargo install --git https://github.com/mcande21/openclaw-bridge --features tui
```

The `tui` feature includes everything in `cli` plus the `ocb tui` command for watching conversations in real-time. Recommended for three-party sessions.

This compiles and installs `ocb` to `~/.cargo/bin/`. The build takes 1-2 minutes on first run.

## Step 2: Verify Install

```bash
ocb version
```

Expected output (exact values will vary):
```json
{"name":"ocb","version":"0.1.0","description":"CLI bridge connecting Claude Code to OpenClaw gateways"}
```

If the command is not found, check that `/usr/local/bin` (Option A) or `~/.cargo/bin` (Option B) is in your `PATH`.

## Next Step

Binary installed. Now read [SETUP.md](SETUP.md) to configure the gateway connection.
