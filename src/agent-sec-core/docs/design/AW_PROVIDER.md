# AW Provider boundary

[中文版](AW_PROVIDER_zh.md)

`agent-sec-cli aw-provider` is a new, versioned consumer of the existing V2
`action.code_scan` method. It does not change a V1 compatibility surface, daemon
method, scanner rule, principal, audit schema, service topology or deployment
contract. Rust remains the only product runtime in this path.

```text
AW configuration + normalized event
  -> AW Host: admission, process/output bounds, audit
  -> agent-sec-cli aw-provider: aw-provider/v1alpha1 + private config
  -> existing asc-daemon-client -> action.code_scan
  -> existing Action Runtime -> Code Scanner + scanner audit
  -> Provider candidate effect -> AW adapter -> native Agent decision
```

AW owns Agent adaptation, configuration revision binding and Provider-call
history. The sec daemon owns security execution and its existing audit lifecycle.
AW does not replace the sec engine or use its database as a deployment registry.
This slice requires no AW daemon, Provider Host implementation or Agent adapter
dependency to compile or test. Actual hook adoption is a later joint acceptance.

## Protocol and policy

The runtime reuses the already merged `aw-provider` offline contract crate by
path. That crate validates request size (1 MiB), depth (32), duplicate keys,
unknown fields, effect scope, identity and input digest. This is a protocol
dependency, not a dependency on `aw-host`, an AW daemon or a concrete Agent.
Successful responses echo the opaque digest; they never manufacture receipts.

The CLI reads one request to EOF and writes one response. AW must close stdin
and enforce the outer execution deadline; the Provider bounds bytes and applies
the remaining invocation budget to the existing daemon client's connect/write/read
deadline. The CLI timeout may shorten that budget. Late results are discarded,
and a transport timeout never triggers replay. Client cancellation cannot undo a
sec daemon scan or guarantee that server work has stopped; the server retains
its own execution control and audit.

`describe` advertises `scan_code` for `tool.before` with `observe/block`, and
`observe_tool` for `tool.after` with `observe`. `validate_config` checks only
private configuration; it does not probe or certify backend availability.
`invoke` revalidates that configuration. Version 1 requires an explicit mode
and a map from normalized tool name to a Bash/Python language plus input JSON
Pointer. `tool_unmapped` is an observation of missing coverage, not an allow
verdict. A matched mapping with invalid input fails instead of skipping.

Blocking is opt-in and uses the existing Code Scanner hook `enable_block`
threshold: `warn` or `deny`. No rule or verdict is changed. `observe_tool`
acknowledges the after-tool event without invoking a scanner. Policy blocks exit zero
with `status: ok`; scanner, daemon and transport faults exit zero with
`status: error` and no effects, leaving AW's `on_error` policy explicit.
Malformed wire input or stdio failure exits nonzero. Raw evidence and input
are absent from both response and Provider diagnostics.

Source0 includes the tracked AW workspace under `third_party/aw`; the sec-core
packaging helper changes only the staged path dependency. Source1 still vendors
registry dependencies through the existing V2 lockfile. This keeps packaged
builds independent of the monorepo checkout without maintaining a second copy
of the protocol implementation in source control.

## Acceptance and compatibility

| Slice | Executable evidence |
|---|---|
| Shared AW protocol | `asc-cli/tests/aw_provider.rs`: describe/validation, binding tamper, duplicate/size/depth rejection, checked responses. |
| Private configuration | Invalid/unknown fields, explicit modes, exact names, nested/escaped/root JSON pointers, missing coverage and input errors. |
| Backend projection | Bounded mock daemon requests; pass/warn/deny/error, malformed verdicts, method/transport failure, timeout and effect admission. |
| Real consumer | `asc-cli/tests/pap_process.rs::aw_provider_uses_real_daemon_rules_and_preserves_protocol_outcomes`: real CLI → daemon → shipped scanner rules; clean, risk/block, observe and failed scan. |
| Packaged source | `tests/packaging/test-aw-source.py`: create and extract Source0, verify every local Cargo package stays inside the archive, build with `--offline --locked`, then run Provider discovery. Registry cache must be populated before the offline check. |
| Existing CLI compatibility | Existing `asc-cli` command, context, output and process tests remain unchanged in meaning. No new daemon protocol fixtures are required because its wire method is unchanged. |

Run from `src/agent-sec-core/v2` with the repository CI Rust toolchain:

```sh
cargo +1.93.0 fmt --all -- --check
cargo +1.93.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.93.0 test --workspace --locked
cargo +1.93.0 doc --workspace --no-deps --locked
```

The [user guide](../../../../docs/user-guide/en/agent-security/agent-sec-core/aw-provider.md)
and [configuration example](../../v2/examples/aw.yaml) define the current
consumer interface. Custom policy scripts, PII, final guards and OS enforcement
are outside this slice. Rollback removes the AW Provider steps/configuration;
existing sec daemon state and native sec-core hooks require no migration.
