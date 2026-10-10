# Unified configuration boundary

[中文版](configuration_zh.md)

`aw-config` owns desired configuration parsing. `aw-contracts` continues to own
capability wire schemas, canonical encodings and record invariants. Configuration
must not change those schemas or make the existing registry accept arbitrary
Provider JSON as canonical wire metadata.

The `AWConfiguration` envelope separates version, object kind and identity from
`spec`. Runtime status is not accepted as desired input. This borrows a
declarative object shape without introducing Kubernetes APIs or dependencies.
Provider instances are named objects referenced by ordered event steps; one
implementation can have multiple instances with different private configuration.

The schema is the authoritative field shape. A reusable offline validator parses
bounded YAML into JSON, validates that shape, then checks static relationships.
`Configuration::as_value` exposes the validated document; there is no duplicate
public Rust field model or implicit default insertion. Provider-owned objects
preserve finite JSON decimals and Unicode keys; existing wire encoding remains
unchanged. The Host and service derive their revision from the complete original
configuration bytes, so even formatting changes select a different revision.

## Provider and native step forms

A structured Provider uses `protocol: aw-provider/v1alpha1`; its step declares
`operation` and `effects`. Preparation performs `describe` and `validate_config`
then checks requested effects against trusted Adapter capabilities. The current
runtime admits before-tool `observe`/`block` and after-tool `observe`.

An existing native Hook command uses `protocol: native-hook/v1alpha1`, an empty
`config` object and steps with `native: {}`. Those steps omit `operation` and
`effects`; mixing both forms is rejected. There is no structured handshake for
a native command. The Host retains its byte output and native exit status for
the Adapter to relay, with the same bounded execution and audit metadata as
structured steps. Native output does not claim portable AW effects.

`spec.agents.<id>.qoder.sequential` is an optional Qoder-only setting applied to
the generated native Hook groups. It belongs to the Adapter configuration,
not to a Provider or the shared execution policy. Qoder retains control of how
all matching native groups are scheduled; false cannot force parallel execution
when another group requests sequential execution.

Static checks reject unknown public fields, duplicate keys and step IDs,
unresolved Provider/guard references, invalid tool selectors, mismatched event
effects/failure actions, mismatched step protocols and active structured `ask`
steps. `security.violation` remains a reserved active-final-check design; the
current runtime rejects guards and does not establish global Hook ordering.

Control effects and their failure actions cannot be discarded via
`required: false`. Runtime admission and native adoption are separate stages.
The Qoder launcher connects the supported tool callbacks; QwenPaw, OpenClaw and
Hermes adapters, sec-core integration and package installation remain subsequent
work. cosh, desktop clients and Herdr can independently use the public service
interface; no field or library dependency requires them.

See the [complete field reference](../../../../docs/developer-guide/en/aw/configuration.md)
and [local service contract](local-service.md) for runtime limits and callback
correlation. Sequential input-rewrite chains and portable approval remain outside
the current native adapter contract.
