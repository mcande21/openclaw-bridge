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
   Check for new messages every 10 seconds:
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

   Monitoring thread every 10s. To watch live:
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
