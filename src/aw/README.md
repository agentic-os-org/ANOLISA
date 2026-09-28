# AW

[中文版](README_zh.md)

AW provides unified configuration, versioned capability contracts and embeddable Core orchestration. `aw-config` validates configuration structure and references; `aw-provider` checks external Provider messages and capability admission offline; `aw-contracts` checks payload shapes and record relationships; `aw-core` executes pinned plans through caller-provided Hosts and journals execution facts. AW has no service process; native Agent control and final tool dispatch remain with the embedding application.

The interfaces are experimental. Tests use synthetic records and do not certify runtime integration.

## Validate a configuration

From the repository root, use the offline example to check a configuration:

```bash
cd src/aw
cargo run --locked -p aw-config --example validate -- crates/aw-config/examples/aw.minimal.yaml
```

A successful result confirms configuration syntax and static references. It does
not start an Agent or enable a policy. See the [configuration guide](../../docs/developer-guide/en/aw/configuration.md)
for fields and examples, and [Contributing to AW](CONTRIBUTING.md) for development
setup, tests and CI.

## Core embedding

`aw-core` provides `Core::prepare` and `Core::execute`, trusted Host/Clock/Journal
ports, and a durable Linux `FileJournal`. Preparation checks the complete plan
before any provider call. Execution records each call before dispatch and returns
terminal results only after the journal acknowledges them. Failed or interrupted
events remain reserved; there is no automatic retry or recovery.

See [Core execution and storage](docs/design/core-execution.md) for ownership,
cancellation, failure and embedding contracts. Core tests use synthetic Hosts;
native Agent integration and effect adoption require separate runtime validation.

## Source reference

- [User guide and availability](../../docs/user-guide/en/user-entrypoint/aw.md),
  [configuration reference](../../docs/developer-guide/en/aw/configuration.md),
  [starter configuration](crates/aw-config/examples/aw.minimal.yaml),
  [full example](crates/aw-config/examples/aw.yaml) and
  [configuration API](crates/aw-config/src/lib.rs)
- [Registered schemas](schemas/) and [synthetic payload examples](tests/fixtures/contracts.json)
- [Public API](src/lib.rs), [record validation](src/validation.rs) and [plan validation](src/orchestration.rs)
- [External Provider protocol and admission](docs/design/provider-protocol.md)
  and [Provider API](crates/aw-provider/src/lib.rs)
- [Encoding tests](tests/canonical.rs), [schema tests](tests/schemas.rs),
  [record tests](tests/contracts.rs) and [plan tests](tests/orchestration.rs)

The Registry includes 21 schema resources. The eight v1 resources in `crates/aw-contracts/schemas/` are reference copies and are not registered. Callers must use matching schema IDs and digests; no automatic version conversion is provided.

Parse incoming wire records with `canonical::parse` before schema validation. Shape checks alone do not validate record relationships or grant authorization. Follow the public API documentation for plan-level checks; callers remain responsible for authenticating evidence and enforcing actions.

User configuration uses the separate `aw-config` crate and its bundled
`aw/v1alpha1` schema. It accepts one `AWConfiguration` object with
`apiVersion`, `kind`, `metadata` and `spec`; Provider instances are named objects
under `spec.providers`. The schema recognizes QwenPaw, Qoder CLI, OpenClaw,
Hermes and all 16 event names, without claiming adapters are implemented.
Configuration has no runtime `status`. `aw-provider` validates externally supplied
operation/private-config responses and admits tool steps against caller-trusted
Adapter capabilities. It does not execute discovery or establish native adoption.
Provider execution and binding installation remain subsequent work.
See the [configuration design](docs/design/configuration.md) for the separation
from the existing wire contracts.
