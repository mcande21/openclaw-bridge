# openclaw-bridge: Setup Guide

This document is for Claude Code agents configuring the OpenClaw bridge after installation. Follow these steps in order with your user. Each step includes verification.

**Prerequisites:** The `ocb` binary must be installed. If not, read [INSTALL.md](INSTALL.md) first.

---

## Step 1: Gather Gateway Information

Ask the user the following questions before proceeding:

1. "What is your OpenClaw gateway host? (Tailscale IP or hostname, e.g. `localhost`)"
2. "What is your gateway auth token? (Found in `~/.openclaw/openclaw.json` → `gateway.auth.token` on your VPS)"

**Security — token handling:** Do NOT ask the user to paste their token in the chat. Instead, instruct them to store it directly in the config file:

Tell the user:
> "Please run this command in your terminal to save your token securely:
> ```bash
> mkdir -p ~/.config/openclaw-bridge
> chmod 700 ~/.config/openclaw-bridge
> echo 'YOUR_TOKEN_HERE' > ~/.config/openclaw-bridge/gateway-token
> chmod 600 ~/.config/openclaw-bridge/gateway-token
> ```
> Replace `YOUR_TOKEN_HERE` with the actual token."

Once the user confirms they've saved the token, set the gateway host:

```bash
# Add to shell profile for persistence
echo 'export OPENCLAW_HOST="<their-host>"' >> ~/.zshrc   # or ~/.bashrc
# Also set in current session
export OPENCLAW_HOST="<their-host>"
```

**Verification:**
```bash
ocb auth status
```
Expected: `"token_status": "present"`. If `"not_found"`, the token file was not created correctly.

---

## Step 2: Pair This Device

```bash
ocb pair
```

This will:
1. Generate a local Ed25519 device identity (first run only — stored at `~/.config/openclaw-bridge/openclaw-device.json`)
2. Attempt to connect to the gateway
3. Return either `"status": "paired"` (already authenticated) or `"status": "pending"` (needs approval)

If the status is `"pending"`, tell the user:

> "Your device needs approval on the OpenClaw gateway. Please run these commands on your VPS:
> ```bash
> openclaw devices list        # Find the pending request — note the request ID
> openclaw devices approve <request-id>
> ```
> Then let me know when you've approved it."

After the user approves, verify the pairing:
```bash
ocb chat --agent main -m "ping"
```

Expected: a JSON response with a `"text"` field containing the agent's reply. The device token is captured automatically — you will not need to do anything else for auth.

---

## Step 3: Verify End-to-End

Run these checks in order:

```bash
# 1. Auth state
ocb auth status
# Expect: identity_status=loaded, token_status=present

# 2. Gateway reachable
ocb status
# Expect: up=true

# 3. Agent communication
ocb chat --agent main -m "Connection test from Claude Code. Confirm receipt."
# Expect: text response from the remote agent

# 4. Session continuity
ocb conversation new --agent main
# Note the thread_id in the response
ocb conversation send --thread <thread-id> -m "Remember this value: 9281"
ocb conversation send --thread <thread-id> -m "What value did I give you?"
# Expect: "9281" in the response
```

If all four checks pass, the bridge is fully operational.

---

## Step 4: Install the /openclaw Skill

Create the Claude Code commands directory and skill file:

```bash
mkdir -p ~/.claude/commands
```

Write the following content to `~/.claude/commands/openclaw.md`:

```markdown
# /openclaw

Connect to your OpenClaw gateway and start a three-party conversation session.

## Steps

1. Check gateway health:
   ```bash
   ocb status
   ```
   If the result shows `"up": false` or returns an error, notify the user and stop. The gateway must be reachable before proceeding.

2. List existing conversation threads:
   ```bash
   ocb conversation list
   ```
   If an active thread exists for the target agent, resume it. Otherwise create a new one:
   ```bash
   ocb conversation new --agent main
   ```
   Note the returned `thread_id`.

3. Send an opening message:
   ```bash
   ocb send <thread-prefix> -m "Connected. Working on: {context}. What's your current state?"
   ```
   Replace `{context}` with a brief description of what you're working on.

4. Tell the user about the TUI (optional):
   > "To watch this conversation live, open another terminal and run:
   > `ocb tui --thread <thread-id>`
   >
   > The TUI shows messages in real-time and lets you type into the conversation.
   > Your messages appear in green, mine in cyan, and the remote agent in magenta."

5. Start monitoring the thread:
   Check for new messages every 30 seconds:
   ```bash
   ocb conversation history <thread-id> --last 3
   ```
   Inspect the most recent messages. Decide whether to respond:
   - If the remote agent addressed you or is waiting for input: respond with `ocb send <thread-prefix> -m "your response"`
   - If the user typed a message in the TUI (source: "tui"): respond appropriately
   - If nothing requires a response: skip (silence is valid)

6. Report ready to the user:
   ```
   OpenClaw session active.

   Gateway: {status from ocb status}
   Thread: {thread-id}
   Agent: {summary of agent's opening response}

   Monitoring thread every 30s. To watch live:
     ocb tui --thread {thread-id}
   ```

## Three-Party Conversations

When the user opens the TUI and types messages:
- Their messages appear as `source: "tui"` in the JSONL (green in TUI)
- Your messages appear as `source: "cli"` (cyan in TUI)
- Remote agent responses appear as `role: "assistant"` (magenta in TUI)

Messages are prefixed on the wire: `[User]` for TUI messages, `[Claude Code]` for CLI messages. The remote agent can distinguish who is talking.

## Commands Reference

| Command | Purpose |
|---------|---------|
| `ocb send <prefix> -m "..."` | Send to a conversation thread by ID prefix |
| `ocb chat --agent <id> -m "..."` | Quick one-off message (no thread) |
| `ocb status` | Gateway health check |
| `ocb agents` | List active agent sessions |
| `ocb conversation list` | List all threads |
| `ocb conversation new --agent <id>` | Create new thread |
| `ocb conversation history <id> --last N` | Read recent messages |
| `ocb spawn --agent <id> -m "..."` | Fire-and-forget task via SSH |
| `ocb tui --thread <id>` | Launch TUI viewer |

## Output Flags

- Default: compact JSON
- `--bare`: raw text only (useful for reading responses directly)
- `--max-chars N`: truncate long responses with a signal
- `--stream`: print streaming deltas to stderr

## When to Use This Skill

- Coordinating with a remote OpenClaw agent on a shared task
- Delegating work to remote specialized agents
- Three-party conversations with the user via TUI
- Checking status of remote infrastructure or agent state
- Any cross-machine AI coordination that requires persistent context
```

**Verification:** Run `/openclaw` in Claude Code. The skill should load and begin the connection sequence.

---

## Setup Complete

Display this to the user after all steps pass:

```
OpenClaw Bridge configured!

Your Claude Code agent can now communicate with your OpenClaw gateway.

Quick start:
- Type /openclaw to start a conversation session
- Your agent will connect, create a thread, and start communicating

What /openclaw does:
- Connects to your OpenClaw gateway
- Creates or resumes a shared conversation thread
- Checks the thread every 30s for new messages
- Responds when addressed or when input is needed

TUI viewer (optional):
- Open a second terminal and run: ocb tui --thread <id>
- Watch the conversation in real-time
  - Green: your messages (typed in TUI)
  - Cyan: Claude Code messages
  - Magenta: remote agent responses

Available commands:
- ocb send <thread-prefix> -m "message"   send to a conversation thread
- ocb chat --agent main -m "message"      quick one-off message
- ocb status                              gateway health check
- ocb agents                              list active sessions
- ocb conversation list                   list all threads
- ocb tui                                 launch the TUI viewer
```

---

## Troubleshooting

| Error | Code | Resolution |
|-------|------|------------|
| Gateway unreachable | `GATEWAY_UNREACHABLE` | Check `OPENCLAW_HOST` and that the gateway is running |
| Token missing | `TOKEN_MISSING` | Write token to `~/.config/openclaw-bridge/gateway-token` |
| Pairing required | `PAIRING_REQUIRED` | Run `ocb pair`, then approve on VPS: `openclaw devices approve <id>` |
| Identity mismatch | `IDENTITY_MISMATCH` | Run `ocb auth reset` then `ocb pair` again |
| SSH error | `SSH_ERROR` | Verify SSH access: `ssh <user>@<host> openclaw --version` |

All errors are JSON on stderr with `error`, `code`, and `hint` fields.
