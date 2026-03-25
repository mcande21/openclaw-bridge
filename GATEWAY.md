# OpenClaw Gateway Setup

An OpenClaw gateway is a WebSocket server that runs AI agent sessions on a remote machine. `ocb` connects to it — the gateway is the other half. This guide gets one running on a VPS.

---

## Prerequisites

- VPS running Ubuntu 24.04 (2+ GB RAM recommended)
- Node.js 24 (or 22.14+) installed on the VPS
- Tailscale on both the VPS and your local machine (see [SETUP.md Step 0](SETUP.md#step-0-set-up-tailscale-recommended))
- An API key for at least one model provider (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.)

---

## Install OpenClaw

SSH into your VPS, then:

```bash
npm install -g openclaw@latest
```

Verify the install:

```bash
openclaw --version
```

**Docker alternative:** For production deployments, the official image is `ghcr.io/openclaw/openclaw:latest`. See [docs.openclaw.ai](https://docs.openclaw.ai) for container configuration.

---

## Quick Setup (Interactive)

```bash
openclaw onboard --install-daemon
```

This walks you through model auth, generates a gateway token, and installs a systemd (Linux) or launchd (macOS) service. It's the fastest path.

After it completes, jump to [Verify](#verify).

---

## Manual Setup

For explicit control over config and token.

### 1. Generate a gateway token

```bash
openssl rand -hex 32
```

Save the output — this is your gateway token. You'll need it on both the VPS and your local machine.

### 2. Create the config

```bash
mkdir -p ~/.openclaw
```

Write `~/.openclaw/openclaw.json`:

```json
{
  "gateway": {
    "bind": "loopback",
    "auth": {
      "token": "YOUR_TOKEN_HERE"
    }
  }
}
```

Replace `YOUR_TOKEN_HERE` with the token from step 1.

`"bind": "loopback"` keeps the gateway on `127.0.0.1`. Tailscale handles the secure tunnel — no need to expose the port publicly.

### 3. Set API keys

```bash
cat >> ~/.openclaw/.env <<'EOF'
ANTHROPIC_API_KEY=sk-ant-...
EOF
```

Add whichever providers you need. The gateway picks these up on start.

### 4. Start the gateway

```bash
openclaw gateway --bind loopback
```

---

## Verify

```bash
curl -fsS http://127.0.0.1:18789/healthz
```

Expected response: `{"status":"ok"}` (or similar). If the command hangs or returns an error, the gateway isn't running — check `journalctl -u openclaw` or the process output.

---

## Connect ocb

With the gateway running, configure your local machine:

1. Set `OPENCLAW_HOST` to your VPS's Tailscale IP (starts with `100.`):

   ```bash
   export OPENCLAW_HOST="100.x.x.x"
   ```

2. Store the gateway token locally:

   ```bash
   mkdir -p ~/.config/openclaw-bridge
   chmod 700 ~/.config/openclaw-bridge
   echo 'YOUR_TOKEN_HERE' > ~/.config/openclaw-bridge/gateway-token
   chmod 600 ~/.config/openclaw-bridge/gateway-token
   ```

3. Pair this device:

   ```bash
   ocb pair
   ```

   If the response shows `"status": "pending"`, approve it on the VPS:

   ```bash
   openclaw devices approve --latest
   ```

See [SETUP.md](SETUP.md) for the full client setup walkthrough.

---

## Running as a Service

If you used `openclaw onboard --install-daemon`, the service is already installed. To install it manually:

```bash
openclaw daemon install
```

On Linux this creates a systemd unit:

```bash
systemctl --user enable --now openclaw
systemctl --user status openclaw
```

On macOS it creates a launchd plist:

```bash
launchctl list | grep openclaw
```

The daemon starts on boot and restarts on crash.

---

For advanced topics — sandboxing, plugins, full config reference — see [docs.openclaw.ai](https://docs.openclaw.ai).
