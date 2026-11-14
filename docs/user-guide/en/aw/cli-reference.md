# AW command reference

[中文版](../../zh/aw/cli-reference.md)

Use this page to look up an `aw` subcommand, its arguments and its effect. For a first launch, start with [Quickstart](index.md); installation steps are in [Installation and first run](installation.md).

## Syntax and help

`FILE`, `DIR`, `TARGET` and other capitalized names below are values to replace, not literal arguments. `ABS_FILE`, `ABS_DIR` and `ABS_PATH` require absolute paths. Options belong after their command; AW has no global `--config` or `--json` option. Except for repeatable `--template-var` and positional arguments, supply each option once.

```bash
aw --help
aw --version
aw commands
aw commands --json
aw run --help
aw config init --help
```

`-h` / `--help` is available on public commands; `-V` / `--version` reports the AW CLI version at the root. A help or version flag must appear once and be the last argument. Help, discovery and completion do not load configuration or start services.

`--agent TARGET` selects a key under `spec.agents`, such as `qoder`; it does not install that Agent. AW does not expand shell variables inside configuration files. Expand them in the shell when supplying command arguments.

## Public command index

| Command | Purpose |
| --- | --- |
| `aw run` | Launch a configured Agent with AW |
| `aw agent connect` | Register native Hooks or attach to a supported running process |
| `aw config init` | Create configuration offline |
| `aw config show` | Print all or selected configuration as JSON |
| `aw config validate` | Validate configuration and optionally compare update scope |
| `aw config add-provider` | Add one complete Provider definition |
| `aw config remove-provider` | Remove an unreferenced Provider |
| `aw config add-event` | Add one complete event definition |
| `aw config add-hook` | Append a step to an existing event |
| `aw config remove-hook` | Remove one event-scoped step |
| `aw config native-sources` | List redacted native source declarations |
| `aw config add-native-source` | Record a native source reference |
| `aw config remove-native-source` | Remove a source reference |
| `aw service serve` | Run the local AW service in the foreground |
| `aw service status` | Query an existing service |
| `aw service stop` | Request service shutdown |
| `aw update` | Activate a policy on one supported idle instance |
| `aw inspect` | Query the service and optional Provider summaries |
| `aw install` | Install local Provider extensions; also retains legacy native Hook connection syntax |
| `aw uninstall` | Remove locally managed Provider extensions |
| `aw list` | List recorded local Provider installations |
| `aw doctor` | Check their installed files |
| `aw recover` | Recover interrupted local package operations |
| `aw commands` | Enumerate public commands; optionally include machine-readable argument metadata |
| `aw completion` | Print a shell completion script |
| `aw instance setup` | Install startup assets in an explicitly selected private directory |
| `aw instance prepare` | Prepare the calling application's native factory and registration |
| `aw instance release` | Release the owner's registration and staged assets |
| `aw instance attach` | Attach or remove supported native Hooks on a selected process |
| `aw instance update` | Apply policy using the shared `aw.yaml` by default |
| `aw instance` | Register, confirm, query and release application-owned instances through JSON |
| `aw audit-export` | Read one retained public audit page without a control connection |
| `aw request` | Send a service operation from stdin JSON; for integration and diagnostics |
| `aw policy` | Run AW's command Provider protocol; for custom rule integration |

## Output options

The following management commands accept the shared output options: local-extension `install`, `uninstall`, `list`, `doctor`, `recover`, `config init`, `config validate`, `config add-provider`, `config remove-provider`, `config add-event`, `config add-hook`, `config remove-hook`, `config add-native-source` and `config remove-native-source`.

| Option | Default and meaning |
| --- | --- |
| `--json` | Emit a versioned management report instead of the human summary |
| `--quiet` | Suppress success summaries; retain errors and cleanup warnings |
| `--verbose` | Add operation-scope details on stderr |
| `--color auto\|always\|never` | `auto`; color human management summaries |

`--json` conflicts with all three human-output options. `--quiet` conflicts with `--verbose`. `--color` can accompany either human-output option. These options do not change native Agent or protocol streams.

`service status`, `service stop`, `update`, `request`, `config show` and `config native-sources` already produce JSON and do not accept `--json`. `inspect` requires `--json`. `run` preserves the Agent's output, exit code and signals. For JSON report fields and scripting exit codes, see [CLI automation](../../../developer-guide/en/aw/reference/cli.md).

## Launch and connect

### `aw run`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--config FILE` | Required | AW configuration to launch |
| `--agent TARGET` | Required | Configured target name |
| `--native-profile NAME` | Adapter-specific | Existing Hermes profile directory; other Adapters can reject it |
| `--native-settings FILE` | Adapter-specific | Native settings; optional Qoder overlay, required for OpenClaw |
| `--native-state-dir DIR` | Adapter-specific | Required existing OpenClaw state directory |
| `--reloadable` | Off | Opt into the restricted stable policy-update bridge on supported entrypoints |
| `-- AGENT_ARGS...` | Optional | Append literal arguments to the configured native argv |

OpenClaw requires both `--native-settings` and `--native-state-dir` and rejects `--native-profile`. Hermes requires `--native-profile` and rejects the other two native path options. Consult the [Agent integration guides](../../../developer-guide/en/aw/native-agents.md) before applying these options to another Adapter.

Ordinary resident Qoder SDK and QwenPaw support policy updates without `--reloadable`. The flag selects a different, restricted mode; it is not a universal way to enable updates. See [capability support](capabilities.md) and [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md).

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

AW can start its local service on demand; the service can remain reusable after the Agent exits. See [service commands](#service-and-policy-updates) for status and shutdown.

### `aw agent connect`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--config FILE` | Required | AW configuration |
| `--agent TARGET` | Required | Configured native target |
| `--native-profile NAME` | Adapter-specific | Native profile used for persistent registration |
| `--pid PID` | Optional positive integer | Attach to an idle, caller-owned supported Qoder TUI or QwenPaw App process |
| `--remove` | Off; requires `--pid` | Remove AW-owned attached Hooks while leaving the native process running |

`--pid` conflicts with `--native-profile`. Without `--pid`, this command performs the Adapter's supported persistent registration; it does not install Agent software. With `--pid`, readiness, ownership and idle checks apply. See [process attachment](../../../developer-guide/en/aw/deployment/process-attach.md) for prerequisites and detach behavior.

## Configuration

Configuration generation and editing are offline: they do not start a Provider, launch an Agent or activate policy changes. Stop users of the old configuration before editing, or use the supported idle-update procedure afterward.

### `aw config init`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--prefix ABS_DIR` | Required | Existing installation prefix used for package and template verification |
| `--config ABS_FILE` | Required | New private configuration file outside the installation prefix; does not overwrite an existing file |
| `--state-dir ABS_DIR` | Required | Private AW runtime and audit directory; must not overlap installed payloads or package metadata |
| `--qoder ABS_FILE` | Required unless `--openclaw` is supplied | Qoder executable |
| `--node ABS_FILE` | Required with `--openclaw` | Node executable |
| `--openclaw ABS_FILE` | Required unless `--qoder` is supplied; requires `--node` | Readable OpenClaw entrypoint |
| `--provider none\|command` | `none` | Generate no Provider calls, or a boolean command rule |
| `--template ABS_FILE` | Optional; conflicts with `--provider` | Explicit distributor or Provider configuration template |
| `--template-var NAME=VALUE` | Repeatable; requires `--template` | Literal template substitution; names must be unique; `PREFIX` and `STATE` are supplied by AW |
| `--check ABS_PROGRAM` | Required for `--provider command` | Executable for the boolean command rule |
| `--effect block\|observe` | Required for `--provider command` | Effect when the command returns `true` |
| `--reason-code CODE` | Required for `--provider command` | Nonempty reason: at most 128 ASCII letters, digits, `_`, `.` or `-` |
| `-- CHECK_ARGS...` | Optional; requires `--provider command` | Literal arguments for the check executable |
| Shared output options | Optional | [Output options](#output-options) |

Select Qoder, OpenClaw or both. `--node` and `--openclaw` must be supplied together. Command-only options and trailing check arguments require `--provider command`; generation verifies executable paths but does not run the check. An existing state directory must be owned by the current user with mode `0700`.

Template variable names contain 1–128 ASCII uppercase letters, digits or `_`; reserved names cannot be overridden.

The generator supports Qoder and OpenClaw entrypoints. Other Agents use the configuration described in their [integration guides](../../../developer-guide/en/aw/native-agents.md). See [configuration and launch](configuration.md) for Provider templates.

### `aw config validate`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--config FILE` | Required | Candidate configuration |
| `--against BASELINE_FILE` | Optional | Also verify that changes are allowed for a policy-only update |
| Shared output options | Optional | [Output options](#output-options) |

Validation checks configuration and offline update scope. It does not confirm native readiness or activate policy. Relative configuration paths are accepted here.

### `aw config show`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--config ABS_FILE` | Required | Existing private configuration |
| `--provider NAME` | Optional | Select a Provider; conflicts with `--event` |
| `--event EVENT` | Optional | Select one event |
| `--id ID` | Optional; requires `--event` | Select a Hook step within the event |

With no selector, print the complete configuration as JSON. Output includes private Provider values; inspect before sharing.

### Configuration edit commands

All commands below require `--config ABS_FILE`. Definition files contain one complete YAML/JSON object. Edits validate and publish the complete configuration; they can rewrite comments and formatting, so back up first. All mutation commands in this table accept [shared output options](#output-options).

| Command | Additional required arguments | Meaning |
| --- | --- | --- |
| `aw config add-provider` | `--name NAME --definition FILE` | Add the complete Provider definition under `NAME` |
| `aw config remove-provider` | `--name NAME` | Remove the Provider only after its Hook references are removed |
| `aw config add-event` | `--event EVENT --definition FILE` | Add a complete event, including its steps |
| `aw config add-hook` | `--event EVENT --definition FILE` | Append one complete step; its `id` comes from the definition |
| `aw config remove-hook` | `--event EVENT --id ID` | Remove the step; retain the event even when empty |
| `aw config add-native-source` | `--agent TARGET --definition FILE` | Append an AW source descriptor without opening the referenced native source |
| `aw config remove-native-source` | `--agent TARGET --id ID` | Remove the descriptor while retaining the native file |

For `config show` and these editing commands, use an absolute, symlink-free private configuration path owned by the current user. See the [configuration editor](../../../developer-guide/en/aw/reference/configuration-editor.md) for file permissions, idempotence, definition formats and source descriptor fields.

### `aw config native-sources`

Requires `--config ABS_FILE --agent TARGET`. Prints redacted source declarations and their order as JSON. This is a declaration query; it does not load native files or activate coverage. It does not accept shared output options.

## Service and policy updates

### `aw service serve`

Requires `--config FILE --state-dir ABS_DIR`. Runs in the foreground until interrupted or stopped. The state directory is private; no systemd unit or system service is installed. Routine launches normally use on-demand service startup through `aw run` instead.

### `aw service status` and `aw service stop`

Both require exactly one of:

| Argument | Meaning |
| --- | --- |
| `--config FILE` | Locate the existing service from its original launch configuration or recorded connection |
| `--socket ABS_PATH` | Connect to the explicit local socket |

Neither command starts a service. Both return JSON. Exit Agents sharing the service before `stop`; external sec-core or AgentSight services have their own owners. See [routine operations](operations.md).

### `aw update`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--config FILE` or `--socket ABS_PATH` | Exactly one | Select the existing service |
| `--agent TARGET` | Required unless `--instance` is given | Select the configured target; more than one matching root instance requires `--instance` |
| `--instance ID` | Required unless `--agent` is given | Select one instance; can be combined with `--agent` to require both to match |
| `--policy FILE` | Required with `--socket`; otherwise defaults to `--config` | Complete candidate configuration |
| `--expected-revision SHA256` | Current selected instance revision | Optimistic concurrency condition; conflicts are not retried |

The target must support updates and be idle through its documented cleanup boundary. Output is JSON; do not add `--json`. Offline `config validate --against` checks only update scope. For pausing dispatch, applying candidates, rollback and recovery, see [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md).

### `aw inspect`

Requires `--config ABS_FILE --json`. Queries service status and optional Provider summaries; it can execute configured Provider view commands. It does not install missing backends. See [routine operations](operations.md) and the individual Provider guides.

## Local Provider extensions

These commands manage local unpacked Provider extensions declaring `aw-provider/v1alpha1` in `package.json`. AW core, backend software, `manifest.json` bundles, URLs and RPM/DEB packages are not accepted. Software installed by another manager stays owned by that manager.

### `aw install`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `--bundle ABS_DIR` | Required for extension installation | Unpacked local Provider bundle |
| `--prefix ABS_DIR` | Required for extension installation | Destination installation directory |
| `--dry-run` | Off | Show files, packages and conflicts without writes |
| `--scope user\|system` | `user` | Package database scope; system writes require root |
| Shared output options | Optional | [Output options](#output-options) |

The legacy native-connection form is listed under [compatibility](#compatibility-and-internal-entrypoints). It cannot be combined with extension-installation options.

### `aw uninstall`

| Argument | Required / default | Meaning |
| --- | --- | --- |
| `PACKAGE...` | Optional | Exact package IDs to remove; omit to remove all recorded packages in the prefix |
| `--prefix ABS_DIR` | Required | Registered installation directory |
| `--dry-run` | Off | Preview removal and blockers without writes or confirmation |
| `--keep-changed` | Off | Retain changed or missing selected payload paths while removing package records; dependencies, access errors and unsafe directories still block removal |
| `--scope user\|system` | `user` | Package database scope; system writes require root |
| `-y`, `--yes` | Off | Confirm removal; does not override ownership, integrity or busy-file checks |
| Shared output options | Optional | [Output options](#output-options) |

Interactive removal asks for confirmation. JSON or non-terminal removal requires `--yes`. External configuration and state are retained.
Blocked human plans write problems to stderr and leave stdout empty; JSON retains the plan. See [operations](operations.md#upgrade-and-uninstall) for changed-file recovery and retained-path reports.

### `aw list`, `aw doctor` and `aw recover`

| Command | Arguments | Meaning |
| --- | --- | --- |
| `aw list` | Required `--installed`; optional `--prefix ABS_DIR`, `--scope user\|system`, shared output options | Read recorded installations; remote search is not supported |
| `aw doctor` | Optional `--prefix ABS_DIR`, `--scope user\|system`, shared output options | Check installed file integrity and permissions; does not repair files or start services |
| `aw recover` | Optional `--scope user\|system`, shared output options | Recover interrupted transactions from that scope's durable records |

Scope defaults to `user`; being root does not automatically select system scope. `list` and `doctor` can follow a prefix's registered scope when scope is omitted; an explicit conflicting scope is rejected. Without `--prefix`, they query the selected scope's inventory. `recover` writes and requires root for system scope. See [package management and recovery](../../../developer-guide/en/aw/reference/packages.md) for database locations and conflict handling.

## Discovery, completion and integration

### `aw commands`

No arguments are required. Optional `--json` emits versioned command paths and argument metadata from the same declaration used for parsing and help. It describes public syntax, not installed Providers or Agent capability support. Conditional requirements still require command-specific help or this reference.

### `aw completion`

Requires one positional `SHELL`: `bash`, `zsh`, `fish`, `powershell` or `elvish`. Prints a script to stdout without installing it. For example:

```bash
aw completion bash > aw.bash
```

### `aw request`

For integration and diagnosis, requires `--socket ABS_PATH`. Optional `--timeout-ms N` is `1..60000`, default `5000`. Reads one service operation from stdin JSON, sends it to an existing service and returns JSON; it does not start a service. Routine users should prefer `service status`, `service stop` and `update`. The [local service contract](../../../../src/aw/docs/design/en/runtime/local-service.md) defines operations and wire data.

### `aw policy`

Takes no options beyond help. Serves the command Provider protocol over stdin/stdout for AW-managed custom rules; it is not a policy display or update command. See [custom Hooks](../../../developer-guide/en/aw/providers/hooks.md) for the boolean-command integration.

## Compatibility and internal entrypoints

| Retained command | Preferred public command and differences |
| --- | --- |
| `aw serve --config FILE --state-dir ABS_DIR` | `aw service serve`; same arguments |
| `aw status (--config FILE \| --socket ABS_PATH)` | `aw service status`; same arguments |
| `aw stop (--config FILE \| --socket ABS_PATH)` | `aw service stop`; same arguments |
| `aw validate --config FILE [--against BASELINE_FILE]` | `aw config validate`; the alias does not accept shared output options |
| `aw install --config FILE --agent TARGET [--native-profile NAME] [--pid PID] [--remove]` | `aw agent connect`; the same native connection restrictions apply; no shared output options or extension options |

The following hidden commands are generated and called by Adapters, not user setup commands. They are omitted from `aw commands` and completion. Their private binding and stdin protocols must match the installed AW version; do not handcraft calls or copy bindings between workers. They use a separate protocol parser: public command help and `aw commands --json` do not enumerate their accepted lifecycle actions or payloads.

| Internal command | Argument surface |
| --- | --- |
| `aw hook` | Required `--adapter`, `--binding`, `--event`, `--step`, `--on-error`; optional `--close-event true` |
| `aw bridge` | Required `--adapter`, `--binding`; optional `--confirm FILE` for native attachment confirmation |
| `aw scope` | Required `--adapter`, `--binding`, `--action`; invocation routes use `--event`, `--step`, `--on-error`, `--close-event true` |

Internal lifecycle actions and payloads are versioned implementation details. Application integrations use the managed factories in the [Agent guides](../../../developer-guide/en/aw/native-agents.md), which own these calls and their cleanup.

## `aw instance setup`, `prepare` and `release`

These commands retain the application's native startup command after the required
one-time integration. `setup` installs startup assets for Qoder TUI/print,
OpenClaw Gateway, Hermes local chat and QwenPaw App. SDK applications call `prepare` directly from
their startup and enter the existing AW factory in that same process. Follow the
[framework guide](../../../developer-guide/en/aw/native-agents.md) for the bootstrap
code and native options; an unprepared running application requires preparation
and restart before it can report Ready.

| Option | Commands | Meaning |
| --- | --- | --- |
| `--config FILE` | `setup`, `prepare` | Defaults to `aw.yaml` in the current directory |
| `--agent TARGET` | `setup`, `prepare` | Required configured target |
| `--directory ABS_DIR` | `setup` | Required owned directory with no group/other writes; Qoder requires mode 0700, Python requires the dedicated venv's `site-packages` |
| `--remove` | Qoder `setup` only | Remove an inactive startup installation while preserving user Hooks; live owners must exit first |
| `--native-settings FILE`, `--native-state-dir DIR` | OpenClaw setup/preparation | Existing native settings and state directory |
| `--native-profile DIR` | Hermes setup/preparation | Existing connected native profile |
| `--binding FILE` | `release` | Required exact private binding returned by `prepare` |

`setup` returns JSON containing `environment` and the exact created `files`.
It refuses overwrites, preserves existing Python `sitecustomize`, and leaves shell
profiles and running processes unchanged. Apply the returned environment only to
the intended application startup, then restart that application. Setup alone
creates no ready instance.

`prepare` is a direct child of the application owner, with no shell intermediary
or selectable PID. It follows the configured service startup policy and returns
JSON containing `instance_id`, `environment`, `binding`, `command` and
`state: pending_ready`. Apply the environment changes and invoke the documented
factory/bootstrap in the owner process. Native confirmation establishes readiness;
printing the preparation response does not enable Hook dispatch.

After native work and cleanup finish, the owner calls
`aw instance release --binding FILE` as a direct child. Success exits 0 and removes
that registration's staged directory; it does not stop the application, sibling
instances or daemon. Keep the binding private. For crash cleanup and removing
installed startup assets, see [independent instances](../../../developer-guide/en/aw/deployment/independent-instances.md).

## `aw instance attach` and `aw instance update`

Run these commands from the directory containing your shared `aw.yaml`, or supply
`--config FILE` explicitly. Attachment currently supports Qoder TUI and QwenPaw
App under the [external-process prerequisites](../../../developer-guide/en/aw/deployment/process-attach.md).
Pause request dispatch before attaching; wait for native confirmation before
resuming. The command does not start or restart the Agent.

```bash
aw instance attach --agent TARGET --pid NATIVE_PID
aw instance update --agent TARGET
aw instance update --instance INSTANCE_ID --policy next.yaml
aw instance attach --agent TARGET --pid NATIVE_PID --remove
```

Replace `TARGET`, `NATIVE_PID` and `INSTANCE_ID` with your configured target,
selected process and returned instance ID. Attach verifies the process identity
and native Hook adoption. Removal retains the native process and unrelated Hooks.
Both attachment forms accept `--config FILE`, `--agent TARGET` and `--pid PID`;
`--remove` selects removal.

Update reuses the [idle policy update contract](../../../developer-guide/en/aw/deployment/policy-updates.md).
It accepts `--config FILE`, `--agent TARGET`, `--instance ID`, `--policy FILE` and
`--expected-revision SHA256`. Select an agent, an instance, or their intersection.
Without `--policy`, the shared configuration is the candidate. Multiple matching
instances require `--instance`. Busy or rejected updates retain the active policy;
an unknown instance is an error and does not trigger attachment.

These commands return the existing attachment/update JSON responses. Update
also supports the prepared native modes in the linked guide; attachment remains
limited to its documented entrypoints. The application-owned JSON protocol below
cannot be combined with these subcommands.

## `aw instance`

Explicit Adapters connect to an existing same-user daemon using the registration protocol behind native preparation. `--socket PATH` is required. `--connect` discovers service identity without reading stdin. Other calls read registration, Ready, query or release JSON from stdin and always return JSON. `--timeout-ms` defaults to 30000 and accepts 1–60000, covering input, connection and request.

```bash
AW_SOCKET=/absolute/private/state/aw.sock
aw instance --socket "$AW_SOCKET" --connect
```

Successful discovery returns `result.connected: true` and the service identity. The application retains its process lifecycle; releasing an instance preserves sibling instances and the daemon. See [independent instances](../../../developer-guide/en/aw/deployment/independent-instances.md) for request fields, native Ready, task scopes and recovery.

## `aw audit-export`

Read retained audit metadata as the service owner. Requires `--state-dir ABS_DIR`
(the existing service state directory containing `journal/`) and `--request ABS_FILE`
(a regular JSON request file). Optional `--timeout-ms` accepts 1–60000, default 5000.
Output is always JSON; a reported error exits 1. The command works after service
shutdown and does not start or stop background processes.

Use the state directory configured for the service or supplied to `aw service serve`.
Old journals without a retained source descriptor return `source_missing`; legacy
chains return `unsupported_history`. For a bounded enumeration example and
consumer checkpoint rules, see [CLI automation](../../../developer-guide/en/aw/reference/cli.md#public-audit-export).
