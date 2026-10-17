# AW configuration reference

[中文版](../../zh/aw/configuration.md)

This reference describes the fields accepted by the current configuration
validator. For a starter file, current availability and the intended Agent
workflow, begin with the [user guide](../../../user-guide/en/user-entrypoint/aw.md).

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
| `agents.<id>.adapter` | `qwenpaw`, `qoder`, `openclaw`, `hermes`, `pi`, `claude-sdk` or `agentscope`; recognition is not runtime certification |
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
[boolean script example](../../../user-guide/en/user-entrypoint/aw.md#boolean-policy-commands).

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
Other recognized events, exact selectors, guards and stronger execution guarantees
are not runtime support. Qoder, OpenClaw, QwenPaw, Hermes, Pi, Claude SDK and explicit AgentScope 2.0.6 factories have launch Adapters;
their supported versions, entrypoints and
after-event coverage are listed in the [user guide](../../../user-guide/en/user-entrypoint/aw.md).

## Events, effects and tool selection

The configuration recognizes these 16 names. Their descriptions define the
vocabulary; runtime support is the narrower surface described above.

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

`tool.before` permits `observe`, `block`, `replace_input`. `tool.after` permits
`observe`, `replace_result`. Other events are observation-only in this revision.
`ask` is reserved for before steps; an active step requesting it is rejected.
An explicitly disabled before step can retain `ask` for future editing, without
acquiring approval capability. Native host approval is unaffected.

`on_error: block` is valid only before execution (`tool.before` or the guard).
`withhold_result` is valid only after a tool. `report` records a failure and
continues. Withholding requires a verified model-consumption boundary; replacing
a history entry is insufficient. Required redaction must not use `report`.
The service must enforce these requirements at admission and execution.

Selectors are `*`, `bash`, `file_read`, `file_write`, or
`native:<adapter>:<exact-name>` for any declared adapter ID. Native selectors
are host-specific, not portable tool semantics. There are no regex/glob selectors
other than the single `*`. All-tools routing includes native custom tools and
preserves their input; it does not make every Provider understand every tool.

`required: false` cannot authorize dropping an active control effect or its
failure action. Runtime admission checks every enabled step against
Provider declarations, implementation and native capabilities, and rejects
unsupported required controls. Optional unavailable observation sources must be
reported explicitly. Parsing alone does not perform that admission.

## Parsing and compatibility

The parser accepts one UTF-8 YAML or JSON document, bounded to 4 MiB before and
after expansion and nesting depth 32. Duplicate keys, non-string mapping keys,
custom YAML tags, merge keys, non-finite numbers and multiple documents fail.
Ordinary aliases are expanded within those limits. Diagnostics include field
paths or source locations and constraints without echoing field values.

This alpha configuration is separate from existing capability wire records and
their Schema IDs/digests. Do not pass Provider configuration through the
integer-only wire canonicalizer. No native files are installed or changed by
this validator; rollback consists of removing the new library dependency and
restoring any configuration draft edited by the caller.
