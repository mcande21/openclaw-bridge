# Install

Three ways to install `ocb`. Pick the one that fits your situation.

---

## Option 1: Homebrew (macOS, easiest)

```bash
brew tap mcande21/tap && brew install openclaw-bridge
```

This installs the full binary (CLI + TUI + MCP) and keeps it up to date with `brew upgrade`.

---

## Option 2: Pre-built Binary

Download from [GitHub Releases](https://github.com/mcande21/openclaw-bridge/releases).

Available platforms:

| Platform | Asset |
|----------|-------|
| macOS universal (Intel + Apple Silicon) | `openclaw-bridge-universal-apple-darwin.tar.xz` |
| macOS Apple Silicon | `openclaw-bridge-aarch64-apple-darwin.tar.xz` |
| macOS Intel | `openclaw-bridge-x86_64-apple-darwin.tar.xz` |
| Linux aarch64 (static musl) | `openclaw-bridge-aarch64-unknown-linux-musl.tar.xz` |
| Linux x86_64 (static musl) | `openclaw-bridge-x86_64-unknown-linux-musl.tar.xz` |

**macOS (universal):**

```bash
curl -sL https://github.com/mcande21/openclaw-bridge/releases/latest/download/openclaw-bridge-universal-apple-darwin.tar.xz \
  | tar -xJ && sudo mv ocb /usr/local/bin/
```

**Linux x86_64:**

```bash
curl -sL https://github.com/mcande21/openclaw-bridge/releases/latest/download/openclaw-bridge-x86_64-unknown-linux-musl.tar.xz \
  | tar -xJ && sudo mv ocb /usr/local/bin/
```

Substitute `aarch64` in the URL for ARM Linux.

> **Note:** v0.2.0 binaries include CLI, TUI, and MCP. v0.1.0 binaries include CLI and TUI only — `ocb mcp` is not available in v0.1.0.

---

## Option 3: Build from Source (contributors)

Requires a Rust toolchain. Install via [rustup](https://rustup.rs/) if needed.

```bash
git clone https://github.com/mcande21/openclaw-bridge
cd openclaw-bridge

# Full build (CLI + TUI + MCP — recommended)
cargo install --path . --features cli,tui,mcp

# CLI only
cargo install --path . --features cli

# CLI + TUI (no MCP)
cargo install --path . --features cli,tui
```

First build takes 1-3 minutes. The binary installs to `~/.cargo/bin/`.

**Feature flags:**

| Feature | Adds |
|---------|------|
| `cli` | Core commands: `chat`, `send`, `spawn`, `status`, `conversation`, `workspace` |
| `tui` | Interactive terminal UI (`ocb tui`) |
| `mcp` | MCP channel server (`ocb mcp`) for Claude Code integration |

---

## Verify

```bash
ocb --version
```

Expected output:

```json
{"name":"ocb","version":"0.2.1","description":"CLI bridge connecting Claude Code to OpenClaw gateways"}
```

If the command is not found:
- Homebrew / binary: check that `/usr/local/bin` is in `PATH`
- Source build: check that `~/.cargo/bin` is in `PATH`

---

Next: [SETUP.md](SETUP.md)
