# Policy reload and native attachment

[中文版](../../zh/runtime/policy-reload.md)

AW can explicitly update the policy of an idle instance launched with a stable
bridge. The native host process stays alive; busy instances reject the update.
The runtime path is opt-in through `aw run --reloadable`, initially pinned to
Qoder 1.1.73 TUI, QwenPaw 2.2.2b4 / AgentScope 2.0.9 App and Qoder SDK
1.0.54 / bundled Worker 1.1.74. External attachment supports the TUI and App
through `aw agent connect --pid PID`; SDK daemon attachment and steer remain
planned. Ordinary `aw run` keeps its existing registration path.

## Qoder SDK request admission

The Query factory registers stable tool routes and wraps input/output only when
`--reloadable` is selected. Every message acquires an existing AW request claim
before SDK submission. All managed Queries in one binding share the active
policy; native daemon ownership stays with the application. The associated
process is the application worker, which issues the SDK callbacks, rather than
an arbitrary daemon PID supplied by a caller.

Worker 1.1.74 can emit command completion and result frames before its generator
cleanup finishes. Its public `getContextUsage()` control is queued behind the
active native turn; the bridge awaits that read-only barrier before releasing
the completed command's claim. This depends on the tested native command-loop
contract, not just a version comparison or receipt of `Stop`. If it fails, the
bridge closes the Query and awaits its native close handshake and in-flight
callbacks. Failed cleanup retains busy admission until the owning worker exits.
A background reader observes terminal frames even when the caller stops reading
after a result. Input exhaustion is held until outstanding claims are released,
so SDK automatic end-input shutdown cannot race the control barrier.

Updates reuse `HookPlan`, `UpdatePolicy`, revision checks, policy validation,
Provider preparation and audit. No SDK code enters the service. The control
queue barrier, cancellation and multi-Query claim isolation must be revalidated
when extending the accepted SDK/Worker combination. External attachment and
per-session policies remain separate capabilities.

## Separate lifecycles

| Capability | Prerequisite | Responsibility |
| --- | --- | --- |
| Policy reload | Working AW bridge on the instance | Prepare and activate an instance policy revision |
| Native attach/detach | Externally started native agent | Register or remove the bridge and associate its process |

Policy reload should reuse bridge registration. Attachment must distinguish
registration requested, bridge confirmed and policy active before admitting
requests that require that policy. Finding a PID alone provides neither native
tool admission nor result replacement.

Keep `aw run --config FILE --agent TARGET` as the integrated launch entrypoint.
Frameworks retain their models, credentials, UI, scheduling, permissions and
sessions. The Adapter retains native registration and effect adoption; the
service retains revision selection, deadlines, correlation and metadata audit.
Do not expose internal RPC assembly as the required user workflow.

## Implemented: offline update boundary

`Configuration::validate_policy_update` compares two fully validated documents.
Only these sections may differ:

- `spec.providers`, including Provider-owned configuration and transport.
- `spec.events`, including steps, guards, error actions and event budgets.
- `spec.execution.default_event_budget_ms`.

All other fields remain identical, including metadata, service paths, Agent
entrypoints, native scheduling settings and optional observation configuration.
Adding or removing an optional frozen field also fails. Public sections are
checked with an allowlist so a future schema field is not implicitly mutable.
Formatting and mapping order are ignored; omitted defaults remain omitted.

Both inputs pass the existing bounded parser, schema and static reference checks.
Provider-private configuration stays opaque. No commands run, files change,
services start, native versions are probed or capabilities are granted.
Errors report a public section without printing configuration values.

```bash
aw validate --config next.yaml --against current.yaml
```

Success prints `policy update scope valid; runtime activation is not checked`.
This means only that the change is confined to policy fields. Provider discovery,
private configuration checks, native support, readiness, idle confirmation and
activation still require runtime admission. A missing or invalid baseline fails.
Plain `aw validate --config FILE` retains its existing output and semantics.
The comparison applies to any configured Adapter and is not proof that any
framework supports runtime reload.

## Implemented: prewarming and idle-only activation

Prewarm the container, AW, the stable before/after bridge and the reusable native
host. A concrete Agent or Session can still be created by the framework when the
user request arrives. Apply the requested AW policy before creating/dispatching
that request. Native model, credential and Session configuration stays with the
framework; this operation changes only the AW policy fields listed above.

`aw run --config current.yaml --agent TARGET --reloadable` preinstalls stable
callbacks even when the default policy has no user hooks. `aw status` reports
instance IDs, active policy revisions, readiness, request/tool counts and process
metadata. Use `aw update --config current.yaml --agent TARGET`
to apply the same edited document, select the unique target instance and read its
revision. The separate `--policy next.yaml` form remains compatible. Multiple matches require `--instance`; `--socket` and
`--expected-revision` allow explicit selection and concurrency preconditions.
The observed revision still guards activation; conflicts never retry silently.
No internal callback assembly is required. Editing a file alone never activates it.

The CLI records the canonical configuration path, socket and original service
identity at reloadable launch or native connection. The private metadata lives
under `$XDG_RUNTIME_DIR/aw/connections` (or `/tmp/aw-UID/connections`), keyed by the
path digest; it contains no policy or credential bytes. Atomic replacement of the
user's file preserves the connection. Status, detach and update use the recorded
identity, never a candidate's endpoint or digest. Stale generations and unsafe
receipts fail visibly. `aw service stop --config` removes the matching connection
receipt; its private lock inode remains until runtime-directory cleanup. A new
explicit attach can adopt a restarted service and applies the current desired
policy after native readiness and before reporting success. Every `aw run`
checks an existing connection before selecting a service. A changed-file relaunch
requires stopping the recorded service first; use `aw update` for active instances.
After that service stops, an explicit launch refreshes the existing association.
An ordinary launch without an existing association keeps its original launch path.

Users maintain one desired document. Editing is offline; failed/busy updates
retain the old in-memory policy without rewriting the user's desired file. The
existing service snapshot and policy revision checks remain unchanged. No new
service RPC, per-session policy semantics or automatic update retry is introduced.

The service generation and bootstrap configuration digest remain fixed. Updates
address an existing instance without resolving a service from the candidate's
digest. Each instance retains its bytes, Host and immutable process context;
updating one instance does not update another or change future bindings' default.

1. Reject unsupported/unready instances, active requests/tools/events and stale
   revisions before preparation. A concurrent update returns `busy`.
2. Validate policy scope and native scheduling/budget limits, then prepare a new
   Host through existing Provider discovery and private configuration admission.
   Preparation runs outside the request-admission gate.
3. Under the same gate as native request entry, recheck revision, process identity,
   readiness and all request/tool/event lifetimes. A newly admitted request wins;
   the update returns `busy` and discards the prepared Host.
4. If idle, durably record and atomically publish the revision. Return instance,
   previous/active revision, audit key, preparation time and activation time.

Busy, failed preparation, conflicts and cancellation do not queue or schedule a
later activation. Retain the old policy and release the candidate. Preparation
and gate ownership are bounded; there is no tool-drain wait or in-flight tool/
middleware replacement. Every native tool keeps one revision through before/after.
An RPC timeout can be indeterminate; inspect the active revision before deciding
what to do next, and never dispatch a request requiring an unconfirmed revision.

The control plane owns exclusive instance assignment and serializes its policy
selection with Session dispatch. Activation acknowledgement is not a reservation
for a future request. Policies are per instance, not per Session; an App serving
several Sessions has one policy and any active request makes updates busy.
Per-Session policies are outside this release.

Zero AW events or tools is not whole-request idle. Model waits remain busy.
Native terminal evidence retains unfinished tools after a request returns,
including background work. Unknown completion remains busy; it never enables a
switch. Original callback correlations remain claimed across revisions, so old
callbacks cannot acquire a fresh execution budget. Event evidence uses the
owning Host's revision and policy constraints, not the bootstrap configuration.

The wire protocol is `aw-service/v1alpha7`. Old daemons and bindings fail explicit
version/generation checks. Process association is independent of optional
observation; it records root PID, start ticks, UID and PID namespace, corroborates
bridge ancestry and rechecks lifetime before activation. Audit stores association
and transitions without Provider configuration values.

### Update authority and audit

Runtime updates require the same trust as editing startup configuration: they
can replace a Provider's `transport.argv` or disable required before Hooks.
Only the trusted same-user control plane may submit them through the existing
private service socket and peer-UID checks. Validate the target service
generation, instance and expected revision; an instance binding or native Hook
callback is not update authority. These same-user controls do not isolate an
Agent or another tenant running under the same UID. Deployments requiring that
boundary must isolate control-plane authority outside this protocol.

Before enabling runtime updates, require metadata-only audit records for each
attempt: verified caller UID/PID, service generation, instance, previous and
candidate revision digests, outcome/reason, and the revision actually activated.
Do not record configuration bodies, Provider arguments, credentials or native
payloads. Keep failed and busy attempts distinguishable from activation, and
reject activation when the required audit record cannot be persisted.

### Later: steer at an Agent-loop boundary

If demanded later, an explicit update could remain pending until an
Adapter-confirmed safe point within an active request. Native loop notifications
or the planned generic Hook mechanism may supply that boundary. The point after
a tool batch while waiting for a model response is a candidate, not proof of
safety: confirm no active tools, prevent concurrent tool dispatch and verify that
the Adapter can select the revision for subsequent hooks without rebuilding an
active middleware chain. Already sent model requests and completed hooks are
not changed retroactively. Pending/accepted is distinct from active; report
activation only after the switch.

No steer queue, automatic deferred activation, idle polling heuristic or new
generic Hook event is required by the first runtime release.

## Qoder stable bridge and external attachment

Reloadable launches retain temporary `--settings` for stable bridge registration;
policy changes happen in AW and do not rely on native settings-file watching.
The pinned TUI entrypoint reports SessionStart, UserPromptSubmit and idle-prompt
Notification boundaries. Stop is not idle: a native Stop hook can continue the
Agent. Tool failures and permission denial close tool lifetimes without adding
new Provider events. Missing idle notifications conservatively retain busy.
Ordinary print/TUI launches remain unchanged; print is not a reusable reload host.

External attachment uses the native user settings source with an exclusive HOME
and project scope. It reuses the stable routes but confirms registration through
`ConfigChange`, because SessionStart has already happened. A bounded second
configuration change invokes the newly registered callback before user input.
The callback corroborates the source path and native process, activates Ready
and writes a nonce-bearing receipt. Its failure never vetoes unrelated settings
edits. Native workspace trust still requires the caller's native approval.

Installation merges owned groups into the latest settings. Removal first uses
Unbind's atomic whole-request idle check and closes service admission, then
replaces owned tool/lifecycle groups with a temporary confirmation route. Native
adoption of that route proves the tool groups have been removed. The final
confirmation route and private binding artifacts are then removed. User groups
and unrelated fields survive; no stale whole-file snapshot is restored.
Publication retains a displaced racing write for inspection and fails visibly.
A newly created empty native settings document is retained after removal.

## QwenPaw stable bridge and external attachment

Preinstall one stable middleware bridge. `on_reply` tracks the complete native
request, including model waits; `on_acting` tracks the actual tool generator.
Callbacks obtain the active plan from AW, preserving native scheduling, denial
and final replacement. Only normal generator exhaustion releases request/tool
claims; a policy denial before native tool entry also releases that tool claim.
AgentScope 2.0.9 does not propagate wrapper closure through every child generator.
A final response or successful wrapper `aclose()` therefore cannot prove native
cleanup. Hold the final tool response and ERROR/INTERRUPTED chunks until normal
exhaustion and the end notification: QwenPaw's ToolCoordinator stops pulling at
any of these. A terminal chunk may be followed by Toolkit's accumulated response;
other output after a terminal chunk or final response is rejected. Nonterminal
chunks remain streaming. Normal structured errors release occupancy after cleanup.
Early closure, cancellation and exceptions before confirmed completion retain busy; use a fresh
instance when completion cannot be established. For external instances, use the target App's plugin-management
interface and correlate its endpoint, working directory and executing process.

Hot installation uses the same pinned AW plugin and startup-ready callback. The
2.2.2b4 native plugin API invokes startup callbacks during hot install. An owned
`aw-attachment.json` descriptor beside the installed plugin replaces the need
for post-start environment injection. The CLI verifies that the selected App PID
owns its loopback port; success also requires that PID's nonce-bearing receipt.
API success alone does not prove callback success. Existing-Session subsequent
requests and a second independent profile are covered by native acceptance.

Removal checks complete native idle admission before invoking the native unload
API, verifies the installed descriptor and assets still belong to this attachment,
and preserves the externally owned App. It does not replace middleware already
assembled for a request; first attachment requires a quiescent pre-request App.

## Process association and ownership

Record generation, instance, attachment and revision alongside root PID, start
time, UID, PID namespace, native profile and framework version. Corroborate
session/tool-call IDs and callback origins; do not trust self-reported fields or
the executable name alone. Use pidfd where needed to track lifetime. This is
same-user process association, not a cross-user authentication boundary.

`aw run` continues owning its native child and signal/reaping lifecycle. External
attachment owns only its registration, binding and callback resources. Detach
must not kill that agent. Process exit invalidates the association. First release requires explicit detach
before the caller stops its process; it does not add a process supervisor or
automatic attachment garbage collector. Failed detach retains the ownership
record for retry; a replaced process or service generation is rejected.

## Delivery and regression acceptance

1. Offline comparison: implemented in this slice.
2. Stable bridges, instance snapshots, readiness, explicit idle-only activation,
   busy rejection and process association: implemented for the pinned entrypoints.
   CLI seam regressions and AgentScope middleware tests cover the contracts;
   the native inventory requires a separate Qoder 1.1.73 TUI startup/idle-switch
   case before dispatch. Full hosted TUI tool-turn adoption and QwenPaw App
   reload acceptance remain separate checks. The 1.1.64 print lane is retained.
3. Native attach/detach through `aw install --pid` / `--remove`, with scoped
   registration, native confirmation and unchanged PID/start-ticks acceptance.
4. Incremental Adapter support for other frameworks. OpenClaw/Hermes and then
   Pi/Claude SDK/AgentScope are candidates; reload and attachment are independent
   capabilities, and SDK integrations may require a preinstalled bridge.

For each runtime integration, test same-process add/update/remove, first-request
revision selection, failed preparation rollback, busy rejection during active
requests/model waits/long-running tools without deferred activation, unknown idle
support, update/request admission races, parallel updates, all terminal outcomes,
stale association and cleanup. Steer requires separate later acceptance. Preserve
security and native vetoes, Tokenless fresh-call retry checks, final authorized text
replacement, native scheduling and existing CLI/config behavior. Keep metadata
audit and observation independent of optional views.

Extend regression/native inventories with exact cases and pinned versions before
advertising support. Measure agent startup, bridge confirmation, policy
preparation and activation separately; offline checks and callback fixtures are
not native adoption acceptance.
