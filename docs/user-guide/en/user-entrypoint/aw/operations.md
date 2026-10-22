# Operations

[中文版](../../../zh/user-entrypoint/aw/operations.md)

Use the configuration path from installation. Exit all Agents sharing that configuration before
stopping its AW service.

## Command help and arguments

Start with `aw --help` (or just `aw`), then request help for a group or action.
Help does not load configuration, contact services or modify files.

```bash
aw --help
aw agent connect --help
aw config validate --help
aw service status --help
aw -V
```

Public commands accept `-h` as well as `--help`. Put help last; trailing
arguments or repeated help flags are errors. Version queries are root-only:
`aw --version` or `aw -V`. Unknown options, duplicates, missing values and
conflicting selections fail before the operation starts. Ordinary argument
errors retain exit code 1; successful help and version queries return 0.

Options belong to the command that lists them. There is no global `--json`:
`inspect` requires it, while `service status`, `service stop`, `request`,
`config show` and the runtime-policy `update` already return JSON without it.
The `update` command changes an idle instance's policy, not installed software.
`config show` includes private Provider values; review its output before sharing.

Both `--option value` and `--option=value` are accepted. Use the latter when
a value begins with `--`. Arguments after `--` in `run` and command-template
`config init` are forwarded literally, including `--help`, empty strings and
spaces. The native Agent retains its output, exit code and signal handling.
Internal Hook and lifecycle callbacks retain their protocol-specific replies
and error codes. The compatibility `aw-package` executable keeps its original
parser, including optional adjacent-bundle discovery for installation.

## Management reports

Use command-local `--json` with local-bundle `install`, `uninstall`, `config init`,
or `config add-provider/remove-provider/add-event/add-hook/remove-hook`. It does
not apply to legacy native `install --config`, `agent connect`, `config validate`
or the compatibility `aw-package` executable.

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --json
aw config remove-hook --config "$HOME/aw/aw.yaml" --event tool.before --id check --json
```

The report is one JSON object on stdout. `format: 1` identifies its schema;
`command` identifies the operation and `status` is `succeeded` or `failed`.
Success returns a typed `result`, for example:

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

`changed: false` means a semantic no-op preserved the file. Installation results
include the selected prefix; configuration generation includes its path and
template name. Neither reports Provider configuration values or runtime
activation. `uninstalled` means owned package removal succeeded; external
configuration and state remain. Installation verification is the existing
Preview package check, not publisher trust or native Agent acceptance.

An operation failure returns exit code 1 and a `failed` object with an `error`
containing `code` and a fixed summary `message`. Codes are `io`, `invalid_json`,
`invalid_yaml` and `package_contract`; raw library errors are not copied into
this report. Success returns 0. A failed command makes no claim that all changes
were rolled back. Existing installer cleanup warnings can still appear on
stderr; the report does not replace those warnings with a transaction inventory.

Syntax errors remain text on stderr with exit code 1 and empty stdout, even
when `--json` was among the rejected arguments. Default human diagnostics and
explicit `config show` output retain their existing contracts; this is not a
global private-data redaction feature. Service JSON, stdin/stdout protocols and
native Agent output do not receive this management envelope.

## Check status and stop

```bash
aw status --config "$HOME/aw/aw.yaml"
aw inspect --config "$HOME/aw/aw.yaml" --json
aw stop --config "$HOME/aw/aw.yaml"
```

`inspect` includes Provider summaries. The AW service can outlive the Agent; use `stop`
to shut it down and `status` to confirm. Manage sec-core and AgentSight separately.

The grouped commands preserve the same arguments and JSON responses:

```bash
aw service status --config "$HOME/aw/aw.yaml"
aw service stop --config "$HOME/aw/aw.yaml"
aw service serve --config "$HOME/aw/aw.yaml" --state-dir "$HOME/aw/state"
aw service --help
aw --version
```

`serve` stays in the foreground until stopped or interrupted. It does not install
systemd units or start an Agent. The original `aw serve/status/stop` commands remain
compatible. `--version` reports the CLI crate version, not an installed bundle's
Preview release identifier or native Agent version.

## Upgrade and uninstall

Before upgrading, exit Agents, stop the service using the old AW binary and original configuration,
and back up configuration and state. Install the new package into a new prefix and generate
configuration with its paths. Keep the old package and backups for rollback. Preview has no
cross-version data migration, so use a new state directory when upgrading.

Uninstall with the prefix used during installation. Use the command below for a system install;
omit `sudo` for a personal install.

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX"
```

Uninstall preserves external configuration, audit history and native Agent data. Modified or missing
package files cause an error first. Follow the diagnostic instead of deleting a directory still in use.

## Keep data

AW stores invocation metadata in `state_dir/journal`, without tool content. Components such as
Tokenless have separate data directories and retention settings; see [Tokenless data](tokenless.md#data-and-privacy).

Set an explicit state directory to preserve records across restarts. It must belong to the runtime
user with mode `0700`. Containers also need persistent storage for this directory, the framework
profile and the workspace. Automatic state paths may use temporary storage. See
[containers and persistence](../../../../developer-guide/en/aw/native-agents.md#containers-and-persistent-data)
for deployment details.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| Validation passes but launch fails | Check the reported Agent version, executable paths and Provider configuration |
| Edits do not change behavior | Stop the old service using the original configuration or socket, then launch the edited file |
| Directory or configuration permissions rejected | Use a private directory owned by the runtime user; avoid symlinks when editing configuration |
| Service protocol mismatch | Stop the old service with the old binary, then consistently use the new installation |
| OpenClaw reports result replacement unavailable | Check the supported framework version and middleware API; safety checks and completion observation can still work |
| Socket remains after a forced exit | Confirm the old service has exited, then remove only its stale `aw.sock`; preserve the journal |
| Long-running session reports `native_event_limit` | Restart the AW-managed Agent; a new native session does not reset the instance counter |
| No compression statistics | Check that a supported tool ran and review Tokenless recording settings |

Each Agent instance currently holds up to 1,024 associated native events. A completed tool call
usually uses two entries when both before and after are enabled. Plan instance restarts for
long-running Gateways and Apps.

Verify policies using both AW's result and actual tool behavior. A model declining to call a tool,
or a native permission denial, does not prove that an AW rule ran.
