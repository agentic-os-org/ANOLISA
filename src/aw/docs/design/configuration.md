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

The configuration schema is the authoritative field shape. A reusable offline
validator parses bounded YAML into JSON, validates that shape, then checks static
relationships. `Configuration::as_value` exposes the validated document; there is
no duplicate public Rust field model or implicit default insertion. Provider-owned
objects preserve finite JSON decimals and Unicode keys; existing wire encoding
remains unchanged. Configuration revisions and their digest representation are
not defined by this increment.

Static checks reject unknown public fields, duplicate keys and step IDs,
unresolved Provider/guard references, invalid tool selectors, mismatched event
effects/failure actions and active `ask` steps. `security.violation` keeps the
existing POC event vocabulary for an active final internal check; only an enabled
tool-before guard can activate it. This does not establish global Hook ordering.

The next protocol increment must define Provider `describe`, `validate_config`
and `invoke` contracts, private-schema validation and effect admission. Names in
this configuration do not prove that an operation exists. Control effects and
their failure actions cannot be discarded via `required: false`. A runtime must
also distinguish unsupported optional observation from installed/connected/
triggered/adopted state, enforce shared budgets and validate actual consumption.

The service, four framework adapters, sec-core integration, installer and
auditing follow those contracts. cosh and Herdr remain later clients of the public
service interface; no field or library dependency here requires them.
