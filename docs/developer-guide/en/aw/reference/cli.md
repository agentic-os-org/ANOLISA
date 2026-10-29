# CLI automation reference

[中文版](../../../zh/aw/reference/cli.md)

Use `aw commands --json` for command and argument discovery, and command-specific `--help` for conditional requirements.

## Output selection

| Entrypoint | Output |
| --- | --- |
| `install`, `uninstall`, `list --installed`, `doctor`, `recover`, `config init/validate/add-*/remove-*` | Optional local `--json` |
| `service status/stop`, `request`, `config show`, `update` | Always JSON; do not add `--json` |
| `inspect` | Requires `--json` |
| `run` | Preserves native Agent output; forwards arguments after `--` unchanged |

Human management output accepts `--quiet`, `--verbose` and `--color auto|always|never`.
Quiet and verbose are mutually exclusive; all three conflict with JSON. Quiet retains errors and cleanup warnings.

## Management reports

```bash
aw config validate --config "$HOME/aw/aw.yaml" --json
aw doctor --prefix "$AW_PREFIX" --json
```

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

| Field or status | Consumer action |
| --- | --- |
| `format: 1` | Report version |
| `status: succeeded` | Exit 0; read typed `result` |
| `failed` / `cancelled` / `partial` | Exit 1; read `error.code`; partial means incomplete recovery |
| `mutation.committed` | Whether package files and records crossed the durable commit point |
| `mutation.recovery` | `not_attempted`, `complete` or `incomplete` |
| `warnings` | Inspect resource paths and finish cleanup; committed uninstall may exit 0 with warnings |
| `changed: false` | Editing preserved configuration bytes |
| `policy_update_scope: true` | Candidate passed offline scope validation; run `aw update` to activate it |

Syntax errors print text to stderr, leave stdout empty and exit 1. I/O failure can occur after publication; inspect state before retrying.
`config show` returns private configuration values. Fixed management error summaries do not apply to native output or protocol streams.

## Uninstall and recovery

Scripts use `aw uninstall --prefix ABS_PREFIX --yes`; system scope also requires `sudo` and `--scope system`.
Interactive uninstall accepts only `y` or `yes`; JSON and non-terminal calls require `--yes`. `--dry-run` only previews.
See [package management](packages.md) for `needs_recovery`, `package_in_use`, `plan_blocked` and recovery steps.

## Command discovery

`aw commands --json` returns `format`, `name`, `version` and `commands`, with command paths and visible arguments.
`required` marks unconditional requirements only. Completion uses the same declarations and supports bash, zsh, fish, powershell and elvish.
Help and discovery run offline. `aw -V` reports the CLI crate version; installation records provide the Preview package identifier.
