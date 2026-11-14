# Routine operations

[中文版](../../zh/aw/operations.md)

## Inspect and stop

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`status` lists instances; `inspect` shows Provider summaries. Exit all Agents sharing this configuration before stopping AW, then use `status` to confirm shutdown. sec-core and AgentSight services have their own owners.

Resident Pi/Claude SDK, Qoder SDK and QwenPaw tasks expose
`instances[].scope_validity` in status. `native_alive` reports worker liveness;
`covered_bridge` reports the monitored AW path. A `failed` bridge blocks covered
calls while independent healthy tasks continue. Drain and close the failed task,
then create a new managed task. Unconfirmed native cleanup requires restarting
its owner. Calls are never replayed automatically; capabilities outside AW
coverage retain native behavior.

`inspect --json` includes `service.resident_scopes` counts for `native_alive`,
`ready`, `failed`, `pending` and `unmonitored`. Startup readiness alone does not
prove continuing validity. Idle checks run two seconds after the prior check
finishes; the service blocks scopes without a successful check for fifteen
seconds. `null` means the connected service did not supply these counts.

## Check installation

For local Provider extensions, reuse `AW_EXTENSION_PREFIX` from installation.

```bash
aw list --installed --prefix "$AW_EXTENSION_PREFIX"
aw doctor --prefix "$AW_EXTENSION_PREFIX"
```

`list` reads Provider extension records; `doctor` checks missing, modified or incorrectly permissioned files. A successful check exits 0; file issues exit 1 and list the affected paths. AW CLI and backend installations remain managed by their original installer or native package manager.

## Upgrade and uninstall

1. Exit the Agent and stop the service with the old AW binary and configuration.
2. Back up configuration and state. Upgrade the AW CLI and backends through their original installer; [install](installation.md) new Provider bundles into a new extension directory.
3. Generate configuration with the new paths, use a new state directory and verify an actual tool call.
4. Retain the old installation and backups. To roll back, stop the new service and restore the old paths and configuration.

An old prefix containing `.aw-packages` can remain registered in the same user
database while you install into a new directory. AW preserves its records and
files. Check the new installation with `--prefix "$AW_EXTENSION_PREFIX"`;
operations explicitly targeting the old prefix reject its metadata. Use the
old CLI to manage that installation. A global `doctor` still reports its
unsupported metadata.

Remove locally managed Provider extensions with:

```bash
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --yes
```

Root-managed extensions require `sudo` and explicit `--scope system`. External configuration, audit and Agent data remain; the command does not remove the AW CLI, backends or their services. Stop any AW process holding package files before retrying.
If `doctor` or uninstall reports modified or missing files, inspect the affected paths and preview removal with `--keep-changed`:

```bash
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --keep-changed --dry-run
aw uninstall --prefix "$AW_EXTENSION_PREFIX" --keep-changed --yes
```

This removes the selected package records and unchanged files. Changed content, modes, file types and symlinks stay untouched; missing paths are left missing. The preview lists `KEEP PATH` entries and successful removal lists `KEPT PATH` entries. Add package IDs after `uninstall` to select packages; without IDs, all packages at the prefix are selected. Remaining dependencies, access errors and unsafe installation directories still block removal.

The empty registration and lock remain. If the entire prefix is missing, AW recreates these metadata files while clearing the package records. Preserved files become user-owned: move or remove any conflicting paths before reinstalling, or choose a new prefix.

For `needs_recovery`, stop package writers and follow [package recovery](../../../developer-guide/en/aw/reference/packages.md).

## Data and troubleshooting

`state_dir/journal` stores AW call metadata. See Tokenless [data and privacy](tokenless.md#data-and-privacy) for text retention.

| Symptom | Action |
| --- | --- |
| Validation passes but launch fails | Check the reported Agent version, executable path and Provider configuration |
| Edits do not take effect | Exit the Agent, stop the old service with its original configuration, then restart |
| Unsafe configuration or directory | Use a private directory owned by the current user; avoid symlinks when editing |
| Service protocol mismatch | Stop the service with the old binary, then use the new AW consistently |
| Socket remains after forced exit | Confirm the old service has exited, remove only its stale `aw.sock` and retain journal |
| `native_event_limit` | Restart the AW-managed Agent instance; a new native session does not reset its count |
| Empty compression statistics | Confirm supported tool calls occurred and check recording settings |

Use `aw --help` and `aw commands` to discover commands. `aw completion bash` (or `zsh`, `fish`) prints a completion script.
See the [CLI reference](../../../developer-guide/en/aw/reference/cli.md) for automation reports and [deployment guide](../../../developer-guide/en/aw/deployment/lifecycle.md) for persistence and capacity.
