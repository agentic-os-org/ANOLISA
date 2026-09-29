# Workspace Checkpoints

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/checkpoint.md)

`cosh-cli checkpoint` asks the ws-ckpt daemon to save, compare, restore, and clean workspace snapshots. Use a snapshot before a high-risk change so a failed operation can be rolled back.

## Requirements and safety

- A running ws-ckpt daemon is required. If its socket is unavailable, the command returns `CheckpointDaemonUnavailable`.
- The default socket is `/run/ws-ckpt/ws-ckpt.sock`; pass `--socket <path>` to use another socket.
- Checkpoint commands do not support `--dry-run`. Check the workspace and snapshot IDs before restoring, deleting, or cleaning up.

## Commands

| Command | Required arguments | Purpose |
|---|---|---|
| `cosh-cli checkpoint init` | `--workspace <path>` | Initialize a workspace |
| `cosh-cli checkpoint recover` | `--workspace <path>` | Restore a plain directory or an interrupted initialization backup |
| `cosh-cli checkpoint create` | `--workspace <path> --id <id>` | Create a snapshot |
| `cosh-cli checkpoint list` | none (`--workspace` is optional) | List snapshots |
| `cosh-cli checkpoint restore <id>` | `--workspace <path>` | Restore a snapshot |
| `cosh-cli checkpoint status` | none (`--workspace` is optional) | Show daemon status |
| `cosh-cli checkpoint delete` | `--snapshot <id>` | Delete a snapshot |
| `cosh-cli checkpoint diff` | `--workspace <path> --from <id> --to <id>` | Compare snapshots |
| `cosh-cli checkpoint cleanup` | `--workspace <path>` | Keep a bounded number of snapshots |

All commands use the `cosh-cli checkpoint` prefix:

```bash
cosh-cli checkpoint init --workspace /home/agent/project
cosh-cli checkpoint create --workspace /home/agent/project --id before-change --message "safe point"
cosh-cli checkpoint list --workspace /home/agent/project
cosh-cli checkpoint diff --workspace /home/agent/project --from before-change --to after-change
cosh-cli checkpoint restore before-change --workspace /home/agent/project
```

Optional controls include `--pin` and `--metadata <json>` on `create`, `--force` and `--workspace <path>` on `delete`, and `--keep <count>` on `cleanup`. `list` and `status` can omit `--workspace` to query all workspaces known to the daemon.

`recover` delegates workspace validation to the daemon, so it also accepts an
original path that is missing after interrupted initialization. When recovery
succeeds with migrated data or backups left for inspection, the JSON response
remains `ok: true` and includes the daemon's message and locations in
`meta.warning`; ordinary success omits that field. See the
[ws-ckpt recovery guide](../../../runtime/ws-ckpt.md#recovering-interrupted-initialization-and-stale-registrations)
for data sources and handling a missing live subvolume.

## Typical rollback flow

Create a snapshot, perform and verify the high-risk operation, then restore it if the operation fails. After a successful operation, clean up old snapshots when they are no longer needed.

```bash
cosh-cli checkpoint create --workspace /path/to/workspace --id pre-action --message "safe point"
cosh-cli checkpoint restore pre-action --workspace /path/to/workspace
cosh-cli checkpoint cleanup --workspace /path/to/workspace
```

Responses use the standard [CoshResponse<T> envelope](../output-format.md).
