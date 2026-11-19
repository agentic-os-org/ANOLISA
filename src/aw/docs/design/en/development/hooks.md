# AW Hook and Source development rules

[中文版](../../zh/development/hooks.md)

These are required development rules for the tasks indexed in [AGENTS.md](../../../../AGENTS.md). Read the relevant sections before editing.

## Semantics, effects and evidence

Use the existing 16-name vocabulary from the configuration schema:
`session.start`, `input.submit`, `tool.before`, `tool.after`,
`permission.request`, `compact.before`, `compact.after`, `subagent.start`,
`subagent.stop`, `turn.stop`, `session.end`, `model.before_request`,
`runtime.observed`, `runtime.exited`, `security.violation`, `coverage.changed`.
Changes to names, effects or payloads require compatible versioned contracts.
Recognition of a name establishes no executable capability.

Declare support for an event together with its phase, effect, target scope,
entrypoint, runtime profile and adoption guarantee. Keep observed input,
effective execution input, callback result and delivered result distinguishable.
A before check covers the input actually checked; stronger guarantees require
verification of later transformations and the final side-effect boundary.
A result check covers only verified fields and delivery routes, including any
streaming path claimed by the profile. Preserve cancellation and deadlines.
Keep failure actions such as `withhold_result` separate from Provider effects.

Preserve existing `security.violation` final-result check semantics: a supported
gate submits candidate output for a policy decision, including allowed output.
A confirmed finding and a successfully adopted block are separate facts.
Do not redefine the callback as a notification emitted only after a violation.
Likewise, audit lifecycle facts do not establish public Provider Hooks. Native
Session, model Turn, request completion, task drain and process exit require
separate identity and completion evidence. Keep framework-specific event
comparisons in the paired Adapter design references and native tests.

## Lifecycle bindings

Shared lifecycle requirements cover instance/task identity, admission,
task-stable policy revisions, Provider execution, audit and bounded cleanup.
Preserve the existing bindings described in the
[Service registration contract](../runtime/local-service.md) and
[native startup flow](../architecture.md#application-owned-native-startup):

- Ordinary `Register` factories, including SDK factories and QwenPaw, confirm
  root Ready and execute each task through an acknowledged child Scope. Retain
  its route snapshot and policy revision through native cleanup.
- Admitted `RegisterNative` bridges for Qoder, OpenClaw and Hermes execute on
  the native root after Ready. Complete native request/tool occupancy fences
  policy updates through cleanup. Preserve owner authentication and callback
  peer checks; native roots cannot open SDK child Scopes.

Future lifecycle unification requires an explicit mapping of native
request/task identities, admission, update occupancy and cleanup to Scopes,
with migration and regression coverage before changing either binding.

## Source selection and lifecycle

The runtime Source SPI cutover and per-task runtime adoption of a Resolved Hook
Plan remain development targets, while the versioned wire contracts
`aw_contracts::source::SourceCapability` (`source-capability-v1alpha1`) and
`aw_contracts::hook_plan::ResolvedHookPlan` (`resolved-hook-plan-v1alpha1`) and
the side-effect-free solver `aw_core::resolver` are defined and contract-tested
(see [Core admission](../core-admission.md#pure-source-resolution-and-shadow-plan-equivalence)).
Current Adapters expose prepared capabilities and native wiring; preserve those
executable contracts while introducing the shared abstraction. Do not describe
an unwired runtime SPI, wire field or CLI as active in runtime dispatch.

For new Source resolution, derive requirements per semantic Hook, effect and
guarantee. Prefer native Hooks when they meet those requirements; fill gaps
with verified instrumentation appropriate to the actual runtime. A single
instance may combine Sources. Python implementations may reuse monitoring or
wrapper mechanisms; dowhen is optional. Reuse shared Node/Bun module-patch and
request-gate mechanisms. Keep ptrace/BPF observations within their verified
scope. Runtime interception alone provides no Agent-level semantic association.

Source contracts describe probe, install, verify, activate and teardown, plus
runtime/code identity, phase, field mapping, effect adoption, scope, cost,
failures and cancellation. Public native APIs need declared compatibility and
acceptance evidence; private code patches additionally need target fingerprint
and seam verification. Do not impose private-patch version pins on generic SDK
transport. Keep optional instrumentation dependencies out of core SDK imports.

Assign one dispatch owner for each logical event/phase and admitted policy step,
including observation-only steps. An enforcement boundary has at most one
control owner; auxiliary Sources may add evidence without dispatching the same
step again. Correlate verified generation, Instance, task/request and invocation
identities, including Scope identity where the lifecycle binding uses one;
retain source-local identities and explicit association gaps.
Do not deduplicate by approximate timestamps or tool names. Reuse the Service
ledger and preserve unknown outcomes; never retry uncertain Provider effects
or replay tools to manufacture an exactly-once completion guarantee.

Resolve and verify Sources before admitting protected work. In the planned
model, freeze Source assignments and policy revision for each task. Source
reconfiguration requires re-probe, installation and verification at a safe
boundary; ordinary policy updates do not authorize live patching. Distinguish
App Ready, installed wiring, verified capability, AW Ready and actual adoption.
An attach must not mark earlier in-flight work as protected. At runtime
capability admission (`admission::preflight` and `resolve_shadow_preflight`),
reject unsupported applicable enabled events and unsupported effects or failure
actions of applicable enabled steps, regardless of `required`; an enabled event
with no steps still requires support, and only additional auxiliary observation
Sources carrying no configured requirement may degrade with an explicit report.
In offline multi-source planning (`aw_core::resolver::resolve`), unsatisfied
`required: true` hooks fail with `ResolveError::UnsatisfiedRequired` while
unsatisfied `required: false` hooks are recorded as explicit `CoverageGap`
entries (`Coverage::is_complete() == false`) so runtime admission can reject
any incomplete plan before activation. Explicit native fallback governs Service
availability and cannot relax static capability admission; it must remain
policy-compatible and preserve denial, Provider errors and cancellation.
Source loss follows the admitted failure contract without changing owners
mid-task. Teardown may require restart when the mechanism cannot be removed.

Pure requirement resolution belongs with shared admission; process probing and
installation belong in the Adapter/Source layer. Service owns Instance, Ready,
Scope activation, budgets and audit. Host/executor retain Provider execution.
A Resolved Hook Plan describes integration bindings; keep it separate from the
existing independent `Core::prepare/execute` plan API. Framework-specific code
must not enter Service, Core or Host to add a new Agent.

## Input replacement and native authorization

Security denial and native permission denial retain precedence. An admitted
`replace_input` step is the first applicable `tool.before` step, has that sole
effect, and requires fail-closed before steps. Host binds the candidate input
without renewing identity, claims or budget. Service checks all later Providers
and native Hooks against that candidate and publishes it only with the final
successful callback. Adapters adopt it within the same invocation and retain
native authorization. Legacy Tokenless `block` suggestions remain explicit
retry mode. Input replacement never emits native approval.

## Adding an Agent or changing support

New integrations extend existing contracts within the three first-class entry
modes. Do not add a parallel launcher for the same integration mode; reuse its
existing launch, inspection and cleanup flow.

Unified Hooks, managed MCP and policy updates are shared AW capabilities.
Keep configuration, admission, revision handling, lifecycle ownership and audit
in the common framework; Adapters supply native boundaries and effect adoption.
Frameworks may deliver these capabilities in different orders. A missing native
boundary must produce an explicit unsupported capability at admission.

Maintain the paired [capability support reference](../../../../../../docs/user-guide/en/aw/capabilities.md)
in the same PR as any support change. Record the Adapter/Source, supported
runtime profile and entry mode, execution boundary, limitations and runnable
acceptance evidence. Keep design decisions, implementation, contract tests,
native adoption verification and released artifacts distinct. The static grid
covers declared default profiles; conditionally prepared capabilities also need
explicit profile-specific tests and documentation. A successful probe alone
does not broaden default support or prove native adoption.
The README and support reference expose the generated event/effect grid.
The reference adds MCP and update summaries; Agent pages own integration details.

Policy updates must reuse candidate preparation, revision checks, durable audit
and atomic activation. Native request/scope admission and activation must share
a gate; preparation, unacknowledged handoffs and unfinished cleanup count as
occupancy. Retain one policy revision through a task's before/after and MCP
checks. Confirm native route adoption before dispatch, preserve old policy on
rejected updates, and keep uncertain cleanup fail-closed. Describe each Adapter's
verified readiness, launch mode and switching boundary; never infer reload
support from a PID or native settings watcher. Keep external PID attachment,
internal process identity and update target selection distinct. Prewarmed workers
need readiness before their first task. The control plane may pause dispatch for
updates; retain the runtime busy gate and avoid adding active-task switching
without an explicit contract. Post-activation audit failure must stop dispatch
rather than report an unchanged old policy. Document current support in the
capability reference; keep proposed development order in design documents.

Update these together in the same change:

1. Register native dialects and default event/effect profiles in
   `crates/aw-adapter/src/adapter/catalog.rs`; use them when building prepared
   capabilities. Test conditional profile additions and restrictions explicitly.
   Shared Source additions reuse existing dialects where applicable; a new public
   semantic ingress requires an admitted, versioned contract. Keep the
   configuration schema aligned.
2. Implement native normalization, registration, effect adoption and teardown in
   the Adapter/Source layer. Keep launched-child ownership in `aw-exec`,
   application-owned registration and Scopes in Service, and entry-mode
   orchestration in its existing caller. Reuse admission and callback helpers.
3. Add exact runnable case references to `tests/regression-matrix.json` for every
   event/effect and for lifecycle, security and Tokenless composition. Extend
   `crates/aw-cli/tests/regression/native.rs` for new native payloads. The
   shared PreTool and PostTool tests iterate the matrix without a fallback
   dialect, so an unimplemented fixture fails.
4. From `src/aw`, run `python3 scripts/check_matrix.py --write-docs` and
   document native adoption limits in both capability references. Regenerate
   both README grids in the same operation.
5. Add pinned native commands, required reports, an owner and the exact acceptance
   boundary to `tests/native-matrix.json`. Unit suites declare counts; custom
   suites declare JSON result files with exact case names or observed counts.
   Both execution and artifact aggregation verify those reports. Every registered
   Agent must be accounted for; new integrations cannot silently opt out of nightly.
6. From `src/aw`, run `python3 scripts/check.py`. The gate compares the compiled
   catalog with the matrix and schema, resolves case names against real libtest
   executables, rejects missing or ignored cases and checks README and user-guide grids.

Use deterministic service/CLI/Provider fixtures on every relevant push. Keep
real SDK/CLI adoption checks explicit and pinned; describe which native seam
was exercised. A callback transport test does not certify native adoption.
Do not substitute ignored, opt-in or credential-dependent tests for a required
matrix case. Package acceptance separately covers the real Tokenless binary.
