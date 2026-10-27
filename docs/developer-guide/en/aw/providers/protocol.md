# Provider protocol and capability admission

[中文版](../../../zh/aw/providers/protocol.md)

[Design index](../../../../../src/aw/docs/design/en/README.md) · [Architecture](../../../../../src/aw/docs/design/en/architecture.md)

`aw-provider` defines the experimental `aw-provider/v1alpha1` interface for
external policy programs and checks their messages and requested capabilities
offline. It connects the unified configuration to a Provider contract without
starting a process, installing an Agent Hook or establishing a runtime binding.
Native integration belongs to `aw-adapter`, orchestrated by `aw-cli`; see the
[capability matrix](../../../../user-guide/en/aw/capabilities.md#hook-support-matrix) for supported capabilities.


## Transport contract

The execution contract is one process per method: the caller launches the
configured argv, writes one UTF-8 JSON object to stdin and closes stdin. The
Provider returns exactly one JSON object on stdout; diagnostic text belongs on
stderr. There is no implicit shell expansion. `aw-provider` specifies and validates
the messages; the [local Provider Host](../../../../../src/aw/docs/design/en/runtime/provider-host.md) executes them through `aw-exec`.

A policy block is a successful result and requires exit status zero. A nonzero
exit means execution failed, regardless of stdout. Invalid JSON, oversized
messages, mismatched identities and invalid output effects are failures, not
policy decisions. The Host must distinguish those failures when applying the
configured failure policy; this offline library does not apply native effects.

The request and response schemas are bundled with the crate:

- [Request schema](../../../../../src/aw/crates/aw-provider/schemas/request-v1alpha1.schema.json)
- [Response schema](../../../../../src/aw/crates/aw-provider/schemas/response-v1alpha1.schema.json)

`Protocol::new` compiles those schemas without network access.
`Protocol::parse_request` validates a request; `Request::as_value` exposes the
checked value. `Protocol::check_response` checks a response against that request
and returns `Reply::Description`, `Reply::Configuration` or `Reply::Invocation`.
Messages are limited to 1 MiB and 32 nesting levels; a Host may impose a smaller
output limit. Parsing rejects duplicate keys and additional JSON documents. Public envelopes
reject unknown fields; Provider-owned configuration and native payloads remain
JSON data rather than executable or interpreted settings.

## Three methods

Every request carries `api_version`, `method` and a request ID. The Provider must
return the same version and request ID. The caller supplies the ID and remains
responsible for its lifecycle and correlation with the chosen Provider instance.

### `describe`

```json
{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"describe-1"}
```

```json
{"api_version":"aw-provider/v1alpha1","request_id":"describe-1","status":"ok","operations":[{"name":"check","events":["tool.before"],"effects":["observe","block"]},{"name":"record","events":["tool.after"],"effects":["observe"]}]}
```

The description declares up to 64 operations and their supported events and
effects. Operation names must be nonempty and unique. A declaration is supplied by the Provider; it
cannot expand the Adapter's trusted capabilities or establish runtime identity.
The vocabulary recognizes all 16 configuration events, while this admission
profile permits enabled execution only at `tool.before` and `tool.after`.

### `validate_config`

```json
{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"validate-1","config":{"rule_sets":["command-safety"]}}
```

```json
{"api_version":"aw-provider/v1alpha1","request_id":"validate-1","status":"ok"}
```

`config` is a required JSON object. The Provider owns its schema and must reject unsupported or invalid
settings. AW checks the response and retains its association with the exact
validated configuration. A successful response does not validate Agent settings,
prove that rules were installed or authenticate the Provider executable.

### `invoke`

An invocation supplies the configured operation, private configuration,
configuration revision, remaining budget, admitted effects, normalized event and
an opaque input digest. A checked successful response returns that same digest
and a list of candidate effects. An empty list imposes no additional restriction;
`observe` requests observation and `block` requests prevention of the pending
tool call. Both are candidate effects; the library itself writes no audit record.
None of these responses grants native permission.
`Outcome::requests_block()` reports only whether that candidate effect is present.

| Invocation field | Meaning |
| --- | --- |
| `operation`, `config` | Selected Provider operation and its validated private settings |
| `config_revision` | SHA-256 of the caller's exact configuration document, encoded as 64 lowercase hex characters |
| `budget_ms` | Positive remaining execution budget supplied and enforced by the Host |
| `allowed_effects` | Effects admitted for this operation and event |
| `event.agent` | Adapter, configured `binding_id` and nullable runtime `instance_id` |
| `event.session_id`, `event.tool.call_id` | Native identifiers, or null when unavailable |
| `event.tool` | `name`, `native_name`, `call_id`, `input` and `result`; before-tool `result` must be null |
| `event.native` | Original native JSON object, retained without inventing additional authority |
| `input_digest` | Opaque binding generated for the complete invocation |

The event preserves the configured Adapter and binding, native session and call
identifiers when available, the arbitrary tool name, JSON arguments, tool result
and original native payload. Missing identifiers remain null. JSON object keys
may contain Unicode. Negative integer literals must fit `i64`, and nonnegative
integer literals must fit `u64`; out-of-range integer literals are rejected.
Decimal and exponent forms use finite IEEE-754 `f64` values. Arbitrary-precision
numbers and lossless round trips of the original numeric spelling are not
supported; large identifiers represented as strings remain strings.

Private values and tool results retain their JSON types and Unicode data within
these numeric limits. A string result is not silently parsed as another object,
and a null result does not certify success. This protocol does not infer that an
unknown tool is a shell command or certify the provenance of its arguments.

`Protocol::bind_invocation` accepts a local invocation without `input_digest`,
hashes its compact JSON serialization and adds the `sha256:`-prefixed binding.
`parse_request` verifies that binding; the Provider echoes it in its response.
Providers need not reproduce the serialization. This digest domain is separate
from the stricter canonical wire encoding in `aw-contracts`. Matching digests do
not authenticate the sender or prove adoption. `config_revision` identifies the
configuration supplied by the caller; the library checks its shape, not the
existence or contents of a configuration file.

An error response uses `status: "error"` and `error_code`, without success fields.
A malformed response or a Provider error produces a protocol error. It cannot be
converted into a successful `block` result by the Provider protocol checker.
At most 64 effects may be returned. `reason_code` and `error_code` contain 1–128
ASCII letters, digits, underscores, hyphens or periods. The library's error
display omits untrusted codes and payloads; callers control any diagnostic use.

## Offline admission

`admission::preflight(config, target, capabilities)` checks enabled configuration
requirements against the supported profile and trusted Adapter capabilities
without discovery or process execution. The Host runs it before starting any
Provider. Its returned steps remain candidates; preflight does not establish
Provider support.

`admission::admit` accepts a statically checked `aw_config::Configuration`, a
target ID, caller-trusted `AdapterCapabilities` and Provider evidence keyed by
the configured Provider IDs. Each Provider entry includes a checked description
and checked private-configuration validation. Adapter evidence identifies the
Adapter, version, entrypoint and supported event/effect pairs; the caller must
obtain those facts from a trusted source appropriate to the actual runtime.

Admission verifies that every enabled selected step has a supported operation,
event and effect, and that its validation evidence matches the Provider's exact
private configuration. Successful admission returns checked steps. It does not
return a Core `PreparedPlan`, install a binding, start an Agent or report `Ready`.
The current transport profile requires `stdio` at location `agent`. Disabled
events and steps need no discovery evidence. Step order within an event is
preserved; admission does not schedule distinct events or choose parallelism.

| Request | Admission result |
| --- | --- |
| `tool.before`: `observe`, `block` | Accepted when the selected operation and Adapter both support every requested effect |
| `tool.after`: `observe` | Accepted when both sides support it |
| `tool.after`: `replace_result` | One structured step with trusted text-replacement capability |
| `ask`, input replacement or a block after execution | Rejected |
| An enabled event outside the two tool events | Rejected, including when `required: false` |
| A guard such as `security.violation` | Rejected; final-check ordering is not implemented by this profile |
| Event and step `match.tools` | Accepted; the Host invokes only tools matching both selectors |
| A guarantee beyond `native_hook` | Rejected |
| A missing Provider, operation, capability or matching configuration validation | Rejected with an explicit reason |

Before-tool `on_error` may be `report`, or `block` when the Adapter supports
blocking. After-tool failures support `report`, or `withhold_result` when the
prepared Adapter explicitly declares that event-scoped failure action. Currently
ordinary Qoder SDK and QwenPaw resident entrypoints declare it. Other actions are rejected.

`required: false` does not authorize silently dropping an enabled unsupported
step. Every enabled structured Provider still supplies discovery and exact
configuration-validation evidence before runtime tool selection.

Event and step selectors intersect; omission or `["*"]` covers all tools at
that level. Standard selectors `bash`, `file_read` and `file_write` match
normalized `event.tool.name`. Exact `native:<adapter>:<name>` selectors match
`event.agent.adapter` and retained `event.tool.native_name`; names are
case-sensitive and no glob or regex patterns apply. See the
[configuration reference](../configuration.md#events-effects-and-tool-selection).
After normalization, binding checks and the once-only claim, an unmatched step
returns a Host-owned skipped result with no Provider invocation or effects.
Event validation, shared deadlines, cancellation, ordering and completion audit
still apply. The Provider receives only selected invocations and owns its
private tool extraction and backend policy.

Native denial and pending permission requests retain precedence over neutral
or skipped steps. Unsupported argument rewrites remain rejected; a Provider
result grants no native permission. `permission.request` remains vocabulary
without an AW interactive approval implementation.

`aw-host` enforces timeout, event budget and output-limit fields during local
execution. Offline admission itself cannot enforce elapsed time, process cleanup,
native scheduling or failure handling. Passing admission is therefore only one
prerequisite for a runnable native binding.


## Standalone stdio example

[policy.rs](../../../../../src/aw/crates/aw-provider/examples/policy.rs) implements all three
methods as a standalone program. From the repository root, run:

```bash
cd src/aw
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"demo-describe"}' | \
  cargo run --locked -q -p aw-provider --example policy
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"demo-config","config":{"blocked_tools":["example_tool"]}}' | \
  cargo run --locked -q -p aw-provider --example policy
```

Each command starts a separate Provider process. `printf` adds a newline and
closes the pipe; the example reads stdin through EOF before parsing its single
JSON request. The newline is JSON whitespace, not a second message. The first
command prints a JSON description of `check`; the second prints this JSON
acknowledgement on stdout, with field order immaterial:

```json
{"api_version":"aw-provider/v1alpha1","request_id":"demo-config","status":"ok"}
```

The example accepts exactly one private setting, `blocked_tools`, an array of
tool names. Its `invoke` method compares `event.tool.native_name` before execution
and requests `block` for a matching name when that effect was admitted. Other
calls, including after-tool events, return an empty effect list. This is a sample
policy, not a sec-core implementation or a built-in security rule set. Running
the example does not start an Agent or cause the AW library to execute commands.


## PostTool text replacement

A structured `tool.after` operation may request `replace_result` when its trusted
Adapter evidence supports it. It must be the event's last enabled step; earlier
observers, boolean Hooks and native Hooks are retained. A second replacement or
any enabled step after replacement is rejected. Boolean Hooks cannot request
replacement. Provider and Adapter evidence remain separately checked.
Tool selectors preserve this admission order. An unmatched replacement step
returns skipped and leaves the original result intact.
Only the admitted replacement step receives the optional projection; existing
observers keep their original event shape. The Adapter supplies `event.tool.result_projection` containing `text`,
`status`, `content_origin` and an optional shell `command`. The original `result`
and native payload remain available and participate in the request binding.

A `replace_result` effect requires a string `text`. A `block` effect may include
a nonempty bounded `text` reason; all other effect types reject that field. Responses allow at most one replacement and require an input
projection. A missing projection permits only a no-replacement result. The
Adapter replaces its authorized text slot and preserves the native envelope and
failure status. This is a
candidate, not proof that a model consumed it; AW's journal excludes the text.
The schema additions retain the experimental v1alpha1 protocol. Deploy matching
CLI, service and Providers from the same Preview build; older implementations
reject unsupported projections/effects rather than ignoring them.

### Tokenless composition

PreTool returns complete retry suggestions through `block`; each new call passes security and native permissions again. PostTool replaces only authorized text slots.
See the [Tokenless reference](tokenless.md) for RTK ownership, result classification and telemetry.
Current recovery capability is `none`; Schema/BeforeModel and Retrieve seams are not available.
See [Core execution](../../../../../src/aw/docs/design/en/core-execution.md) for the separate plan API contract.
