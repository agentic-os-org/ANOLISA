# AW configuration reference

[中文版](../../zh/aw/configuration.md)

This reference describes the fields accepted by the current configuration
validator. For a starter file, current availability and the intended Agent
workflow, begin with the [user guide](../../../user-guide/en/user-entrypoint/aw.md).

The [bundled schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/schemas/configuration-v1alpha1.schema.json)
defines the public shape. Rust validation also checks references and relationships
between fields. Provider discovery and native capability checks remain planned.

## Document fields

The only accepted envelope is `apiVersion: aw/v1alpha1`,
`kind: AWConfiguration`, `metadata: {name: ...}` and `spec: {...}`.
The earlier flat `api_version`/`name` design draft is not accepted or migrated.
`status`, installed bindings, revisions and capabilities are not user input.
All fields below are inside `spec` unless stated otherwise.

| Field | Contract |
| --- | --- |
| `metadata.name` (outside `spec`) | Configuration identity; 1 to 128 ASCII letters, digits, `.`, `_` or `-` |
| `daemon.startup` | `on_demand` or `external`; lifecycle intent only, no process is started during validation |
| `daemon.endpoint`, `daemon.state_dir` | Required nonempty strings; `auto` denotes a future product-selected local endpoint/directory; explicit deployment validity is checked by the future service |
| `execution.guarantee` | Only `native_hook`; no OS, final or protected guarantee |
| `execution.default_event_budget_ms` | Required positive event budget; covers the future complete event path, not just each Provider |
| `audit.enabled`, `audit.payload` | This revision requires `true` and `metadata_only`; audit persistence is subsequent service work |
| `agents.<id>.adapter` | `qwenpaw`, `qoder`, `openclaw` or `hermes`; recognition is not runtime certification |
| `agents.<id>.argv` | Nonempty executable/argument array; first element must be nonempty; no implicit shell or interpolation |
| `providers.<id>.protocol` | Only `aw-provider/v1alpha1`; independent of configuration version |
| `providers.<id>.transport` | `{type: stdio, location: agent, argv: [...]}`; describes a future one-shot process at the Agent execution location |
| `providers.<id>.timeout_ms` | Positive per-invocation ceiling; runtime must also cap it by the event's remaining budget |
| `providers.<id>.max_output_bytes` | Positive stdout ceiling; runtime enforcement is subsequent work |
| `providers.<id>.config` | Required opaque JSON object, including Unicode keys and finite decimals; its Provider must validate its private schema later |
| `events.<name>.enabled` | Required boolean for a declared event; omitted events are disabled |
| `events.<name>.required` | Defaults to `false`; a disabled event cannot be required |
| `events.<name>.budget_ms` | Optional positive override of the default event budget; a nested guard also shares the parent remaining budget |
| `events.<name>.steps` | Required ordered array; an empty array invokes no Provider |
| `events.tool.before.match.tools`, `events.tool.after.match.tools` | Optional nonempty selector array; omitted means all native tools; `['*']` cannot be mixed with exact selectors |
| `events.tool.before.guard` | Optional reference to a declared `security.violation` event, enabled when this before event is enabled |
| `steps[].id`, `steps[].enabled` | ID unique within its event; enabled defaults to `true` |
| `steps[].provider`, `steps[].operation` | Declared Provider ID and nonempty operation name; whether the Provider implements the operation is checked later |
| `steps[].effects` | Nonempty unique effect list; describes requested upper bounds, not a permission grant |
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

## Events, effects and tool selection

The configuration recognizes these 16 names.

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

`security.violation` only runs through the guard of an enabled `tool.before`.
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
`native:<adapter>:<exact-name>` for any of the four adapter IDs. Native selectors
are host-specific, not portable tool semantics. There are no regex/glob selectors
other than the single `*`. All-tools routing includes native custom tools and
preserves their input; it does not make every Provider understand every tool.

`required: false` cannot authorize dropping an active control effect or its
failure action. Future runtime admission must check every enabled step against
Provider declarations, implementation and native capabilities, and reject
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
