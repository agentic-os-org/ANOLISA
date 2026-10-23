# Operations

[中文版](../../zh/aw/operations.md)

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
and error codes. The standalone `aw-package` executable has been removed.
First installation uses the `aw` in the bundle root and requires an explicit
absolute `--bundle` path; it does not discover an adjacent bundle automatically.

## Command discovery and completion

List public commands without loading configuration, contacting a service or
executing a Provider. The inventory and completion scripts use the same command
declaration as parsing and help; hidden compatibility aliases and internal
callbacks remain callable but are excluded from discovery.

```bash
aw commands
aw commands --json
aw completion bash
aw completion zsh
aw completion fish
aw completion powershell
aw completion elvish
```

`completion` writes a script to stdout; it does not install it or modify shell
startup files. Review and load it using your shell's usual completion setup.
The shell name is required and unsupported names fail before any work.

`commands --json` returns one object with `format: 1`, the CLI `name`, its crate
`version`, and a `commands` array. Each entry has a full command path,
description and visible arguments, including names, value labels and fixed
choices. `required` reports only unconditional requirements; conditional
requirements, conflicts and argument forwarding remain authoritative in the
parser and action help. This is a syntax inventory, not a complete validation
schema, installed-package inventory or runtime/Provider capability report.
Success returns 0; invalid syntax or an output-stream failure returns 1.

## Management reports

Use command-local `--json` with local-bundle `aw install`, `aw uninstall`,
`aw list --installed`, `aw doctor`, `aw recover`,
`aw config init`, `aw config validate`, `aw config add-provider`,
`aw config remove-provider`, `aw config add-event`, `aw config add-hook` or
`aw config remove-hook`. It does not apply to legacy native installation,
`aw agent connect` or legacy `aw validate`.

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --json
aw config remove-hook --config "$HOME/aw/aw.yaml" --event tool.before --id check --json
aw config validate --config "$HOME/aw/aw.yaml" --json
```

The report is one JSON object on stdout. `format: 1` identifies its schema;
`command` identifies the operation and `status` is `succeeded`, `failed`,
`cancelled` or `partial`.
Success returns a typed `result`, for example:

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

`changed: false` means a semantic no-op preserved the file. Installation results
include the selected prefix; configuration generation includes its path and
template name. Neither reports Provider configuration values or runtime
activation. `uninstalled` means owned package removal succeeded; external
configuration and state remain. Installation verifies local descriptions and
files; it does not establish publisher trust or native Agent acceptance.

An operation failure returns exit code 1 and a `failed` object with an `error`
containing `code` and a fixed summary `message`. Codes are `io`, `invalid_json`,
`invalid_yaml`, `package_contract`, `invalid_configuration`, `cancelled`,
`needs_recovery`, `package_in_use` and `plan_blocked`;
raw library errors are not copied into this report. Validation is offline;
`policy_update_scope: true` means `--against` checked only the permitted edit
scope, not runtime activation. The original `aw validate` output remains unchanged.

Local installation and removal also report `mutation.committed`,
`mutation.recovery` and any `warnings`. `committed` identifies the
durable database commit for package files and installation records. Recovery values are
`not_attempted`, `complete` and `incomplete`, covering only this invocation's
tracked resources. `not_attempted` does not mean zero writes; `complete` does
not certify machine-wide state. Warning entries have a stable `code`, resource
`path`, and an optional coarse `io_kind`, not a copied I/O error string.

Successful operations return 0, including a committed uninstall whose backup
cleanup left warnings. Those warnings remain visible in JSON and human quiet
mode. A failure with incomplete recovery uses `partial` and exit 1; its original
`error.code` is retained, including cancellation. Otherwise an observed
cooperative package cancellation uses `cancelled` and exit 1, preserving the
existing package exit convention; other failures use `failed` and exit 1.
A failed command never implies that all changes were rolled back. Reported
package operations place notices inside JSON without separate stderr warnings.

Syntax errors remain text on stderr with exit code 1 and empty stdout, even
when `--json` was among the rejected arguments. Default human diagnostics and
explicit `config show` output retain their existing contracts; this is not a
global private-data redaction feature. Service JSON, stdin/stdout protocols and
native Agent output do not receive this management envelope.

## Installed packages and file checks

List the packages recorded for an installation, then check their files:

```bash
aw list --installed --prefix "$AW_PREFIX"
aw doctor --prefix "$AW_PREFIX"
aw list --installed --prefix "$AW_PREFIX" --json
aw doctor --prefix "$AW_PREFIX" --json
```

`list --installed` reads package records without testing whether the installed
files still exist or match. `doctor` checks each recorded file for missing or
changed content, unexpected permissions, symbolic links and other invalid file
types. It reports all detected file problems, returns 1 when any are found, and
does not repair them. A clean check returns 0. Both commands are offline and
read-only: they do not create directories or lock files, start services, execute
Providers or update installation records.

For registered raw installations, `doctor` also requires the database locator
and the existing installation lock, including after all packages have been
removed. Missing management metadata is reported with its path; listing retains
the central package facts so they remain available for diagnosis.

### Where records are stored

Without `--prefix`, both commands read the selected package database. User scope
is the default, at
`$XDG_STATE_HOME/aw/packages`, or `$HOME/.local/state/aw/packages` when
`XDG_STATE_HOME` is unset or empty. A nonempty `XDG_STATE_HOME` must be absolute.
This database is separate from the runtime `state_dir` in `aw.yaml`.
`--scope system` selects `/var/lib/aw/packages`. It is a root-managed raw AW
database, not the RPM or DEB database. Scope is never inferred from `sudo`.
With an explicit prefix, queries follow its recorded database locator instead;
they do not search other directories. Ordinary users can read a system database
and create their own AW configuration from a readable system prefix.

The database's `installed.json` contains the schema version, database generation
and installed-package records, including package versions, installation paths,
dependencies and owned files. The existing `lock` coordinates access;
`transactions/` holds unfinished package operations. Queries do not create any
of these files. A missing database returns an empty list; an existing but
incomplete, malformed or unsupported database returns an error. Unfinished
transactions return `needs_recovery` rather than triggering automatic recovery.

New installations store one locator at `<prefix>/.aw-packages/aw-core.json`.
The central database owns the package facts; the locator only identifies its
database, scope and installation ID. It must match the central record and cannot
coexist with old component records. Historical installers reject this locator
before changing their files.

Older Preview installs have component records in `<prefix>/.aw-packages/*.json`.
Specify an absolute `--prefix` to query them. Competing old records at a registered
prefix cause an error; queries do not merge or silently discard them.
Older records do not identify the original installation scope, so the text
output shows `not recorded` and JSON uses `scope: unknown`.
Omitting `--prefix` does not search for
old installations. Queries never import old records. Each migration attempt
archives exact old JSON bytes under `legacy/<installation_id>/<transaction_id>/`
in the selected database. A committed transaction replaces the old records with
the sole locator; rolled-back attempts keep separate archives, so later changes
made by an older installer do not block another migration attempt.

The first write to an old installation also checks that its recorded AW
executable matches the current command. A mismatch returns `runtime_unknown`
before migration writes anything: that old runtime cannot yet participate in
the new package locks. Keep the old installation and install this Preview in
a new prefix. In-place core upgrades are not supported.

For machine-readable checks, both commands use the management JSON envelope.
A `doctor` file problem returns `status: failed`, `error.code: integrity_check`
and the findings in `result.report.issues`. The JSON remains on stdout with no
separate stderr diagnostics. Database and record errors also return 1; do not
treat them as an empty installation or a clean file check. The same local
`--quiet`, `--verbose` and `--color` rules apply as to other management commands.

## Package plans and recovery

Inspect local package changes before executing them:

```bash
aw install --bundle /tmp/example-base --prefix "$AW_PREFIX" --dry-run --json
aw install --bundle /tmp/example-base --prefix "$AW_PREFIX"
aw install --bundle /tmp/example-mant --prefix "$AW_PREFIX"
aw uninstall example.mant --prefix "$AW_PREFIX" --dry-run
aw uninstall example.mant --prefix "$AW_PREFIX" --yes
```

These are user-scope examples. For a root-managed prefix, add `--scope system`
and run writes with `sudo`; dry-run itself grants no privileges. Plans show
package names and exact versions, added or removed files, dependencies and every
observed blocker. A blocked plan returns 1, including with `--dry-run`. A dry run
does not create a prefix, database, lock or recovery directory. Execution reads
the inputs again while holding write locks, so a stale plan cannot authorize
changed files.
JSON keeps this information in `result.plan`, including `problems[].code` and
`problems[].message`; a blocked plan uses `error.code: plan_blocked` while
retaining the plan. A successful preview has no mutation report.

Each independent bundle contains `package.json` and a `payload/` tree. The
description records its package ID, SemVer version, Linux architecture, each
file's SHA-256 and mode, and exact package requirements. A normal tool need not
declare a Provider or depend on `aw-core`. For example, `example.mant` may require
exactly `example.base` version `2.1.0`, while keeping its own version `7.4.2`.
Requirements must exist in the selected prefix; the command neither downloads
missing packages nor selects dependencies in another prefix. Preview bundles
keep their existing same-build requirements. Package replacement and in-place
upgrade are not supported.
Each payload file is limited to 1 GiB. Installation and append plans limit the
combined recorded payload in one prefix to 4 GiB, using bytes actually verified
from existing and incoming files. An append beyond that limit is blocked before
any write, so the resulting installation remains within the verification bound
needed for later removal.
One prefix may contain at most 32,767 package-owned files. Plans also budget
the size of recovery records and reject operations beyond their capacity before
changing package files.

Removal checks reverse dependencies throughout the selected database and never
deletes unrecorded files. It refuses modified or missing owned files and prefixes
held by a cooperating AW runtime. After the last package is removed, the empty
registration, locator and prefix lock remain. The list contains zero packages;
a later install reuses that prefix without permitting an old installer to write it.

An interrupted operation keeps durable recovery records. Stop package writers
and run the matching scope:

```bash
aw recover --json
sudo aw recover --scope system --json
```

Recovery finishes cleanup when the database already committed, otherwise it
restores the previous files and records. Changed or unrecognized files are
preserved and reported as conflicts; keep the reported backups until resolved.
If database publication cannot be confirmed durable, the operation reports
incomplete recovery and retains its journal. Recovery retries the required
durability checks before removing that evidence.
Normal package queries and writes report `needs_recovery` while unfinished
transactions remain. Recovery does not execute a Provider or repair arbitrary
file damage. The package database is not a runtime instance inventory.

## Terminal output

The management commands listed above also accept local human-output options:

| Option | Effect |
| --- | --- |
| `--quiet` | Suppress success summaries; retain errors and installer cleanup warnings. |
| `--verbose` | Add fixed operation-scope details on stderr, without configuration values. |
| `--color auto` | Color a summary only when its output stream is a terminal (the default). |
| `--color always` | Color summaries even through a pipe or with color-disabling environment variables. |
| `--color never` | Disable summary colors. |

```bash
aw install --prefix "$AW_PREFIX" --bundle "$PWD/bundle" --quiet
aw config remove-provider --config "$HOME/aw/aw.yaml" --name unused --verbose
aw uninstall --prefix "$AW_PREFIX" --yes --color never
```

`--quiet` and `--verbose` conflict. `--json` conflicts with all three options;
it is never silently suppressed. They are not global options and do not apply
to native connection, service/protocol commands, `config show` or legacy `aw validate`.
Arguments after the forwarding `--` remain literal.

Auto color is disabled by nonempty `NO_COLOR` or `TERM=dumb`; explicit `always`
overrides those settings. Success summaries use stdout, operation errors use
stderr, and each stream's terminal status is checked independently. Presentation
options do not grant permission or supply confirmation. Canonical removal has
the command-local consent rule described below; quiet does not bypass it.

CLI-owned management summaries and operation errors escape control and bidi
characters in metadata so paths cannot inject terminal instructions or lines.
Install/removal notices use the same escaping. Other commands retain their
existing diagnostic formats. It is not global private-data redaction.
JSON values and native/protocol streams retain their existing formats.

An output-stream failure can return nonzero after files were already published.
That does not imply rollback. Inspect the files before retrying; the CLI does
not retry an operation to repair its output.

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
configuration with its paths. Keep the old installation directory and backups for rollback. Preview has no
cross-version data migration, so use a new state directory when upgrading.

To roll back, switch back to the preserved old installation and its configuration.
New core packages install only `bin/aw`. Registered prefixes contain a locator
that old installers reject; copying an older installer there does not make it
compatible. Keep the locator and its database together.

Uninstall with the prefix used during installation. Use the command below for a system install;
omit `sudo` for a personal install.

```bash
sudo "$AW_PREFIX/bin/aw" uninstall --prefix "$AW_PREFIX" --scope system --yes
```

Uninstall preserves external configuration, audit history and native Agent data. Modified or missing
package files cause an error first. Follow the diagnostic instead of deleting a directory still in use.
The current runtime holds package read locks while it uses the installed CLI,
Providers and their processes. Removal refuses a held prefix. If the recorded
AW executable differs from the command performing removal, AW cannot verify
that older runtime's lock support and reports the plan problem `runtime_unknown`, even if its
files are intact. Preserve that prefix until a supported migration is available.
This does not enumerate arbitrary externally launched programs or implement the
later instance-management commands.

### Confirm removal

`aw uninstall` asks `Remove package-owned files from ...? [y/N]` on stderr only
when stdin and stderr are both terminals and JSON is not selected. `--dry-run`
does not ask for consent and never executes removal. Pressing
Enter, answering no, or EOF declines removal with exit 1. Only `y` or `yes`
(case-insensitive, ignoring surrounding whitespace) confirms it. The package
operation has not started while the prompt is waiting. SIGINT and SIGTERM in
that phase return 130 and 143 without starting removal.

For automation, redirected streams, containers without a terminal or JSON,
confirm explicitly:

```bash
aw uninstall --prefix "$AW_PREFIX" --yes
aw uninstall --prefix "$AW_PREFIX" -y --json
```

Non-interactive or JSON removal without `--yes` fails before package work;
JSON returns `failed` with `error.code: confirmation_required` and no mutation
report. Piping `yes` into stdin is not consent. `--quiet` only suppresses the
success summary; it still requires consent. `--yes` skips this prompt only:
ownership, integrity, path and permission checks still apply, and it does not
run sudo. It is local to removal, not accepted by installation, configuration,
service or native Agent commands.

Use `aw uninstall --prefix ... --yes` in scripts. Existing
cooperative cancellation during package work remains distinct from cancelling
the prompt, using the typed management result and exit 1 described above.

## Keep data

AW stores invocation metadata in `state_dir/journal`, without tool content. Components such as
Tokenless have separate data directories and retention settings; see [Tokenless data](tokenless.md#data-and-privacy).

Set an explicit state directory to preserve records across restarts. It must belong to the runtime
user with mode `0700`. Containers also need persistent storage for this directory, the framework
profile and the workspace. Automatic state paths may use temporary storage. See
[containers and persistence](../../../developer-guide/en/aw/native-agents.md#containers-and-persistent-data)
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
