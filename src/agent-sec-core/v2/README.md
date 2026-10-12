# AgentSecCore V2 Policy, SkillSec and daemon

SkillSec supplies scanning, system-key integrity, versions and activation through the Rust
`skill-ledger` CLI and a root daemon. See the [core guide](../../../docs/user-guide/en/agent-security/agent-sec-core/skillsec-v2.md)
and [migration/acceptance boundaries](../docs/design/SKILL_SEC_PHASE_ONE.md).
Agent Hook migration is separate; the core never calls Python Ledger.

This workspace slice contains the dependency-light contracts, Policy
Administration Point, first-version PAP daemon protocol, Policy-template
validation, protocol-independent Unix-domain-socket service framework, and runnable
foreground process bootstrap, together with the first AgentSight file-deletion
target Adapter, and its independent deployment Client used by later AgentSecCore
V2 work packages. `asc-pcp` provides synchronous attempts and
`asc-policy-runtime/src/reconciliation/` provides the bounded Binding queue,
workers, retry timers and compensation scanning. The daemon stores Policy, Scope
and Binding state in SQLite, including deployment cleanup responsibility.

Each attempt re-reads current state and prepares afresh. Reconciliation writes
update specific fields without carrying spec. Plans and prepared requests are
call-local; a storage failure retains the original write receipt and remote outcome
until its commit is confirmed. See the [runtime design (Chinese)](../docs/design/BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)
for retry, ownership and staged acceptance.

Start the daemon with background Binding delivery:

```sh
sudo agent-sec-daemon serve --socket /run/agent-sec-core/daemon.sock
```

Create the root-owned runtime directory first (0755 for local clients). The daemon entrypoint
starts the reconciliation service. Its `reconciliation.rs`
composition module registers `AgentSightClientFactory::default()` without reading
credentials or contacting the PEP. Each due attempt opens its own Client and reuses
it through preparation and create/update, or through deletion for a saved route.
Environment-based PEP selection is deferred. The Client owns endpoint/token-file
defaults; daemon and CLI expose no AgentSight configuration options. Missing or
invalid credentials are retryable Binding failures; each retry reads the token
file again. Network failures also follow the Binding retry contract.
Individual reconcile panics are caught per attempt; core bookkeeping preserves
completed results and cleanup responsibility, and the worker continues other Bindings.
Storage failures pause target I/O and retry the retained write without consuming the
remote retry budget. Reconciliation startup failure aborts daemon startup. A later
timer or worker scheduling failure leaves the daemon serving queries and
other services, while new Scope assignments are rejected using the unavailable admission error.
Policy writes, queries and Scope deletion remain available.
Individual Binding errors are rescheduled without closing PAP admission; exhausted
CAS contention is distinct from storage unavailability. There is no automatic restart of a failed reconciliation runtime.
Scope responses confirm intent admission, and GET/LIST expose subsequent completion.
Startup restores discovery and pending or interrupted reconciliation from SQLite.
Ready Bindings and terminal failures are not automatically reapplied.

The Rust `agent-sec-cli` exposes 12 Policy, Scope and Binding administration commands through
an explicit daemon socket. Its Cargo package and source directory remain `asc-cli`;
the executable target is `agent-sec-cli`. See the [CLI reference](../../../docs/user-guide/en/agent-security/agent-sec-core/policy-cli.md)
and [CLI acceptance record](../docs/design/POLICY_CLI_ACCEPTANCE_zh.md).

The current crates are:

- `asc-foundation-types`: bounded transport-independent identifiers and revisions.
- `asc-policy-types`: authored Policy and immutable prepared Policy/Scope/Binding
  snapshots, template validation, and target Adapter contracts. Snapshots retain
  `policyId`, `policyName`, `revision` and `template`.
- `asc-policy-adapter-agentsight`: deterministic file-deletion and pinned-process
  translation directly from the Binding's template into an AgentSight/ActPlane
  plan, with semantic and encoding checks. Reusable policies contain general
  rules; this Adapter supports only block + file/write + operation=delete without
  history. Any unsupported rule rejects the complete Binding with its rule index.
  Typed resources currently cover files; undefined resource formats are rejected.
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
- `asc-pap`: transport-independent revisioned Policies, immutable Scope assignments and
  system-owned Binding admission over the repository port. PAP validates authored
  templates before saving them.
- `asc-policy-repository-sqlite`: authoritative Policy, Scope and Binding storage,
  atomic lifecycle writes, status CAS, durable write receipts and recovery scans.
- `asc-pap-repository-memory`: process-local test backend implementing the same
  PAP and reconciliation contracts.
- `asc-daemon-protocol`: strict request/response contracts and an explicit
  allowlist for 12 Policy, Scope, and Binding administration methods.
- `asc-daemon-handler`: inbound protocol adapter that decodes daemon requests,
  applies server-owned authorization, routes PAP methods, and projects protocol
  responses through the application port.
- `asc-daemon-core`: trusted Principal construction boundary and the
  `PolicyAdministration` application port. `PapService<R>` implements this
  port directly, so repository generics do not leak into dispatch.
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
- `asc-model-client`: shared, loopback-only HTTP client for local model
  inference backends (Ollama); injected by scanner crates.
- `asc-capability-prompt-scan`: prompt injection/jailbreak scanner combining
  a rule engine, model-backed classification and multi-turn intent detection;
  served by the daemon through `action.prompt_scan`. It supports `fast`,
  `standard`, and `strict` single-turn text scans, plus `multi_turn` with the
  conversation triple in `history`/`text`/`assistantResponse`; an optional
  `model` field overrides the L2 backend.

The crate relationships, acceptance types, executable pass/fail matrix,
compatibility report, direct-consumer evidence, and rollback boundary are recorded
in [`PAP_DAEMON_API_ACCEPTANCE_zh.md`](../docs/design/PAP_DAEMON_API_ACCEPTANCE_zh.md).

The [scan capability development guide (Chinese)](../docs/design/V2_SCAN_CAPABILITY_DEVELOPMENT_GUIDE_zh.md)
maps Prompt Scan and Code Scan migration work onto the repository architecture, including module
locations, dependency order, interface boundaries, and acceptance requirements.
The workspace now exposes `action.code_scan`, `action.pii_scan` and `action.prompt_scan`
through the common Action Runtime. PII migration and its future policy boundary are described in the
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
is accepted through the shared CLI `--trace-context` adapter and top-level OTel
carrier/compatibility envelope; PII params reject `traceContext`. Finalizer reads
correlation from Context, while kernel peer identity is passed as `CallerIdentity`.
Opaque labels are not OpenTelemetry IDs. Business requests retain 4 MiB capacity
with a separate 32 KiB propagation allowance; responses remain limited to 4 MiB.

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
with a root-managed Principal policy and the SQLite Repository. Policy state lives
in `policy-state.db` under `AGENT_SEC_DATA_DIR`
(default `/var/log/agent-sec`). The data directory must be private (0700), with
database and lock files restricted to 0600. Unsafe existing permissions, incompatible
schema and corruption fail startup; the daemon never deletes or rebuilds this state.
An independent database lease prevents two socket namespaces from sharing it.
See [persistence, upgrade and recovery](../docs/design/POLICY_SQLITE_PERSISTENCE_DESIGN_zh.md).
The daemon and CLI default socket is `/run/agent-sec-core/daemon.sock`;
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
rejection path remain independent of PAP and its repository.

## PAP RPC contract

The closed inventory has 12 methods: Policy create/update/get/list/delete; Scope
create/get/list/delete/retry; Binding get/list. Scope update and Binding mutation
methods return `unknown_method`. Scope requests no longer accept a revision.
Success uses `{requestId,result}` and failure uses `{requestId,error}`. Results
are `PreparedPolicy`, `PreparedScope`, `BindingView`, `{items,total}` lists, or
`ScopeDeletion {scopeId,completed}` for deletion.

Exact inputs/results are frozen in `asc-daemon-protocol/tests/fixtures/pap-methods.json`.
The stateful `pap-crud-e2e.json` fixture covers assignment admission, template
changes, saved snapshots, queries and deletion. Rust CLI/UDS/bootstrap tests and
`tests/v2/e2e/test_policy_cli_e2e.py` exercise the public surface. Real procfs to
PAP/runtime/Adapter component tests use a scripted Client.
`tests/v2/e2e/test_policy_delivery_e2e.py` additionally runs real CLI and daemon
processes through SQLite, discovery, the Adapter and the production Client to an
HTTP mock at `127.0.0.1:7396`. It reuses the Binding/template and AgentSight wire
fixtures, checks delivery of the Scope's saved revision after a template update,
then verifies remote deletion and local Scope/Binding cleanup. Run it in the root
E2E environment with that port available; an existing token file is preserved,
and only a token created by the test is removed. The existing
`make test-e2e-rpm-v2` target collects it. These checks do not prove live kernel
enforcement.

Policy and complete Scope admission reject encoded records larger than 1 MiB
before mutation. List pages stop at a 3 MiB item budget, leaving room in the default
4 MiB response frame. Advance offset by `items.len()`, not by the requested limit.
`total` is the count before pagination. Large templates can therefore pass CLI file
validation but fail daemon admission.

## Policy revisions and immutable Scope assignments

The [lifecycle contract](../docs/design/POLICY_SCOPE_BINDING_CONTRACT_zh.md) is implemented
with SQLite storage. Policies retain a stable ID and only their current
complete record. Changed content advances a never-reused revision; identical content
is idempotent. Deletion retains the allocation head. Old revisions are not queryable.

Scope creation receives `policyTemplates: [{policyId,policyRevision}]` and stores
full server-resolved `policySnapshots`. The repository verifies all snapshots under
the same transaction as Policy mutation. Concurrent update/deletion either follows successful
admission or causes it to fail; it never substitutes a newer revision. Existing
Scopes and future matching instances keep their saved snapshots after Policy changes.
A Scope has no revision, and its selector/policies cannot be updated.

Name/path selectors continuously match processes; PID selection pins its first
observed instance. Cgroup assignment is unsupported. Discovery submits complete
known instance sets to PAP, which deduplicates and writes real Binding intents.
Unreadable processes remain selected; only confirmed exit or mismatch retires them.
Each Binding contains one Policy snapshot plus Scope provenance and a process
identity, not a copy of all the Scope's policies. Boot ID, PID namespace, PID and
start time remain fixed through retries; the Client rechecks them before target I/O.

## Scope deletion and Binding lifecycle

Deleting a Scope atomically marks it `DELETING` and closes child admission, then
cancels/joins discovery and requests deletion of every owned Binding. Existing
Reconciler workers serialize target calls, preserve observations from an in-flight
Apply, and clean all known or uncertain deployments. An old Apply cannot restore
`READY` over deletion intent. Once discovery has stopped, removing the last owned
Binding after confirmed cleanup also removes the Scope and its snapshots. A deleting
Scope with no Bindings is removed when discovery stops. Active Scopes remain when
process exit leaves them without Bindings.

`scope delete` returns `completed:false` while cleanup remains; `completed:true`
means the Scope was removed. Repeating it does not reset retry budgets. Completed
deletion succeeds even after restart; deleting an absent Scope also reports completion. Other Scopes
and the source Policy are unaffected. `scope retry` resets only failed
owned Bindings (`APPLY_FAILED`/`DELETE_FAILED`) to pending, preserving deployment
responsibility. Queries remain successful even when status reports failure.

Bindings start at internal `bindingRevision:1`. Scope/instance lifecycle drives
`PENDING_APPLY` -> `APPLYING` -> `READY` and `PENDING_DELETE` -> `DELETING` -> removal.
Permanent or exhausted failures remain queryable. Reconciler CAS, serial execution,
retry budgets and lower-level changed-spec revision regression coverage are retained;
there is no public changed-spec Binding mutation. Queue notifications carry only IDs.

Retry counts and timers remain process-local and reset on restart. SQLite preserves
status versions, deployment responsibility, discovery pins and stop barriers. WAL
with synchronous FULL protects committed local state; remote operations still depend
on the Client's idempotency and absence contract. Local SIGKILL tests use an external
mock ledger; they do not establish physical power-loss or real PEP/kernel guarantees.
The Adapter translates the saved general rules on every Apply attempt, preserving
rule order and per-rule reasons. File-deletion rules lower to unlink DSL; real
delete-only kernel behavior remains subject to the backend operation mapping.

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
Code Scan, PII Scan and prompt scan share this lifecycle. PII parameters and normalized business
metadata enter the typed `ActionService`; authorized parameter rejection uses
`Invocation.reject` and the same finalizer. Telemetry retains only allowlisted
scalars, including PII verdict and elapsed time, independently of audit output.
Other scan capabilities add their own identities and projections when implemented.

See the [implementation, compatibility and acceptance record](../docs/design/RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)
for the exact scope, executable checks, deferred work, and rollback procedure.

## Native OpenTelemetry tracing

This is the first OTel integration. V1/V2 refer to the Python/Rust product
implementations, not OTel generations. Existing `--trace-context` JSON and record
metadata remain supported inputs. `bind_trace_context_input` maps the former into
the unified OTel Context. Caller-supplied opaque trace/invocation labels retain
their correlation meaning separately from SDK TraceId/SpanId.

`asc-observability` supplies the current OTel Context, five Agent baggage fields,
read-only correlation snapshots and a process-owned `runtime` feature. CLI/client,
daemon and PAP spans share SDK identity. A raw UDS caller that omits
context gets a fresh daemon root. Missing Agent metadata is allowed for ordinary
PAP calls; future observability consumers call `validate_metadata` when required.
Adapters for the existing V1 record input use `bind_metadata(parent, value, kind)` with `AgentRun`,
`ModelCall` or `ToolCall`. It validates the record's own V1 metadata before replacing
session/run/call/tool fields: missing required fields fail even if the parent has
them; omitted/null optional fields clear inherited values. Trace parentage,
request correlation, compatibility labels and independent agent attribution remain.
Metadata extras are ignored per hook schema; `agent_name` comes from trace-context
or the native carrier. Ordinary child-context propagation continues to inherit.
The production runtime uses a real SDK with fixed `AlwaysOff` sampling for local
correlation. IDs, parentage and baggage remain available; no exporter is installed.

Existing caller input remains flat JSON; put this bootstrap option before command
names and before other options' non-option values, matching the V1 parser:

```bash
agent-sec-cli --trace-context '{"agent_name":"openclaw","session_id":"session-123","tool_call_id":"tool-1"}' \
  --socket /run/agent-sec-core/daemon.sock policy list
```

A native upstream parent may be supplied alongside it:

```bash
agent-sec-cli --trace-context '{"session_id":"session-123"}' \
  --otel-context '{"version":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}' \
  --socket /run/agent-sec-core/daemon.sock policy list
```

The current release exposes no OTLP exporter or exporter configuration. `OTEL_*`
export, sampling and batch settings cannot enable export or change the fixed
local sampling policy. No Collector, HTTP client or exporter worker is created.

| Variable | Behavior |
| --- | --- |
| `RUST_LOG` | Default warn; `info` enables bounded JSON correlation diagnostics on stderr; `off` suppresses these records without disabling context |
| `AGENT_SEC_INVOCATION_ID` | Optional caller-supplied invocation label; never automatically generated |

Service resources use `asc-daemon` / `agent-sec-cli` and the build package version.
Each runtime owns one diagnostic worker with a 64-record queue and a 32 KiB
per-record limit (2 MiB queued payload). It also handles daemon startup warnings
and operational errors, independently of `RUST_LOG`. Reconciliation, JSONL and
SQLite library warnings use the `asc_process_diagnostic` tracing target, routed
to the same writer without a synchronous fallback. Library hosts must install a
subscriber; the libraries do not create threads or initialize the SDK. Producers never wait for
stderr I/O; overflow, oversized records, worker creation failure and sink failures
lose diagnostics. There is no per-second rate limit; the stderr consumer owns
retention and rotation. CLI draining waits at most 50 ms; daemon draining shares
the additional 2 s provider shutdown budget after service/runtime shutdown.
`init_runtime` is called once from main. Subscriber conflicts or invalid SDK
identity fail before business work with exit 1; `otel: <reason>` is best effort,
using a bounded worker even before successful runtime initialization.
The process panic hook also queues only `runtime: panic`, without payloads;
unwind/abort behavior is unchanged.
CLI help, usage, errors and business results retain synchronous output semantics.
These required outputs can wait for their consumer; the diagnostic queue is not a
lossy replacement for business output.

Native requests support optional `traceContext` (version 1, optional string
`traceparent`, `tracestate`, `baggage`) and `compatibility` (version 1, optional
`traceId`, `invocationLabel`). The new CLI requires a daemon supporting this
carrier; pre-carrier daemons are outside the supported version matrix. Deploy
server first; a client never retries without context after rejection. Regular
RPCs inject a carrier even without tracing flags. The preserved `--trace-context`
input and explicit `AGENT_SEC_INVOCATION_ID` can affect wire attribution/labels. Requests have separate 4 MiB business and 32 KiB
propagation budgets; response capacity remains 4 MiB, including LF.

Business functions use `tracing::info_span!` or `#[tracing::instrument(skip_all)]`;
names are defined at each callsite. For a task/thread boundary, capture
`asc_observability::Context::current()`, bind a fresh child using `parent_span`,
and instrument the future. Do not hold an entered span/Context guard across await.
Consumers call `snapshot()` inside the scope, then persist that read-only snapshot
independently of span sampling/export. This package does not implement event
storage or local trajectory reconstruction.

Validation and rollback: [OTel acceptance](../docs/design/V2_OTEL_ACCEPTANCE_zh.md).

The AgentSec UDS adapter accepts up to 16 KiB of encoded baggage, preserving all
five 256-code-point values. Non-ASCII bytes must be percent-encoded; using fewer
ASCII escapes does not reduce their size. Receivers supporting only the W3C
8192-byte interoperability minimum may drop larger headers; the locked SDK's
standard BaggagePropagator drops them whole. Cross-service forwarding must define
its own budget/compatibility contract before use. The local adapter does not
silently shrink original metadata to meet another receiver's limit.

After building the V2 workspace, run the cases from the component directory:

```bash
PATH="$PWD/v2/target/debug:$PATH" uv run --project agent-sec-cli pytest tests/v2/e2e/test_otel_e2e.py -v
```

These tests require Linux, UDS, loopback TCP and subprocess support. Missing
binaries on PATH or unavailable sockets fail; only the root-inapplicable non-root
authorization case explicitly skips. The existing `make test-e2e-rpm-v2` target
also collects these cases against installed binaries. Source-built process tests
do not establish RPM/systemd acceptance. Storage-fault cases verify that blocked
stderr cannot prevent scanning, independent audit writes or shutdown.
