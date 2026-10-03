# Provider protocol and capability admission

[中文版](provider-protocol_zh.md)

`aw-provider` defines the experimental `aw-provider/v1alpha1` interface for
external policy programs and checks their messages and requested capabilities
offline. It connects the unified configuration to a Provider contract without
starting a process, installing an Agent Hook or establishing a runtime binding.
The same Provider interface is intended for QwenPaw, Qoder CLI, OpenClaw and
Hermes; this delivery uses synthetic fixtures, not new native integration evidence.

## Delivery boundary

| Layer | Current responsibility |
| --- | --- |
| `aw-config` | Parse one configuration and check its structure and static references |
| `aw-provider` | Validate protocol messages, correlate responses and check configured steps against supplied Provider and Adapter evidence |
| `aw-contracts` / `aw-core` | Validate and execute their existing capability profiles through a trusted Host and Journal |
| Future bounded Host | Execute Provider commands, enforce deadlines and output limits, handle cancellation and collect execution evidence |
| Future Adapter and service | Authenticate native capabilities, register callbacks, translate effects, manage bindings and audit native adoption |

Configuration fields are unchanged. A named object under `spec.providers`
selects `aw-provider/v1alpha1`, a literal `transport.argv` and private `config`.
A script, binary or CLI client of a background service can implement this
interface. AW does not infer or install that program's dependencies.

The [configuration example](../../crates/aw-config/examples/aw.yaml) illustrates
all 16 event names and later capabilities. Static validation of that example is
not a promise that its enabled events, guard or replacement effects pass the
narrower admission checks described here.

## Transport contract

The execution contract is one process per method: the caller launches the
configured argv, writes one UTF-8 JSON object to stdin and closes stdin. The
Provider returns exactly one JSON object on stdout; diagnostic text belongs on
stderr. There is no implicit shell expansion. D1b specifies and validates the
messages; the process runner is a later delivery.

A policy block is a successful result and requires exit status zero. A nonzero
exit means execution failed, regardless of stdout. Invalid JSON, oversized
messages, mismatched identities and invalid output effects are failures, not
policy decisions. The Host must distinguish those failures when applying the
configured failure policy; this offline library does not apply native effects.

The request and response schemas are bundled with the crate:

- [Request schema](../../crates/aw-provider/schemas/request-v1alpha1.schema.json)
- [Response schema](../../crates/aw-provider/schemas/response-v1alpha1.schema.json)

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

| Request | D1b result |
| --- | --- |
| `tool.before`: `observe`, `block` | Accepted when the selected operation and Adapter both support every requested effect |
| `tool.after`: `observe` | Accepted when both sides support it |
| `ask`, input/result replacement or a block after execution | Rejected |
| An enabled event outside the two tool events | Rejected, including when `required: false` |
| A guard such as `security.violation` | Rejected; final-check ordering is not implemented by this profile |
| Exact tool selectors or a guarantee beyond `native_hook` | Rejected |
| A missing Provider, operation, capability or matching configuration validation | Rejected with an explicit reason |

Before-tool `on_error` may be `report`, or `block` when the Adapter supports
blocking. After-tool failures support `report`. Other failure actions are rejected.

`required: false` does not authorize silently dropping an enabled unsupported
step. All configured tools are eligible through the wildcard selector; the
Provider can inspect tool names and arguments in its own operation. The presence
of `permission.request` in the vocabulary does not imply support for an
interactive approval UI in any particular framework or entrypoint.

Timeout, event budget and output-limit fields remain obligations for the future
Host. Offline admission cannot enforce elapsed time, process cleanup, native
serial/parallel scheduling, failure handling or an event-wide deadline. Passing
admission is therefore only one prerequisite for a runnable binding.

## Relationship to Core and security enforcement

The current Core profile admits four capabilities:
`security.command.inspect/v2`, `security.code.inspect/v2`,
`security.content.inspect/v2` and `context.projection.prepare/v2`. Its pre-tool
plan requires a mandatory command inspection and a `required_final_guard`;
dispatch validation additionally checks the plan's OS requirement against fresh
native evidence. Those contracts remain unchanged.

A generic native Hook Provider returning `block` does not satisfy that command
inspection or final-guard contract. Its `describe` response is not an admitted
`provider-descriptor-v1`, and its invocation response is not a Host-generated
`provider-receipt-v1`. The `denied` receipt disposition represents an unavailable
or denied capability execution, not a successful security verdict: a successful
security inspection that rejects a command produces an output with `verdict:
"deny"` and receipt disposition `produced`.

D2 must introduce an explicitly reviewed, versioned native-Hook profile before
wiring these generic operations into Core. Its Host must authenticate Provider
identity, execute the transport, validate candidate effects, translate the
selected profile and create correlated execution evidence for Core and Journal.
The existing security profile retains its stronger final-dispatch requirements.
Native readback is still required to establish that the framework adopted a
returned effect. Neither a Provider response nor a journaled call proves that.

## Standalone stdio example

[policy.rs](../../crates/aw-provider/examples/policy.rs) implements all three
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

## Joint delivery with sec-core

sec-core can implement the three protocol methods and private policy schema
against these schemas before the AW process runner exists. It owns built-in
rules, custom policy evaluation and the meaning of a tool-specific safety
verdict. A CLI wrapper may connect to a separately managed sec-core service.
The wrapper must translate scanner outcomes into declared Provider effects and
separate scanner failure from a successful policy block.

AW owns protocol validation and admission now. Later deliveries add the bounded
Host, service lifecycle, authenticated Adapter capabilities, native Hook
translation and audit integration. Final `security.violation` composition and
its relation to sec-core are part of that later enforcement design; this slice
does not claim to supply a last, non-bypassable security check.

The four-framework fixtures verify one common message shape and capability
rejection behavior. They do not establish that all native entrypoints load Hooks,
that a framework honors a returned block, or that execution cannot bypass a
callback. Those claims require separate runtime acceptance.
