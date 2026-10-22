# Read-only AgentSight integration

[中文版](../../../zh/integrations/agentsight/agentsight-readonly.md)

The subsequent [evidence/context layer](../../runtime/evidence-context.md) composes this
connector with admitted native identity; the diagnostic command remains
unassociated and independent.

## Scope and sequence

This implements [#7621](https://github.com/agentic-os-org/ANOLISA/issues/7621),
the first slice of AW as the base with AgentSight supplying additional evidence.
`aw-service::agentsight::Client` reads a selected source trajectory.
The connector is a library; it adds no standalone query command. The normal
entrypoint is `aw run`, integrated by #7622 / #7655 as specified in the
[lifecycle design](agentsight-integration.md). Hook dispatch, Provider effects,
local Journal persistence and AgentSight lifecycle keep their existing owners.

| Stage | Issue | Dependency |
| --- | --- | --- |
| Read-only source connector | [#7621](https://github.com/agentic-os-org/ANOLISA/issues/7621) | Existing AgentSight GET APIs |
| Run lifecycle, trusted identity, evidence and bounded context | [#7622](https://github.com/agentic-os-org/ANOLISA/issues/7622) | #7621; coordinate scope admission with #4372 |
| Capability coverage and explicit degradation modes | [#7623](https://github.com/agentic-os-org/ANOLISA/issues/7623) | #7621, #7622 |
| Public incremental AW audit export | [#7624](https://github.com/agentic-os-org/ANOLISA/issues/7624) | #7622, #7623 |
| Idempotent AgentSight audit consumption | [#7625](https://github.com/agentic-os-org/ANOLISA/issues/7625) | #7623, #7624 |

Semantic evaluation, Jev-like models, scoring and action routing belong to phase
two. All-in-one profiles/lifecycle follow separately after the capability and
audit contracts. This slice does not introduce `aw.evidence/v1`, `aw.audit/v1`,
`off`/`auto`/`required` policy admission, or new YAML/service RPC fields.

## Query boundary

The composition layer supplies the configured origin, HTTPS credential file and
an associated source session ID; the connector itself does not establish association.
HTTP and HTTPS use the same ordered GET requests and response checks:

1. `/api/auth/status` must report the Boolean `auth_enabled: true`. This is a
   source-reported configuration fact, not proof that the peer checked a token.
2. `/api/trajectories/{session_id}` returns the selected `ATIF-v1.7` document.
   The ID is encoded as one URL segment, including slash or query-like characters.
   The root session ID must match; the agent name must be nonempty, step IDs
   sequential, and the aggregate step count bounded across embedded subagents.

Compatibility is defined by these API response contracts and error semantics,
reviewed against AgentSight 0.14.0. No product-version allowlist or SemVer range is
inferred. Neither transport calls the localhost-only `/health` endpoint; the
common query API exposes no product version, so `source.version` is always null.
This does not claim that AgentSight advertises a separate API negotiation version.
Missing/invalid auth fields and unsupported ATIF versions fail explicitly; a
trajectory 404 remains unknown record/store availability. Optional extra response
fields do not establish compatibility, identity or coverage.

The client preserves arbitrary source content as JSON. This is a diagnostic
envelope, not canonical AW execution metadata or a complete ATIF semantic
validator. Embedded subagent content remains source-reported.

A reusable connector lives at the service composition boundary. It adds no HTTP,
probe or database dependency to `aw-core`. HTTP uses the repository-established
reqwest family with blocking transport and rustls; its shared timeout includes
request/body transport, while payload parsing is bounded by bytes/depth/steps.
No raw HTTP parser or cross-component deployment client is introduced. The
AgentSecCore client also supports enforcement mutations and has a different
ownership/contract, so AW does not depend on it.

## Authentication and limits

Remote origins require HTTPS with certificate and hostname verification. Plain
HTTP accepts only literal loopback addresses. Userinfo, query strings, fragments,
non-root paths and port zero are rejected.

Literal-loopback HTTP never reads `token_file` or sends an Authorization header;
no credential file is required. `source.authentication: loopback_unauthenticated`
records this mode. AgentSight currently bypasses token checks for these reads,
and another local process can occupy its port. Such a listener is not an
authenticated source; returned data remains untrusted observation.

HTTPS reads credentials from an explicitly selected regular file owned by the
effective user, with no group/other access; symlinks and special files are
rejected. The file is bounded to 4096 bytes and accepts one trailing line ending.
Each HTTPS request supplies a sensitive Bearer header; `bearer_supplied` records
delivery, not server-side enforcement (a proxy can reach AgentSight via loopback).
Credentials never enter command arguments or URLs. The Dashboard token is
coarse-grained; this connector limits its own reads and does not claim a
source-issued read-only role.

Redirects, environment proxies, automatic retries and connection pooling are
disabled. TLS failures and HTTP 401/403 never trigger a plaintext or anonymous
retry. Transport authentication does not establish an AW instance identity.

All requests share one deadline, default 5000 ms, allowed 1..60000 ms. Their
combined bodies share one byte budget, default 1048576, allowed 1..4194304.
At most 1000 steps across the root and subagent documents are accepted. Excess
content is rejected as a whole; the client does not silently truncate. Dropping the caller does not manage any AgentSight worker.
No source readiness, probe activation or kernel/BTF checks run on the Hook path.

## Connector result and failure semantics

`aw.agentsight.query/v1alpha1` is separate from evidence and audit contracts.
It contains `source` (endpoint, nullable product version, `authentication`), `session_id`,
`association`, `coverage` and a tagged `status` outcome.

| Outcome | Library result |
| --- | --- |
| Valid selected document | `status: available`, `step_count`, `trajectory` |
| Empty valid document | Same available outcome with zero steps; coverage remains unknown |
| Detail API returns 404 | `status: unknown`, `reason_code: record_or_store_unavailable` |
| Invalid configuration/authentication, redirect, timeout, transport, schema or bound | Sanitized typed error |

Every returned result has `association: unassociated` and `coverage: unknown`.
Caller-supplied session names are not trusted AW bindings. No match or an empty
trajectory cannot establish absence of activity, verified tool executions, full
coverage or native adoption. Dashboard capability names are not readiness facts.
HTTP 401/403 and other failures never turn into empty successful evidence. Source
error bodies and credentials are excluded from diagnostics.

The library returns selected content to its caller. The connector writes neither
source content nor the query result into the Journal or a cache.
No daemon/socket is created. A failed optional query has no effect on ongoing AW
execution or its existing critical local audit boundary.

## Coexistence and verification

[#7411](https://github.com/agentic-os-org/ANOLISA/pull/7411) owns PostTool
compression, native Adapter ordering, result projection, Provider effects and
packaging. This slice changes none of those execution paths. Shared module,
dependency and documentation additions must be checked against its current head.
Later identity/audit work must consume its final once-only phase identities and
preserve metadata-only local audit, rather than changing scheduling independently.

Real HTTP fixture tests cover request scope, credentials, empty/unknown/error
results, version/identity rejection, shared timeout and byte/step bounds, through the library API. Existing AW gates validate operation without AgentSight/eBPF.
These checks do not certify a live deployed AgentSight, TLS termination setup,
verified native association or eBPF coverage. Rollback removes the optional connector; there is no installed service, schema
migration or source data to undo.
