# AW configuration reference

[中文版](../../zh/aw/configuration.md)

This reference describes the fields accepted by the current configuration
validator. For a starter file, current availability and the intended Agent
workflow, begin with the [user guide](../../../user-guide/en/aw/index.md).

The [bundled schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/schemas/configuration-v1alpha1.schema.json)
defines the public shape. Rust validation also checks references and relationships
between fields. The local Host performs Provider discovery and admission; the
selected Adapter performs its native preparation checks before binding. Qoder
requires CLI >=1.1.64 and records the probed version instead of an exact-release gate; OpenClaw,
QwenPaw, Hermes, Pi, Claude SDK and AgentScope SDK retain their adapter-specific
version or revision restrictions. AgentScope SDK requires exactly 2.0.6.
For a working four-framework example, start with the [integration walkthrough](integration.md).

## Document fields

The only accepted envelope is `apiVersion: aw/v1alpha1`,
`kind: AWConfiguration`, `metadata: {name: ...}` and `spec: {...}`.
The earlier flat `api_version`/`name` design draft is not accepted or migrated.
`status`, installed bindings, revisions and capabilities are not user input.
All fields below are inside `spec` unless stated otherwise.

| Field | Contract |
| --- | --- |
| `metadata.name` (outside `spec`) | Configuration identity; 1 to 128 ASCII letters, digits, `.`, `_` or `-` |
| `daemon.startup` | `on_demand` starts or reuses the matching service; `external` requires an existing service; validation starts neither |
| `daemon.endpoint`, `daemon.state_dir` | Required nonempty strings; `auto` selects a private configuration-specific location; explicit paths must be absolute and agree on `aw.sock` |
| `execution.guarantee` | Only `native_hook`; no OS, final or protected guarantee |
| `execution.default_event_budget_ms` | Required positive shared event budget, not a fresh budget for each step |
| `audit.enabled`, `audit.payload` | This revision requires `true` and `metadata_only`; the service persists execution metadata |
| `agents.<id>.adapter` | `qwenpaw`, `qoder`, `qoder-sdk`, `openclaw`, `hermes`, `pi`, `claude-sdk` or `agentscope`; recognition is not runtime certification |
| `agents.<id>.shutdown_grace_ms` | External-signal shutdown grace, 1..120000 ms, default 2000; see [lifecycle](deployment/lifecycle.md) |
| `daemon.limits` | Resident service concurrency limits; see [capacity settings](deployment/lifecycle.md#concurrent-service-budgets) |
| `agents.<id>.argv` | Nonempty executable/argument array; first element must be nonempty; no implicit shell or interpolation |
| `agents.<id>.qoder.sequential` | Optional boolean for `adapter: qoder`; marks generated native groups as sequential; false does not override other matching groups |
| `providers.<id>.protocol` | Defaults to invoke-only `aw-hook/v1alpha1`; explicit `aw-provider/v1alpha1` retains structured messages, `native-hook/v1alpha1` retains native callback bytes |
| `providers.<id>.transport` | `{type: stdio, location: agent, argv: [...]}`; runs a one-shot process in the bound Agent context |
| `providers.<id>.timeout_ms` | Positive per-invocation ceiling; runtime must also cap it by the event's remaining budget |
| `providers.<id>.max_output_bytes` | Positive stdout ceiling enforced by the command executor |
| `providers.<id>.config` | Required opaque JSON object for structured Providers; native Hooks require an empty object; boolean Hooks omit it or use an empty object |
| `events.<name>.enabled` | Required boolean for a declared event; omitted events are disabled |
| `events.<name>.required` | Defaults to `false`; a disabled event cannot be required |
| `events.<name>.budget_ms` | Optional positive override of the default event budget; a nested guard also shares the parent remaining budget |
| `events.<name>.steps` | Required ordered array; an empty array invokes no Provider |
| `events.tool.before.match.tools`, `events.tool.after.match.tools` | Optional nonempty selector array; omitted means all native tools; `['*']` cannot be mixed with exact selectors |
| `events.tool.before.guard` | Optional reference to a declared `security.violation` event, enabled when this before event is enabled |
| `steps[].id`, `steps[].enabled` | ID unique within its event; enabled defaults to `true` |
| `steps[].provider` | Declared Provider ID whose protocol must match the step form |
| `steps[].match.tools` | Optional tool selector array for a tool-before/after step; intersects the event selector; omitted means all tools |
| `steps[].operation` | Defaults to `invoke` for boolean Hooks (the only supported value); structured Providers require an explicit operation checked against discovery |
| `steps[].native` | Empty object selecting native callback execution; mutually exclusive with `operation` and `effects` |
| `steps[].effects` | Required nonempty unique list; all requested on a boolean match, an admitted upper bound for structured Providers; omit for native steps |
| `steps[].reason_code` | Optional match reason for boolean Hooks only; 1–128 ASCII letters, digits, `.`, `_` or `-` |
| `steps[].on_error` | `report`, `block` or `withhold_result`, constrained by event timing |

Agent/Provider IDs and step/operation names use the same syntax as
`metadata.name`. Numeric limits are integers from 1 through 4,294,967,295.
There are at most 128 Agents, Providers or steps per event, and 128 arguments
per executable. Empty arguments after the executable are preserved. NUL bytes
are rejected in executable arguments and endpoint/directory strings.

Public objects reject unknown fields. Provider `config` alone accepts private
fields. Missing Provider references and duplicate step IDs are errors even in
disabled steps, so enabling a step does not uncover a hidden reference typo.
Defaults are documented behavior, not values inserted into the parsed document.

## Qoder SDK execution assets

Set these fields under `agents.<id>.qoder_sdk` for a `qoder-sdk` target.
Omit them when the application needs no execution extensions.

| Field | Integration and limits |
| --- | --- |
| `sites` | Nonempty unique list of `global` and/or `cn`; the first entry is the default. Omitted: `[global]`. Install the selected SDK packages in the application directory. |
| `command_wrappers` | Up to 16 grants for native Bash consumers. Each is `{path, sha256}` with an absolute regular-file path and a 64-character lowercase hexadecimal SHA-256. |
| `runtime_preloads` | Up to 16 grants with the same file shape, for trusted Worker imports. |
| `declarative_plugins` | Default `false`; admit snapshots of local role, Agent, command and skill assets on ordinary resident Queries using managed daemons. |

Each granted file is limited to 16 MiB. Keep wrappers and preloads immutable in
the image. AW verifies files before SDK probing and rechecks them for each Query;
a mismatch rejects startup. These limits count installed assets, not lifetime
Queries or tool calls. Changing assets or sites requires a worker restart.

The Bridge accepts `awSite: "global"` or `"cn"` in `startDaemon`,
`createQuery().options` and `createSdkMcpServer`. Omitted Query sites inherit the
managed daemon's site; an explicit different site is rejected.

### Worker imports

On `startDaemon`, `executableArgs` accepts paired `--import` arguments naming
admitted preload paths. `pathToQoderCLIExecutable`, if provided, must identify
the selected SDK package's official Worker. AW rejects other executable and
transport replacements. Direct Queries reject `executableArgs` because the pinned
Worker-thread transport ignores it; use a managed daemon for imports.

### Declarative plugin snapshots

With `declarative_plugins: true`, pass local plugin roots through native
`options.plugins: [{type: "local", path: "/absolute/plugin"}]`. Use metadata
manifests and declarative Markdown. Agent `tools` selections are supported.
AW rejects executable plugin Hooks, MCP/settings redirection, shell interpolation,
`permissionMode`, `allowed-tools`, and skill/command `context` or `agent`
frontmatter. Unknown or ambiguous frontmatter and symlinks are rejected.
Use managed SDK Hooks or MCP factories for executable behavior.

| Snapshot resource | Per-Query limit |
| --- | --- |
| Files and directories across all plugin roots | 10,000 entries |
| Total content | 128 MiB |
| Directory depth below each root | 64 |

AW forces `skills.disableShellExecution: true` and confirms native settings
before releasing the first prompt. Direct Queries reject this profile because
they cannot confirm the effective settings. Reloadable launches reject plugin
snapshots and command wrappers before SDK probing. This profile is pinned to
SDK 1.0.56 / Worker 1.1.78; revalidate on SDK upgrades.

After confirmed native and callback drain, AW removes snapshots. Unconfirmed
cleanup retains snapshots and scope ownership; inspect the failure and restart
the worker. Native command consumers additionally require explicit approval and
completion acknowledgement; see the [Qoder SDK factory](agents/qoder-sdk.md#native-bash-command-consumers).

## Default boolean hooks

Omitting a Provider's `protocol` selects `aw-hook/v1alpha1`. Its step omits
`operation` or declares `invoke`; the Host performs local admission and makes
no `describe`/`validate_config` calls. The command receives only the normalized
event and returns one JSON boolean with exit 0. AW projects the step's effects
and optional reason code on true, and no effects on false, retaining request
correlation internally. Nonzero exit, timeout, incomplete stdin, invalid or
oversized output remains a failure governed by `on_error`. Stdout is capped at
32 bytes as well as the configured output ceiling. Existing explicitly declared
structured and native protocols keep their behavior. See the
[boolean script example](providers/hooks.md).

## Native commands and runtime support

Native steps use `native: {}` and a `native-hook/v1alpha1` Provider. They run
without `describe` or `validate_config`, receive the exact native callback input
and return byte output and exit status to the Adapter. They do not claim
structured AW effects. Nonzero exits remain native results for CLI dialects; Pi treats nonzero/signal
exits and invalid native directives as callback failures under `on_error`, alongside
deadline or output-limit errors. A native command
written for one framework is not automatically portable. Current Qoder callback
input must remain identical across AW steps; sequential rewrite chains and
approval flows are outside the accepted native combinations. Changed input is
rejected and handled through the affected step's `on_error`.

For Claude SDK `PreToolUse`, a native command's exit-0
`hookSpecificOutput.permissionDecision: deny` uses the same veto path as exit 2,
including its optional reason and early-event closure. This does not enable
native approvals or input rewrites.

Runtime currently admits `tool.before` with `observe`/`block` and `tool.after`
with `observe` or a single final structured `replace_result`, plus native steps
at these points. Boolean/native steps cannot request result replacement. Qoder CLI maps them to
`PreToolUse` and successful `PostToolUse`; failed tools are not yet connected.
Other recognized events, guards and stronger execution guarantees
are not runtime support. Qoder, Qoder SDK, OpenClaw, QwenPaw, Hermes, Pi, Claude SDK and explicit AgentScope 2.0.6 factories have launch Adapters;
their supported versions, entrypoints and
after-event coverage are listed in the [native Agent reference](native-agents.md).

## Events, effects and tool selection

The configuration recognizes these 16 names. Public Provider dispatch currently
covers only `tool.before` and `tool.after`; the other fourteen names are reserved.
Implementation status is in the [16-Hook overview](../../../user-guide/en/aw/capabilities.md#16-common-hooks).
See the [capability matrix](../../../user-guide/en/aw/capabilities.md#hook-support-matrix)
for Adapter effect support.

| Event | Meaning |
| --- | --- |
| `session.start` | Session creation, load or restore |
| `input.submit` | Input reaches a native submission point |
| `tool.before` | Tool intent before native execution |
| `tool.after` | A native tool completion, including reported failures |
| `permission.request` | The host requests a permission decision |
| `compact.before` | Before context compaction |
| `compact.after` | Native compaction result |
| `subagent.start` | Native subagent startup |
| `subagent.stop` | Native subagent stopping point |
| `turn.stop` | Task stop check, not proof of success |
| `session.end` | Native session end |
| `model.before_request` | Model request at a verified sending boundary |
| `runtime.observed` | Trusted runtime registration |
| `runtime.exited` | Trusted root runtime exit observation |
| `security.violation` | Provisional name for AW's active, final internal tool-before check |
| `coverage.changed` | Change in observed integration coverage |

The reserved `security.violation` design runs through the guard of an enabled
`tool.before`; the current runtime rejects guards.
It inspects the final candidate and permits `observe`/`block`, without changing
parameters. It is not a second native Hook or a promise to run after every
third-party Hook. Parameter changes after the check require another check at
the actual enforcement boundary.

The schema vocabulary permits `tool.before` to declare `observe`, `block`, `replace_input`. `tool.after` permits
`observe`, `replace_result`. Other events accept observation effects in the schema. Runtime admission supports only the two implemented tool events and rejects `replace_input`.
`ask` is reserved for before steps; an active step requesting it is rejected.
An explicitly disabled before step can retain `ask` for future editing, without
acquiring approval capability. Native host approval is unaffected.

`on_error: block` is valid only before execution (`tool.before` or the guard).
`withhold_result` is valid only after a tool. `report` records a failure and
continues. Withholding requires a verified model-consumption boundary; replacing
a history entry is insufficient. Required redaction must not use `report`.
The service enforces these requirements at admission and execution. Ordinary
Qoder SDK and QwenPaw resident launches declare `withhold_result` for structured Provider
and boolean Hook after steps. Reloadable launches and native-command steps reject it.

Both tool events and their steps accept `match.tools`. A step runs only when
the tool matches both lists; entries within one list are alternatives. Omitting
either selector, or using `["*"]`, selects all tools at that level. Lists must
be nonempty and unique; `*` must stand alone. The rule applies to structured
Providers, boolean Hooks and native commands.

Selectors are case-sensitive `*`, `bash`, `file_read`, `file_write`, or
`native:<adapter>:<exact-name>`. Standard names match normalized
`event.tool.name`. Native selectors match the registered Adapter ID in
`event.agent.adapter` and the retained `event.tool.native_name`; the Adapter ID
is separate from the configured target name. For example, `native:qoder:Bash`
selects Qoder's exact `Bash` name. There are no regex or glob patterns; `*` is the
only wildcard. Provider-private tool mappings remain independently configured.

Before startup, AW projects event and step selectors for the selected Adapter, excluding steps whose intersection is provably empty. Only remaining steps participate in Provider preparation and effect admission; structure and references are still checked across the complete configuration.
`aw validate` reports projected-out steps for each target. `aw config validate --json` uses `result.projected_out_steps` with target, event and step_id.
OpenClaw after normalizes `bash`, `apply-patch` and `cron` to `exec`, `apply_patch` and `automations`; native_name retains the callback's original name.

Selection occurs in the shared Host after callback normalization, binding checks
and the once-only step claim. An unmatched step completes with `kind: skipped`,
no effects and no invocation process. It still validates the normalized event,
checks cancellation and the shared deadline, and retains step order, correlation
and completion accounting. Enabled structured Providers still complete
`describe` and `validate_config` during preparation, even when a later tool
does not match.

A skipped or neutral step grants no permission. Native denial and pending
permission requests retain precedence; unsupported argument rewrites remain
rejected. A Tokenless PreTool suggestion requires a fresh call through selected
security steps and native permission checks. PostTool replacement remains one
final authorized text slot, preserving the native envelope and failure status.

`required: false` cannot authorize dropping an active control effect or its
failure action. Runtime admission checks every enabled step against
Provider declarations, implementation and native capabilities, and rejects
unsupported required controls. Optional unavailable observation sources must be
reported explicitly. Parsing alone does not perform that admission.

## Parsing and compatibility

The parser accepts one UTF-8 YAML or JSON document, bounded to 4 MiB before and
after expansion and nesting depth 32. Duplicate keys, non-string mapping keys,
custom YAML tags, merge keys, non-finite numbers and multiple documents fail.
Ordinary aliases are expanded within those limits. JSON input follows the same
YAML scalar resolution: an exponent spelling such as `1e3` arrives as
float-typed `1000.0`, which the schema's integer type accepts. A JSON starter
document mirrors the YAML starter at
[aw.minimal.json](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.minimal.json).
Decimal integer scalars outside both the signed and unsigned 128-bit ranges are
rejected rather than rounded into floats, including scalars tagged `!!float`.
Use exponent/decimal notation for floating-point values and quoted strings for
exact large identifiers. This does not expand the existing integer value range.

Diagnostics include field
paths or source locations and constraints without echoing field values.

This alpha configuration is separate from existing capability wire records and
their Schema IDs/digests. Do not pass Provider configuration through the
integer-only wire canonicalizer. No native files are installed or changed by
this validator; rollback restores configuration compatible with the selected binary.
