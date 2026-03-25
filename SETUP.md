# Setup

Post-install configuration. Gets you from a fresh `ocb` binary to a working gateway connection.

**Prerequisites:** `ocb` installed and on your PATH. If not, see [INSTALL.md](INSTALL.md).

---

## Step 1: Set OPENCLAW_HOST

```bash
export OPENCLAW_HOST="your-gateway-host"
```

This is your OpenClaw gateway's Tailscale IP, hostname, or domain. Add it to your shell profile for persistence:

```bash
echo 'export OPENCLAW_HOST="your-gateway-host"' >> ~/.zshrc   # or ~/.bashrc
```

If your SSH commands use a custom host alias, add a stanza to `~/.ssh/config`:

```
Host openclaw
    HostName your-gateway-host
    User your-user
```

---

## Step 2: Store Your Gateway Token

Your gateway auth token lives at `~/.config/openclaw-bridge/gateway-token`. Write it directly — do not pass it through env vars or chat.

```bash
mkdir -p ~/.config/openclaw-bridge
chmod 700 ~/.config/openclaw-bridge
echo 'YOUR_TOKEN_HERE' > ~/.config/openclaw-bridge/gateway-token
chmod 600 ~/.config/openclaw-bridge/gateway-token
```

Replace `YOUR_TOKEN_HERE` with the token from your gateway config (`~/.openclaw/openclaw.json` → `gateway.auth.token` on the VPS).

**Verify:**

```bash
ocb auth
```

Expected: `"token_status": "present"`. If you see `"not_found"`, the token file path or permissions are wrong.

---

## Step 3: Pair This Device

```bash
ocb pair
```

This generates a local Ed25519 key pair (first run only) and registers the device with the gateway. You'll get back either:

- `"status": "paired"` — already done, proceed
- `"status": "pending"` — needs approval on the VPS

If pending, run this on your VPS:

```bash
openclaw devices list          # find the pending request ID
openclaw devices approve <id>
```

After approval, confirm the pairing worked:

```bash
ocb chat --agent main -m "ping"
```

Expected: a JSON response with a `"text"` field. The device token is captured automatically.

---

## Step 4: Verify the Connection

```bash
# Gateway reachable
ocb status

# Full end-to-end test
ocb conversation new --agent main
# Note the thread_id
ocb send <thread-prefix> -m "Remember this value: 9281"
ocb send <thread-prefix> -m "What value did I give you?"
# Expected response: "9281"
```

If all checks pass, the bridge is operational.

---

## Optional: MCP Channel Server

Connect `ocb` to Claude Code as an MCP server. This lets Aria's responses arrive as `<channel>` events inside your Claude Code session.

**Register once:**

```bash
claude mcp add -s user ocb -- ocb mcp
```

**Start a session with the channel active:**

```bash
claude --dangerously-load-development-channels server:ocb
```

This is a research preview — the `--dangerously-load-development-channels` flag is required.

> **Note:** Requires v0.2.0 or a source build with `--features mcp`. Not available in v0.1.0 binaries.

---

## Optional: Shell Aliases

Shorthand for common patterns:

```bash
# Add to ~/.zshrc or ~/.bashrc

# Quick alias for the claude-bridge workflow
alias claude-bridge='claude --dangerously-load-development-channels server:ocb'

# Send to the most recently active thread
alias ocb-send='ocb send'
```

---

## Troubleshooting

| Error | Code | Resolution |
|-------|------|------------|
| Gateway unreachable | `GATEWAY_UNREACHABLE` | Check `OPENCLAW_HOST` and confirm the gateway is running |
| Token missing | `TOKEN_MISSING` | Write token to `~/.config/openclaw-bridge/gateway-token` |
| Pairing required | `PAIRING_REQUIRED` | Run `ocb pair`, then approve on VPS: `openclaw devices approve <id>` |
| Identity mismatch | `IDENTITY_MISMATCH` | Run `ocb auth reset` then `ocb pair` again |
| SSH error | `SSH_ERROR` | Verify SSH access: `ssh openclaw openclaw --version` |

All errors are JSON on stderr with `error`, `code`, and `hint` fields.
