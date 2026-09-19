# AgentSecCore V2 Policy and daemon foundations

This workspace slice contains the dependency-light contracts, Policy
Administration Point, first-version PAP daemon protocol, product Policy-template
compiler, protocol-independent Unix-domain-socket service framework, and runnable
foreground process bootstrap, together with the first AgentSight file-deletion
target Adapter, and its independent deployment Client used by later AgentSecCore
V2 work packages. `asc-pcp` provides synchronous attempts and
`asc-policy-runtime/src/reconciliation/` provides the bounded Binding queue,
workers, retry timers and compensation scanning. Storage remains process-local.

Each attempt re-reads current state and prepares afresh. Reconciliation writes
update specific fields without carrying spec; no plan, prepared request or pending
outcome is stored across calls. See the [runtime design (Chinese)](../docs/design/BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)
for retry, ownership and staged acceptance.

Start the daemon with background Binding delivery:

```sh
asc-daemon serve --socket /run/agent-sec-core/daemon.sock
```

The daemon entrypoint starts the reconciliation service. Its `reconciliation.rs`
composition module registers `AgentSightClientFactory::default()` without reading
credentials or contacting the PEP. Each due attempt opens its own Client and reuses
it through preparation and create/update, or through deletion for a saved route.
Environment-based PEP selection is deferred. The Client owns endpoint/token-file
defaults; daemon and CLI expose no AgentSight configuration options. Missing or
invalid credentials are retryable Binding failures; each retry reads the token
file again. Network failures also follow the Binding retry contract.
Individual reconcile panics are caught per attempt; core bookkeeping preserves
completed results and cleanup responsibility, and the worker continues other Bindings.
Unconfirmed outcomes stop that ID's automatic execution until a new notification.
Reconciliation startup, timer or worker scheduling failure leaves the daemon serving queries and
other services, while Binding mutations are rejected using the existing unavailable
admission error. Policy/Scope CRUD is independent of reconciliation readiness.
Individual Binding errors are rescheduled without closing PAP admission; exhausted
CAS contention is distinct from storage unavailability. There is no automatic restart of a failed reconciliation runtime.
CRUD responses confirm intent admission, and GET/LIST expose subsequent completion.
No process restart recovery is available with memory storage.

The Rust `agent-sec-cli` exposes all 15 Policy, Scope and Binding CRUD commands through
an explicit daemon socket. Its Cargo package and source directory remain `asc-cli`;
the executable target is `agent-sec-cli`. See the [CLI reference](../../../docs/user-guide/en/agent-security/agent-sec-core/policy-cli.md)
and [CLI acceptance record](../docs/design/POLICY_CLI_ACCEPTANCE_zh.md).

The current crates are:

- `asc-foundation-types`: bounded transport-independent identifiers and revisions.
- `asc-policy-types`: authored Policy and immutable prepared Policy/Scope/Binding
  snapshots, backend-independent IR, and target Adapter contracts.
- `asc-policy-engine`: deterministic `prevent_file_deletion` authoring-template
  compiler with a frozen Canonical Policy IR golden. Other template kinds remain
  explicitly unsupported until their lowering and Adapter evidence are defined.
  The implemented template covers path-entry deletion only; rename, move, and
  other namespace mutations are outside its contract.
- `asc-policy-adapter-agentsight`: deterministic file-deletion and PID-Scope
  translation into an AgentSight/ActPlane plan, with semantic and encoding checks.
  Compiler acceptance belongs to the deployed target, not an embedded compiler.
- `asc-agentsight-client`: health-gated AgentSight apply/delete transport for
  one configured endpoint, with process identity resolution and complete HTTP
  fixtures. It does not depend on a reconciliation framework.
- `asc-policy-target-contracts`: shared, PEP-neutral `TargetBindingAdapter` and
  `TargetDeploymentClientFactory` and `TargetDeploymentClient` ports; data lives in `asc-policy-types::target`.
- `asc-policy-repository`: shared Binding aggregate data, consistent reads and
  field-scoped conditional writes; independent of reconciliation implementation.
- `asc-pcp`: synchronous single-attempt `BindingReconciler::reconcile` core over
  repository, Adapter and Client ports, with fresh preparation on every attempt.
- `asc-policy-runtime`: bounded Binding queue, workers, retry timers, compensation
  scans and owned shutdown; the daemon supplies target-specific composition.
- `asc-pap`: transport-independent current-record Policy/Scope/Binding CRUD with
  monotonic revisions over explicit compiler and repository ports.
- `asc-pap-repository-memory`: explicitly temporary process-local Repository
  adapter used only to keep daemon/PAP integration runnable before durable
  persistence lands; also implements consistent reads and reconciliation patches over PAP's Binding map.
- `asc-daemon-protocol`: strict request/response contracts and an explicit
  allowlist for 15 Policy, Scope, and Binding administration methods.
- `asc-daemon-handler`: inbound protocol adapter that decodes daemon requests,
  applies server-owned authorization, routes PAP methods, and projects protocol
  responses without depending on a concrete Repository or compiler.
- `asc-daemon-core`: trusted Principal construction boundary and the
  `PolicyAdministration` application port. `PapService<R, C>` implements this
  port directly, so repository/compiler generics do not leak into dispatch.
- `asc-daemon-service`: bounded UDS admission, one-request framing, kernel peer
  credentials, dispatcher/rejection-encoder injection, connection isolation,
  dispatch cancellation, and controlled drain.
- `asc-daemon-client`: synchronous UDS client preserving complete responses, with
  a single connect/write/read deadline and no retries or local fallback. It uses
  standard-library blocking I/O and `socket2` for bounded connect; neither it nor
  the CLI binary requires a Tokio runtime.
- `asc-cli`: command parsing, typed Policy request construction and Policy output;
  server dependencies are test-only. `commands.rs` registers and dispatches the
  top-level commands; `commands/{policy,scope,binding}.rs` own their arguments and
  request mappings, with pagination and encoding helpers in `commands/common.rs`.
  `capabilities.rs` adds the V1-compatible environment-variable capability view,
  which is rendered locally without a daemon endpoint; its migration contract and
  the remaining gaps are recorded in
  [`V2_CAPABILITY_VIEW_MIGRATION_zh.md`](../docs/design/V2_CAPABILITY_VIEW_MIGRATION_zh.md).
- `asc-daemon`: foreground process and composition root that configures and
  injects concrete adapters into the daemon service.

The crate relationships, acceptance types, executable pass/fail matrix,
compatibility report, direct-consumer evidence, and rollback boundary are recorded
in [`PAP_DAEMON_API_ACCEPTANCE_zh.md`](../docs/design/PAP_DAEMON_API_ACCEPTANCE_zh.md).

The [scan capability development guide (Chinese)](../docs/design/V2_SCAN_CAPABILITY_DEVELOPMENT_GUIDE_zh.md)
maps Prompt Scan and Code Scan migration work onto the repository architecture, including module
locations, dependency order, interface boundaries, and acceptance requirements.
The workspace now exposes `action.code_scan` and `action.pii_scan` through the common
Action Runtime. PII migration and its future policy boundary are described in the
[two-stage PII design](../docs/design/PII_V2_MIGRATION.md).

## PII scanning

`asc-capability-pii-scan` provides a transport-free Rust detector, immutable centralized
rule sets, a typed report, executor and safe audit projector. The daemon loads built-ins
and the optional `/etc/agent-sec/pii-checker/rules.yaml` once; `--pii-rules` accepts an
administrator-selected absolute path. Restart applies rule changes. There is no HOME
lookup or caller-selected file access.

`agent-sec-cli --socket /run/agent-sec-core/daemon.sock scan-pii --stdin --redact-output`
reads input locally and invokes `action.pii_scan` using LocalUser authorization.
The response preserves V1 fields with coverage, input digests and rule identity under
`summary`; `deny` classifies findings and is not a PDP decision. Authorized parameter
failures use the same Finalizer as normal scans. V1-compatible opaque trace metadata
is accepted through top-level `--trace-context`; it is not an OpenTelemetry ID.

See the [PII user guide](../../../docs/user-guide/en/agent-security/agent-sec-core/pii-checker.md)
for limits, rule migration, compatibility differences and rollback. Hook/RPM tests
exercise real Rust subprocesses without switching Agent hosts.

## Daemon service boundary

`asc-daemon-service` is a `PARTIAL_MIGRATION` work package. It preserves the V1
one-request-per-connection LF/EOF framing, bounded first-frame read, bounded
connection admission, and socket ownership cleanup. Normal response encoding
belongs to an injected dispatcher; transport rejection encoding belongs to a
separate protocol-only port. Its acceptance type is current-version contract
testing with socket bytes and fake handlers. The V1 Python daemon is discovery
evidence only and is not linked or executed by the Rust runtime.

The service framework does not deserialize a daemon request, generate protocol
request IDs, choose authorization roles, or render protocol errors. The concrete
`asc-daemon-handler::DaemonDispatcher` receives a bounded raw request frame and
owns method routing. Method
allowlist routing is internal to that one dispatcher implementation; it is not a
second service dispatch layer. A separate `RejectionEncoder` receives typed
transport failures and must remain independent of PAP/Repository state.

The PAP request path is:

```text
UDS frame -> DaemonDispatcher -> method metadata authorization
          -> PapHandler -> PolicyAdministration -> PapService<R, C>
```

RPC reuses the domain `PolicyTemplate`, `ScopeSelector`, `PreparedPolicy`,
`PreparedScope`, and `BindingView` types. It does not define result wrappers for
each CRUD operation. `PolicyAdministration` intentionally mirrors the use cases
once: it erases `R`/`C` before dispatch and repeats authorization at the
application boundary; there is no additional `PapServiceAdapter` forwarding
object.

The current bootstrap bounds frame read, application dispatch, rejection
encoding, response write, connection drain, and final Tokio runtime shutdown.
Dispatch timeout releases transport capacity and signals cooperative cancellation;
it cannot forcibly stop an application blocking call that ignores that signal.
The framework also cannot prove that a concrete PAP/Repository avoids global
locks; that remains a required direct-consumer concurrency test at integration.

The current `asc-daemon` executable composes and registers the PAP dispatcher and
protocol rejection encoder from `asc-daemon-handler`. It composes `PapService`
with `PolicyTemplateCompiler`, a
root-managed Principal policy, and an explicitly transitional process-local
Repository. Policy CRUD therefore works during one daemon lifetime, but all
state disappears on restart; this is integration evidence, not durable
persistence or distribution readiness. The process prints that limitation at
startup. The daemon and CLI default socket is `/run/agent-sec-core/daemon.sock`;
a nonempty `AGENT_SEC_DAEMON_SOCKET` overrides it, and explicit `--socket` takes
precedence over both. Both entrypoints require absolute paths. HOME and
XDG_RUNTIME_DIR do not select a daemon namespace. The V2 RPM stages a system
unit running as `root:root`, with a 0755 runtime directory and 0666
socket. Ordinary users can connect; server-side peer-UID authorization still
protects policy administration. Runtime directory validation, a retained flock and conservative stale
socket recovery protect this namespace. Readiness and persistence remain separate
work. See [systemd acceptance](../tests/v2/systemd/README.md) for deployment,
validation and upgrade boundaries.

UID 0 is always a Policy administrator. A deployment operator can add other UIDs
at startup with repeatable `--policy-admin-uid <UID>` options. Omitted means root
only. Configured administrators cannot delegate other UIDs at runtime; that API
still requires root. The allowlist is process-local and must be supplied on each
startup. Configuration-file loading, persistence and management RPCs remain later
work. Authorization does not change OS socket permissions or deployment topology.

Code scanning is available to any caller that can connect to the UDS. Its
`LocalUser` access policy imposes no administrator or UID allowlist check;
kernel peer credentials supply audit attribution. PAP methods still require root
or an explicitly configured administrator. Audit storage remains private
(`0700` directory, `0600` files).

Run the daemon in the foreground (the existing directory must belong to the
process UID, have mode 0700, 0750 or 0755, and have protected, non-symlink ancestors):

```bash
cargo run -p asc-daemon -- serve --socket /absolute/existing-directory/daemon.sock
```

`asc-daemon-handler::DaemonDispatcher` implements `RequestDispatcher` directly
and is injected by the executable composition root together with
`JsonRejectionEncoder`. PAP is one
registered method family inside the dispatcher; the service framework and
rejection path remain independent of PAP, its compiler, and its repository.

## PAP RPC contract

The closed method inventory contains create, update, exact get, bounded list,
and delete for each of Policy, Scope, and Binding. Successful responses are
`{requestId,result}` and failures are `{requestId,error}`. The result is the
domain record itself; list is the sole shared `{items,total}` shape. Exact inputs
and output type names are frozen by
`asc-daemon-protocol/tests/fixtures/pap-methods.json`.

The stateful `asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json` scenario
freezes complete request and response values for all 15 methods, including
Canonical Policy IR, Scope templates, embedded Binding snapshots, revisions,
statuses, and deterministic digests. Server-generated request and resource UUIDs
use named placeholders so the same fixture can assert their format and identity
flow across later requests. A UDS integration E2E always executes the complete
scenario with a server-authorized test principal. The `asc-daemon` bootstrap E2E
also starts the real binary with `--policy-admin-uid` set to the test UID and
executes the complete scenario without root. A separate default-config case
verifies non-root `permission_denied` (or full CRUD when root). CLI process tests
use an in-process daemon service; a combined CLI and daemon binary E2E is deferred.

New Binding intent normally returns `status: {phase: PENDING_APPLY}` for create/update
or `status: {phase: PENDING_DELETE}` for delete; no-ops return the current lifecycle.
After saving intent, queue rejection conditionally records Failed and `status.error`
and returns the complete BindingView in the same result envelope. Mutation callers
must inspect status.phase; the CLI prints Failed results to stdout and exits 1,
while GET/LIST remain successful queries. A concurrent worker claim
returns the current record; unconfirmed termination returns internal. These
successful responses prove PAP acceptance only. They do not
mean target enforcement or deletion completed. LIST is integration-ready but is
not distribution-ready until a server-owned aggregate encoded-byte budget is
passed through Repository, PAP, and transport.

TODO(policy-response-bounds): direct `PreparedPolicy` and `BindingView` mutation
results can exceed the response-frame limit after process-local state has already
changed, while embedded snapshots can also make Binding GET/LIST oversized.
Before a durable Repository or distribution gate, converge the public result and
storage shapes and enforce server-owned encoded-size budgets for mutations,
single-record GET, and LIST; increasing the transport limit alone is not the fix.

## Current-record revision boundary

Policy, Scope, and Binding each retain one current record per stable identity.
Changed writes advance a positive, never-reused revision and atomically replace
the previous current content. An exact GET for an older revision returns
not-found, and LIST returns at most one current record per identity.

Deleting current Policy or Scope content retains its allocation head as a
tombstone, so a later update of the same identity advances rather than reuses a
revision. A `PreparedBinding` embeds complete Policy and Scope snapshots; an
existing Binding therefore remains deterministic after either source record is
updated or deleted. A new Binding can select only a currently retained source
revision. PAP does not expose historical resource-version CRUD; durable
operation/audit history belongs to later work packages.

## Binding spec and lifecycle boundary

`PreparedBinding` is an immutable Policy/Scope snapshot. `(binding_id,
binding_revision)` identifies that spec; `BindingView { spec, status }` projects
its current lifecycle. Only spec changes increment `bindingRevision`.

| Current | Request | Result | Revision |
|---|---|---|---|
| absent | CREATE | fresh server-generated ID, `PENDING_APPLY` | 1 |
| `PENDING_APPLY`, `APPLYING`, `READY` | identical UPDATE | no-op | unchanged |
| `APPLY_FAILED` | identical UPDATE | `PENDING_APPLY`, clear status.error, retain cleanup targets | unchanged |
| `PENDING_APPLY`, `READY`, `APPLY_FAILED` | changed-spec UPDATE | `PENDING_APPLY`, prepare afresh, retain cleanup targets | +1 |
| `APPLYING` | changed-spec UPDATE | `OperationInProgress` | unchanged |
| `PENDING_DELETE`, `DELETING`, `DELETE_FAILED` | any UPDATE | `OperationInProgress`; deletion is irreversible | unchanged |
| Apply-side states, `DELETE_FAILED` | DELETE | `PENDING_DELETE`, clear status.error, retain spec/targets | unchanged |
| `PENDING_DELETE`, `DELETING` | DELETE | no-op | unchanged |
| absent | GET / UPDATE / DELETE | `NotFound` | — |

Workers claim pending work as `APPLYING` or `DELETING`. Apply success becomes
`READY`; retryable failure returns to the corresponding pending state with a
WorkQueue-owned deadline; permanent/exhausted failure becomes `APPLY_FAILED` or `DELETE_FAILED`.
Attempt counts and deadlines are process-local; explicit pending requests without
an error reset that progress on the next attempt. The table describes Repository
admission before queue notification; queue rejection can replace Pending with Failed.
Delete success atomically removes the Binding and its deployment records only after
all targets are confirmed absent. `Deleted` remains an internal completion marker
in the state machine, never a persisted current status. LIST omits removed rows.
Re-deployment uses CREATE with a new ID at revision 1.

PAP writes compare the complete expected Binding under the same transaction as
request admission. `update_binding(None, next)` inserts a fresh ID;
`update_binding(Some(expected), next)` updates only an existing record. It cannot
resurrect a record removed between the service read and repository write.
Reconciler patches compare revision/phase and the status/error or deployment fields
being written; attempt counts and deadlines are not Repository fields. Patches carry no spec and cannot erase
a newer intent or target observation. A Delete accepted while Apply is running
keeps the same revision, and the old Apply still records its target observations
before the next cleanup attempt.

PAP request semantics, the synchronous core and background Runtime are tested with
the memory repository. The daemon wires post-commit notifications and timers
through an attempt-local Client factory. PAP acceptance still does not imply target
completion. Full process E2E, durable storage, cross-process recovery and outbox
remain separate work. The compiler is limited to the golden-backed
`prevent_file_deletion` lowering described above.

Dependency sources, TLS/unsafe boundaries and release audit requirements are
recorded in [DEPENDENCIES.md](DEPENDENCIES.md).

Run the workspace validation from this directory:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Shared scan lifecycle

Scan handlers now receive `asc-daemon-core::ActionService`; production runtime
registration lives in `apps/asc-daemon/src/actions.rs`. The process constructs one
shared finalizer with security-event, telemetry and diagnostic outputs. Each scan
invocation automatically finalizes; handlers and capabilities do not own sinks.

`asc-telemetry` provides the V1 scan field allowlist and policy gates;
`asc-event-sink::telemetry::TelemetryWriter` appends only to an existing uploader-owned
file. Audit JSONL/SQLite and telemetry attempts remain synchronous and independent.
Code Scan and PII Scan share this lifecycle. PII parameters and normalized business
metadata enter the typed `ActionService`; authorized parameter rejection uses
`Invocation.reject` and the same finalizer. Telemetry retains only allowlisted
scalars, including PII verdict and elapsed time, independently of audit output.
Other scan capabilities add their own identities and projections when implemented.

See the [implementation, compatibility and acceptance record](../docs/design/RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)
for the exact scope, executable checks, deferred work, and rollback procedure.
