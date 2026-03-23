# openclaw-bridge: Install Guide

This document is for Claude Code agents installing the `ocb` binary. Follow these steps in order. Each step includes a verification check — do not proceed until verification passes.

## Step 1: Check Prerequisites

```bash
# Check if cargo is available
which cargo
cargo --version
```

If cargo is not available, proceed to Option B (pre-built binary). If cargo is available, use Option A.

## Step 2: Install the Binary

### Option A: From Source (cargo)

```bash
cargo install --git https://github.com/mcande21/openclaw-bridge --features cli
```

This compiles and installs `ocb` to `~/.cargo/bin/`. The build takes 1-2 minutes on first run.

### Option B: Pre-Built Binary

```bash
# Detect platform
PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

# Download from GitHub releases
# Check https://github.com/mcande21/openclaw-bridge/releases for the latest version
curl -sSL "https://github.com/mcande21/openclaw-bridge/releases/latest/download/ocb-${PLATFORM}-${ARCH}" \
  -o ~/.local/bin/ocb
chmod +x ~/.local/bin/ocb
```

## Step 3: Verify Install

```bash
ocb version
```

Expected output (exact values will vary):
```json
{"name":"ocb","version":"0.1.0","description":"CLI bridge connecting Claude Code to OpenClaw gateways"}
```

If the command is not found, check that `~/.cargo/bin` (Option A) or `~/.local/bin` (Option B) is in your `PATH`.

## Next Step

Binary installed. Now read [SETUP.md](SETUP.md) to configure the gateway connection.
