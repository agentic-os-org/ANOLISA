# AgentSight Audit Crate Boundary

## Status

Approved for implementation on 2026-08-02.

## Context

AgentSight currently keeps normalized security-event models, SQLite persistence,
risk-case correlation, runtime event subscription, containment orchestration,
and HTTP handlers under the main `agentsight` crate. The implementation works,
but the audit domain cannot be tested or reused without compiling the Linux
runtime and server integration layers.

The extraction must preserve the existing `/api/audit/*` contract, leave
Security Observability unchanged, and continue to ship as one AgentSight
product. It must not add a separately deployed audit daemon.

## Decision

Create an `agentsight-audit` workspace crate that owns:

- normalized audit query and risk-case models;
- SQLite schema, immutable event persistence, case correlation, review state,
  containment lifecycle persistence, and retention;
- an `AuditService` application API for ingest, query, review, and evidence
  retrieval;
- typed errors that do not depend on Actix, eBPF probes, or the concrete
  ActPlane adapter.

The main `agentsight` crate continues to own:

- eBPF and UDS event collection;
- runtime subscription lifecycle and reconnect behavior;
- Actix request parsing, authentication, and HTTP response mapping;
- process identity discovery and health information;
- containment execution adapters and the concrete enforcement coordinator.

`agentsight serve` constructs one `AuditService` and places it in `AppState`.
The existing audit routes delegate to that service. Runtime code delivers
normalized `SecurityEvent` values to the same service. No second audit process
or network hop is introduced.

## Dependency Direction

```text
agentsight server/runtime
          |
          v
  agentsight-audit
          |
          v
agentsight-enforcement-protocol
```

The audit crate must not depend on the main `agentsight` crate, Actix, probes,
or `agentsight-enforcer`. The main crate may provide adapters around UDS and
ActPlane, but the audit domain only accepts normalized protocol events and
typed requests.

## Crate Layout

```text
crates/agentsight-audit/
├── Cargo.toml
├── src/
│   ├── lib.rs          # public domain surface
│   ├── model.rs        # filters, pages, cases, containment records
│   ├── service.rs      # ingest, correlate, query, review
│   └── store.rs        # SQLite repository and schema
│       ├── containment.rs
│       └── retention.rs
└── tests/
    ├── service_contract.rs
    └── store_contract.rs
```

Rust 2018+ file layout is mandatory; no `mod.rs` files are introduced.

## Public API

The crate exposes `AuditService`, `AuditStore`, `AuditError`, query models,
risk-case models, and containment persistence models. `AuditService` owns an
`Arc<AuditStore>` so callers share one SQLite connection and one correlation
boundary.

```rust
pub struct AuditService {
    store: Arc<AuditStore>,
}

impl AuditService {
    pub fn new(store: Arc<AuditStore>) -> Self;
    pub fn ingest(&self, event: SecurityEvent) -> Result<(), AuditError>;
    pub fn summary(&self, filter: &AuditEventFilter) -> Result<AuditSummary, AuditError>;
    pub fn events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, AuditError>;
    pub fn sessions(&self, filter: &AuditEventFilter) -> Result<AuditSessionPage, AuditError>;
    pub fn cases(&self, limit: usize, offset: i64) -> Result<AuditCasePage, AuditError>;
    pub fn case_detail(&self, case_id: Uuid) -> Result<RiskCaseDetail, AuditError>;
    pub fn review_case(
        &self,
        case_id: Uuid,
        status: RiskCaseStatus,
        reviewed_at_ns: u64,
    ) -> Result<RiskCase, AuditError>;
    pub fn store(&self) -> &Arc<AuditStore>;
}
```

Existing `SecurityStore`, `SecurityStoreError`, and `SecurityEventFilter`
imports remain source-compatible through deprecated-free re-exports in the
main crate during this PR. New code uses the `Audit*` names.

## Runtime Integration

The current `SecurityCoordinator` becomes a runtime adapter rather than the
owner of correlation logic. It keeps the UDS client, thread lifecycle, retry,
and last-context handling, then calls `AuditService::ingest` for each normalized
event. Correlation and idempotency move into the audit crate.

Containment execution remains in the main crate because it depends on live
process identity, enforcement leases, replacement transitions, and the concrete
enforcer client. Its durable action records remain in `AuditStore`, accessed
through public persistence methods. This preserves the audit trail without
pulling privileged runtime dependencies into the audit crate.

## HTTP Compatibility

The following routes retain their paths, methods, authentication, query
parameters, status codes, and JSON shapes:

- `GET /api/audit/summary`
- `GET /api/audit/events`
- `GET /api/audit/sessions`
- `GET /api/audit/cases`
- `GET /api/audit/cases/{case_id}`
- `POST /api/audit/cases/{case_id}/review`
- containment plan and activation routes under `/api/audit/cases/{case_id}`

`server/system_audit.rs` remains a thin Actix adapter. It parses HTTP inputs,
calls `AuditService`, and maps `AuditError` to the existing response envelope.
It contains no SQL or correlation behavior.

## Storage Compatibility

The database remains `security.db` and the schema is additive and compatible.
No migration renames tables or columns. Existing data, event IDs, correlation
keys, case IDs, and containment records remain valid after upgrade.

Production file ownership and permission checks stay in the host crate. The
main crate opens the private SQLite connection and constructs `AuditStore` from
that connection; the audit crate owns schema and query behavior.

## Error Handling

`AuditError` is a named `thiserror` enum covering SQLite failures,
serialization, invalid filters, missing evidence, missing cases, invalid stored
data, timestamps, and poisoned locks. Runtime subscription errors stay in the
main crate. HTTP handlers map audit errors without exposing database paths,
policy DSL, or sensitive evidence content.

## Testing

- Crate unit and integration tests verify schema compatibility, event
  idempotency, deterministic case correlation, pagination, review transitions,
  retention, and containment lifecycle persistence.
- Main-crate integration tests verify UDS delivery delegates to
  `AuditService` and existing audit HTTP responses remain byte-compatible at
  the JSON field level.
- Security Observability regression tests must remain unchanged and pass.
- Linux verification runs formatting, Clippy, workspace tests, and the existing
  AgentSight integration suite.

## Alternatives Considered

### Keep audit inside the main crate

Rejected because it preserves the current coupling to Linux runtime and Actix
and does not meet the independent extension goal.

### Deploy audit as a separate service

Rejected because it adds lifecycle, authentication, network, and failure-mode
complexity without a current scaling requirement.

### Move containment execution into the audit crate

Rejected because concrete process and ActPlane orchestration would reverse the
desired dependency direction. The audit crate owns durable evidence; the main
crate owns privileged execution adapters.

## Non-goals

- No new public route or CLI command.
- No change to the Security Observability page or `/api/security/*` routes.
- No database format migration or data backfill.
- No independent audit daemon.
- No direct AgentLoop integration in this refactor.
