# Read-only Provider presentation

[中文版](../../../zh/aw/providers/views.md)

AW exposes one desktop snapshot through `aw inspect --config ABS_FILE --json`.
This interface performs no binding, tool event, approval, configuration edit or
service startup. It uses the existing bounded executor and local service client;
it adds no daemon, domain engine, database reader or statistics store.

## Registration and compatibility

Optional `spec.providers.<id>.view` contains `protocol`, `implementation` and a
trusted `transport` using the existing stdio shape. Its executable must be
absolute. Registration is separate from the event transport and opaque private
configuration. It is supported by both the offline validator and package-generated
Tokenless profiles. It does not change event admission or grant native effects.
The shared [example](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml) registers
the same CLI/data directory for both entries.

`aw-provider-view/v1alpha1` is independent of `aw-provider/v1alpha1` and
`aw-service/v1alpha7`. The event request/response schemas remain unchanged.
Use matching Preview binaries/configuration; older binaries can reject the new
registration. Missing/unsupported presentation capabilities do not change
sec-core enforcement, after observers or custom Hook scheduling.

## Executable contracts

The [bundled schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/schemas/view-v1alpha1.schema.json)
contains `$defs/request`, `$defs/response` and `$defs/panel`.
The Rust validator reuses the existing bounded JSON parser to reject duplicate
keys, excessive nesting, oversized messages and overflowing integer literals.
All times with `_ms` are Unix milliseconds; no date-format dependency is needed.
Counts use interoperable nonnegative JSON integers, at most `2^53 - 1`.

| Message | Contract |
| --- | --- |
| describe request | protocol, operation, request_id, provider_id, full config_revision, budget_ms |
| query request | Same identity, plus `view: metrics.summary` and `parameters: {limit: 200}` |
| response | Echo all identity fields including operation, then status, observed_at_ms, code, data |
| describe data | implementation, nullable version, supported views |
| query data | Allowlisted totals/by_operation, explicit store/window scope, unknown recording state and unverified adoption |
| Panel | api_version, generated_at_ms, config_revision, service, providers, recent_execution |

Describe and query share at most two seconds per Provider. A late reply, wrong
identity/implementation, unknown schema field, incompatible version or unsupported
view cannot become a successful metrics result. Commands, URLs, markup, arbitrary
private fields and event effects are outside the response shape. This is trusted
same-user process integration, not a sandbox for a malicious Provider.

Inspect bounds the snapshot to three seconds and 1 MiB. Existing executor cleanup
can add its one-second verification budget and cooperative grace. Tokenless view
mode handles termination by cancelling/reaping its nested CLI process group.
The event entry's existing lifecycle is unchanged. Providers are queried serially
within the snapshot budget; remaining views can time out without extending it.
Consumers coalesce requests and discard late responses rather than overlapping
unbounded pollers. Query failures never change event failure policy.

| Status | Meaning |
| --- | --- |
| ok | This view returned valid data for its stated scope |
| disabled | No enabled event step references this configured Provider |
| unsupported | No registered/supported view, or unsupported statistics-store override |
| no_data | Absent store or no active records in the selected window |
| unavailable | Query, transport, output, schema or identity failure |
| partial | Reserved for partially available consumer snapshots; individual cards retain their actual status |
| stale | Service observation can no longer be confirmed; consumer-retained data must be labelled stale |

Codes are enumerated in `$defs/code`. No-data with an absent store uses
`store_absent`; an empty existing store has no error code. Numeric unknowns are
not fabricated as zeros. Static configuration health does not prove live backend
health, Provider admission, native installation or model adoption. These stay
unknown/unverified unless an independent contract supplies evidence.

## Source boundaries and retention

Tokenless view mode invokes public `stats summary --json --limit 200` using its
explicit data directory, and accepts source schema `1.1`. Missing `stats.db`
is detected before invocation, avoiding empty-store creation. Existing-store
queries can perform component-owned migration/maintenance. Nonempty
`TOKENLESS_STATS_DB` and effective DSH overrides are unsupported until a public
resolved-store contract exists. AW does not reproduce component path resolution.

Validate record limits, safe counts, additive operation totals and before/after
differences; project only the allowlist. Exclude dry runs using the public summary.
Whole-table retrieval attribution is not mixed with this limited compression
window. Store history can include non-AW integrations. No session, daily, billing,
net-savings or native-adoption claim follows from those counts.

The service's additive read-only `recent_executions` operation returns at most 20
completed step projections from its current generation, newest first. A bounded
memory ring is populated only after successful durable append. Presentation-cache
publication shares the journal lock to preserve append order across workers;
cache failure cannot fail the event or change audit health. Reads clone at most 20
entries, holding no lock across Provider I/O. No new persistent history is created;
restart clears the ring while existing keyed durable audit remains available.
Projection includes Provider/step, method, duration, status, failure category,
failure action and structured policy block request. The optional `kind` field
distinguishes `provider`, `native` and `skipped`; a skipped completion starts no
invocation process. Readers also accept projections from services that omit `kind`. No raw input/output, native
environment, replacement contents or approval receipt is retained there.

Inspect pins configuration revision and service generation. It rechecks the same
client after view collection, clearing recent data if service state cannot be
confirmed and refreshing audit health from that final response. A successful
recent result requires data and a null code; other statuses carry null data.
Component-store history is independent of a live AW session. Existing
Tokenless SQLite/SLS retention remains a separate boundary; excluding bodies from
Panel JSON does not delete or disable those sinks.

## Regression and client fixtures

[Synthetic fixtures](https://github.com/agentic-os-org/ANOLISA/tree/main/src/aw/crates/aw-provider/examples/views) cover both methods
and six Panel states. They do not establish native adoption. Tests exercise
strict validation, subprocess limits/cancellation, no-start/no-initialization,
identity/configuration mismatch, generation reset, bounded history and preserved
blocking. Package CI additionally seeds statistics through the real public CLI,
verifies dry-run exclusion and compares the view against the same summary.
