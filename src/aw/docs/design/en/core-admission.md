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

## Offline deployment descriptors

`aw_core::descriptor::compile(bytes, target, capabilities, availability)` runs
static admission against a trusted existing Adapter profile and produces an
`aw_contracts::deployment::DeploymentDescriptor`. The deployment contract is
registered as `deployment-descriptor-v1alpha1`; its `version` is
`aw-deployment/v1alpha1`. Serialize the returned value with `serde_json`; parse
stored bytes with `DeploymentDescriptor::parse`, which rejects duplicate keys,
unknown fields, invalid shapes and unsupported versions.

The descriptor carries only the exact-byte `configuration_sha256`, configured
`target`, profile `adapter`/`version`/`entrypoint`, absolute `endpoint`,
`wait_for_input_submit` and `availability`, plus the contract version. It excludes
Provider configuration and execution plans. An `auto` endpoint resolves to
`state_dir/aw.sock`; compilation rejects unresolved or relative service paths.
The compiler reads no environment, starts no process and performs no installation.

An enabled `input.submit` requires input waiting, including an optional event
with an empty step list. Static admission still checks that the supplied profile
supports it. `NativeFallback` conflicts with any globally enabled required event,
matching Service availability checks even when selectors exclude that event
from this target. Adapters own profile identities, capability declarations and
stricter service requirements. The CLI derives its profile choices from the
Adapter declarations. Native SDK bindings pass their expected identity and service
requirement to the shared readers, which validate them without framework-name
branches. Core applies the same static admission to every supplied profile.

`descriptor::verify` recompiles the exact configuration bytes, target, capability
profile and availability choice and compares every hint. Use this during
production of trusted deployment artifacts. At startup, SDK consumers must
check the stored configuration digest and expected target/profile identity
before using hints; a digest identifies configuration bytes and does not
authenticate a modified descriptor. Runtime Service admission, Provider checks,
Ready and native adoption remain required. Source selection and task Overlay
revisions are separate future consumers of requirement projection.

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
