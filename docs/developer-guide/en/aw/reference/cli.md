# CLI automation reference

[中文版](../../../zh/aw/reference/cli.md)

For the full command and option inventory, see the user [CLI reference](../../../../user-guide/en/aw/cli-reference.md).
This page describes output contracts for scripts and automation.

Use `aw commands --json` for command and argument discovery, and command-specific `--help` for conditional requirements.

## Output selection

| Entrypoint | Output |
| --- | --- |
| `install`, `uninstall`, `list --installed`, `doctor`, `recover`, `config init/validate/add-*/remove-*` | Optional local `--json` |
| `service status/stop`, `instance`, `request`, `config show`, `update` | Always JSON; do not add `--json` |
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
Help and discovery run offline. `aw -V` reports the CLI crate version.

## Independent instance JSON

`aw instance` uses the `aw-instance/v1` envelope, separating `result.state`, operation errors and preparation audit keys. See [independent instances](../deployment/independent-instances.md) for fields, confirmation deadlines and duplicate-request handling.

## Public audit export

`aw audit-export` reads one page from an existing same-user service state directory.
The service may be stopped. Use its configured state path; do not create an empty
replacement. The journal directory must be owned by the effective user with mode
0700, its source descriptor and chain files with mode 0600. The request file is
bounded to 16 KiB and must be regular. The CLI accepts no control operation.

```bash
AW_STATE=/absolute/private/state
AW_QUERY="$HOME/aw-audit-query.json"
cat > "$AW_QUERY" <<'JSON'
{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":64}}
JSON
aw audit-export --state-dir "$AW_STATE" --request "$AW_QUERY"
```

A successful response has `error: null`, a durable `source_id`, and `result.keys`.
Copy `result.next_cursor` into the next enumeration request until it is null.
`result.changed` reports key-set changes while continuation still progresses.
The cursor retains the first page's `through` ceiling; a new scan collects later
and newly earlier keys. Retain per-chain checkpoints and bound the consumer loop.
For each key, send `{"type":"read","chain_id":"KEY","cursor":null,"limit":64}`
as `operation`, retaining the same `api_version` wrapper. Replace `KEY` with a
returned key. Continue with that response's `result.cursor` in the next read.

Read responses contain the ExecutionEvent, public AuditRecords, captured tip,
`caught_up`, `terminal` and fixed limits. A page counts source positions, so it can
have no public records and still advance. Only `caught_up && terminal` completes
a chain; revisit open chains and periodically start a new inventory scan.
Persist records and cursors atomically, deduplicate by producer and UID, and
compare object digests on replay. Historical producer generations remain intact.
The response never contains private journal records. See the
[export contract](../../../../../src/aw/docs/design/en/runtime/audit-export.md)
for schema names, bounds, corruption and missing-history handling.

Protocol failures return `result: null` and a stable `error` code, with exit 1.
Command syntax/request-file I/O failures use stderr. Cancellation has no write
side effects. After the one-shot command exits there is no resource to stop.
Remove `"$AW_QUERY"` when the request file is no longer needed.
