# Session Recovery

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/session-recovery.md)

With the cosh-core adapter, `cosh` can resume an Agent conversation saved for the current workspace. Recovery restores the messages available to the model; it does not restore terminal processes, old terminal output, approval cards, or other transient UI state.

## Resume a session

Open the picker or select a known session UUID:

```bash
cosh --resume
cosh --resume 2d711642-b726-4b04-8d2a-8a0470f4ed24
```

You can also manage sessions from the prompt:

| Command | Use |
|---|---|
| `/session` | Open the current workspace's session picker. |
| `/session list` | List a bounded page with complete session UUIDs. |
| `/session list --all` | List sessions from every workspace under the same storage root. |
| `/session resume <id>` | Select one session by UUID. |
| `/session new` (`/new`) | Start a new Agent conversation without deleting the old record. |
| `/session status` | Show the selected and active session state. |
| `/session clear <id>...` | Confirm and clear the listed sessions. |
| `/session clear --all` | Confirm and clear all clearable sessions. |

Selecting a session does not call the model. Recovery starts with the next Agent request. If recovery fails, the shell remains usable; refresh the list, retry, or start a new session.

## Workspace and safety boundaries

- A session belongs to the canonical workspace where it was created. `/session list --all` can show sessions from other workspaces, but `resume` refuses a scope mismatch and never changes your working directory.
- Only healthy, current-workspace entries can be resumed. Damaged or incompatible entries can be identified and cleared after confirmation.
- Clear operations always confirm the exact IDs or count. The selected session and active provider session are protected and are skipped by clear-all requests.
- The default persistence root is `~/.copilot-shell/cosh-core/sessions/`. Set `session.persist_dir` to change it or `session.auto_persist = false` to keep sessions only for the current `cosh` process.

In the picker, use `Up`/`Down` or `j`/`k` to move, `Enter` to resume, `Space` to mark entries, `d` then `y` to confirm clearing, and `Esc` or `Ctrl+C` to cancel.
