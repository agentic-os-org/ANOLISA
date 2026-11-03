# Core Hook policy admission

[中文版](../zh/core-admission.md)

[Design index](README.md) · [Architecture](architecture.md)

`aw_core::admission` provides a side-effect-free Hook policy admission API over
one configuration snapshot. It binds configuration parsing, the exact-byte
SHA-256 revision, the target Agent, trusted Adapter capabilities and admitted
Hook steps into immutable Core types while keeping `aw_provider::admission` as
the single rule implementation.

On `agentic-os-org/ANOLISA` `main`, production Hook preparation in
`crates/aw-service/src/runtime.rs` and `crates/aw-host/src/lib.rs` still calls
`aw_config::Validator` and `aw_provider::admission::{preflight, admit}`
directly. This module establishes the Core API and type boundary first;
switching `aw-service` and `aw-host` to consume these types is tracked as the
next migration step.

## Admission stages and types

`AdmissionEngine` compiles the bundled `aw-config` schema once and exposes two
pure transitions:

| Stage | Entry | Input | Output |
| --- | --- | --- | --- |
| Static preflight | `AdmissionEngine::preflight` | Raw configuration bytes, target name, trusted `AdapterCapabilities` | `PolicyCandidate` |
| Evidence admission | `AdmissionEngine::admit` | Consumed `PolicyCandidate`, protocol-checked `BTreeMap<String, ProviderEvidence>` | `AdmittedPolicy` |

`preflight` parses the input bytes with `aw_config::Validator`, computes the
lowercase hexadecimal SHA-256 digest of those exact bytes, and delegates static
target, event, effect, selector, transport and failure-action checks to
`aw_provider::admission::preflight`. Semantically equal documents with different
byte encodings keep distinct revisions, matching `aw-host::Host::revision`.

`admit` consumes the `PolicyCandidate` by value so the validated
`aw_config::Configuration`, revision, target and `AdapterCapabilities` remain
bound to the same snapshot. It delegates Provider operation, event, effect and
private-configuration checks to `aw_provider::admission::admit`. Steps using
`aw-hook/v1alpha1` or `native-hook/v1alpha1` require no `ProviderEvidence`
entry.

`AdmissionError` wraps `aw_config::Error` and `aw_provider::Error` without
changing `aw_core::Error` used by `Core::prepare` and `Core::execute`.

## Invariants and runtime boundaries

`PolicyCandidate` and `AdmittedPolicy` keep all fields private, omit `Deserialize`
and public struct constructors, and expose read-only accessors
(`configuration`, `revision`, `target`, `capabilities`, `requirements` and
`steps`). Callers cannot inject unverified `AdmittedStep` values or swap the
configuration between preflight and Provider admission. `Debug` formatting omits
the parsed configuration document so Provider-private values do not leak into
diagnostics.

`AdmittedPolicy` records capability compatibility for the supplied snapshot only:

- `aw-host` remains responsible for bounded `describe` and `validate_config`
  exchanges, runtime file leases, Provider process execution and cleanup.
- `aw-service` remains responsible for active bindings, scopes, step-count and
  call budgets, policy activation gates and durable audit records in
  `aw_core::journal::FileJournal`.
- `aw-adapter` remains responsible for native Hook registration, callback
  verification and actual effect adoption in the target Agent.
