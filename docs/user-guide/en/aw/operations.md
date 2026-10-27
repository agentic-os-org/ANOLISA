# Routine operations

[中文版](../../zh/aw/operations.md)

## Inspect and stop

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`status` lists instances; `inspect` shows Provider summaries. Exit all Agents sharing this configuration before stopping AW, then use `status` to confirm shutdown. sec-core and AgentSight services have their own owners.

## Check installation

Reuse the installation's `AW_PREFIX`.

```bash
aw list --installed --prefix "$AW_PREFIX"
aw doctor --prefix "$AW_PREFIX"
```

`list` reads installation records; `doctor` checks missing, modified or incorrectly permissioned files. A successful check exits 0; file issues exit 1 and list the affected paths.

## Upgrade and uninstall

1. Exit the Agent and stop the service with the old AW binary and configuration.
2. Back up configuration and state, then [install](installation.md) the new bundle into a new directory.
3. Generate configuration with the new paths, use a new state directory and verify an actual tool call.
4. Retain the old installation and backups. To roll back, stop the new service and restore the old paths and configuration.

Uninstall a system installation with the following command. For a personal installation, remove `sudo` and use `--scope user`.

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX" --scope system --yes
```

External configuration, audit and Agent data remain. Resolve modified, missing or busy package files before retrying.
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
