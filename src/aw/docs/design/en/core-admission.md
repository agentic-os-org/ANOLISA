# Core Hook policy admission

[中文版](../zh/core-admission.md)

[Design index](README.md) · [Architecture](architecture.md)

`aw_core::admission` owns Hook policy admission rules, admitted plan types and
target-specific requirements supplied by `aw_config::requirements`. It binds configuration parsing, the exact-byte
SHA-256 revision, the target Agent, trusted Adapter capabilities and admitted
Hook steps into immutable Core types while exposing pure rule helpers for
callers that already hold a validated `aw_config::Configuration`.

Production Hook preparation in `crates/aw-service/src/runtime.rs` and
`crates/aw-host/src/lib.rs` runs through this API via the sequence
`AdmissionEngine::preflight → Host::discover → AdmissionEngine::admit → Host::from_admitted`.
`Host::prepare` delegates to the same pipeline for standalone callers, while
native Hook event execution continues along `Adapter → Service → Host → aw-exec → Provider`.

## Admission stages and types

`AdmissionEngine` compiles the bundled `aw-config` schema once and exposes two
pure transitions alongside direct `Configuration` helpers:

| Stage | Entry | Input | Output |
| --- | --- | --- | --- |
| Static preflight | `AdmissionEngine::preflight` / `admission::preflight` | Raw configuration bytes (or validated `Configuration`), target name, trusted `AdapterCapabilities` | `PolicyCandidate` / `Vec<AdmittedStep>` |
| Evidence admission | `AdmissionEngine::admit` / `admission::admit` | Consumed `PolicyCandidate` (or validated `Configuration`), protocol-checked `BTreeMap<String, ProviderEvidence>` | `AdmittedPolicy` / `Vec<AdmittedStep>` |
| Selector projection | `admission::projected_out_steps` | Validated `Configuration`, target name | `Vec<ProjectedOutStep>` |

`AdmissionEngine::preflight` parses the input bytes with `aw_config::Validator`,
computes the lowercase hexadecimal SHA-256 digest of those exact bytes, and runs
`admission::preflight` to validate target existence, event/effect support, tool
selector intersections, transport protocols and failure actions. Semantically
equal documents with different byte encodings keep distinct revisions, matching
`aw-host::Host::revision`.

`AdmissionEngine::admit` consumes the `PolicyCandidate` by value so the
validated `aw_config::Configuration`, revision, target and `AdapterCapabilities`
remain bound to the same snapshot. It checks Provider operations, events,
effects and private-configuration evidence against the preflighted
`PolicyCandidate` requirements without repeating static preflight. Steps using
`aw-hook/v1alpha1` or `native-hook/v1alpha1` require no `ProviderEvidence`
entry. `ProviderEvidence` remains in `aw-provider` as the protocol evidence
carrier (`Description` and `ValidatedConfig`) and is re-exported by
`aw_core::admission`.

`AdmissionError` wraps `aw_config::Error` and carries
`AdmissionError::Rejected` for Core-owned admission rule rejections without
changing `aw_core::Error` used by `Core::prepare` and `Core::execute`.

## Desired requirement projection

`aw_config::requirements::project(&configuration, target)` returns borrowed
`HookRequirement` events and their ordered `StepRequirement` entries. It exposes
the effective event budget, availability requirement, event and step selectors,
Provider reference, effects and separate failure action. The original validated
event and step fields remain accessible for admission checks of guards,
protocols, operations and native steps. Projection requires no entry mode,
Provider evidence, process, socket or installation.

Static admission consumes this projection. Disabled events and steps are omitted;
selector intersections proven disjoint for the configured Adapter are excluded.
Exact foreign native selectors cannot match. Normalized/native alias pairs stay
applicable until the Adapter supplies an actual tool identity. Filtering preserves
step order and does not change the runtime intersection of event and step
selectors. An enabled event with no enabled steps still requires capability
admission, even with `required: false`; an event whose enabled steps are all
projected out is omitted. Event budgets use `execution.default_event_budget_ms`
when no event override is present. Native steps retain their raw semantics and
have no invented Provider effects.

The projection describes desired policy and carries no capability proof. Source
resolution and deployment compilation can consume it alongside the existing
Admission API; runtime registration and execution still use admitted policy.

## Invariants and runtime boundaries

`PolicyCandidate` and `AdmittedPolicy` keep all fields private, omit `Deserialize`
and public struct constructors, and expose read-only accessors
(`configuration`, `revision`, `target`, `capabilities`, `providers`,
`requirements` and `steps`). Callers cannot inject unverified `AdmittedStep`
values or swap the configuration or Provider evidence between preflight,
discovery and Host construction. `Debug` formatting omits the parsed
configuration document and Provider evidence so Provider-private values do not
leak into diagnostics.

`AdmittedPolicy` records capability compatibility for the supplied snapshot only:

- `aw-host` remains responsible for bounded `describe` and `validate_config`
  exchanges, runtime file leases, Provider process execution and cleanup.
- `aw-service` remains responsible for active bindings, scopes, step-count and
  call budgets, policy activation gates and durable audit records in
  `aw_core::journal::FileJournal`.
- `aw-adapter` remains responsible for native Hook registration, callback
  verification and actual effect adoption in the target Agent.
