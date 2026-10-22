# Policy reload and native attachment

[中文版](../../zh/runtime/policy-reload.md)

AW can explicitly update the policy of an idle instance launched with a stable
bridge. The native host process stays alive; busy instances reject the update.
The runtime path is opt-in through `aw run --reloadable`, initially pinned to
Qoder 1.1.73 TUI and QwenPaw 2.2.2b4 / AgentScope 2.0.9 App. External attachment
and steer remain planned. Ordinary `aw run` keeps its existing registration path.

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
metadata. Use `aw update --config current.yaml --agent TARGET --policy next.yaml`
to resolve the original service, select the unique target instance and read its
revision. Multiple matches require `--instance`; `--socket` and
`--expected-revision` allow explicit selection and concurrency preconditions.
The observed revision still guards activation; conflicts never retry silently.
No internal callback assembly is required. Editing a file alone never activates it.

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

The wire protocol is `aw-service/v1alpha4`. Old daemons and bindings fail explicit
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

## Qoder stable bridge and future attachment

Reloadable launches retain temporary `--settings` for stable bridge registration;
policy changes happen in AW and do not rely on native settings-file watching.
The pinned TUI entrypoint reports SessionStart, UserPromptSubmit and idle-prompt
Notification boundaries. Stop is not idle: a native Stop hook can continue the
Agent. Tool failures and permission denial close tool lifetimes without adding
new Provider events. Missing idle notifications conservatively retain busy.
Ordinary print/TUI launches remain unchanged; print is not a reusable reload host.

External attachment instead needs a native settings source that reloads Hooks.
Require a verified version/entrypoint and an exclusive configuration scope:
shared HOME or project settings cannot establish PID-specific attachment.
Check trust, disabled Hooks and configuration sources before registration.
Preserve unrelated Hooks and concurrent edits; detach removes only AW-owned
entries rather than restoring an entire old settings file.

Writing settings is not readiness. Require a correlated handshake with the
target process and separate native adoption evidence. Successful `PostToolUse` retains the existing policy path. Lifecycle-only notifications
do not expand the structured Provider event catalog automatically.
Print and TUI need separate pinned native acceptance.

## QwenPaw stable bridge and future attachment

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

Use plugin configuration or a protected local descriptor for bridge discovery.
A running App cannot inherit `AW_NATIVE_CONFIG` set by a later launcher.
Add a hot-install acknowledgement independent of the startup Hook. Distinguish
plugin installed, runtime registration committed and request interception.
Middleware constructed for an active request must not be assumed to change when
a plugin is installed; verify subsequent requests in existing sessions separately.

Current AW pins QwenPaw 2.2.2b4 / AgentScope 2.0.9. Select and accept a precise
version pair before enabling hot installation; do not simply loosen version
checks. Drain existing invocations before removing AW-owned registrations and
leave externally supervised processes running.

## Process association and ownership

Record generation, instance, attachment and revision alongside root PID, start
time, UID, PID namespace, native profile and framework version. Corroborate
session/tool-call IDs and callback origins; do not trust self-reported fields or
the executable name alone. Use pidfd where needed to track lifetime. This is
same-user process association, not a cross-user authentication boundary.

`aw run` continues owning its native child and signal/reaping lifecycle. External
attachment owns only its registration, binding and callback resources. Detach
must not kill that agent. Process exit invalidates the association and releases
owned resources without touching user configuration or other instances.

## Delivery and regression acceptance

1. Offline comparison: implemented in this slice.
2. Stable bridges, instance snapshots, readiness, explicit idle-only activation,
   busy rejection and process association: implemented for the pinned entrypoints.
   CLI seam regressions and AgentScope middleware tests cover the contracts;
   the native inventory requires a separate Qoder 1.1.73 TUI startup/idle-switch
   case before dispatch. Full hosted TUI tool-turn adoption and QwenPaw App
   reload acceptance remain separate checks. The 1.1.64 print lane is retained.
3. Native attach/detach for those frameworks after scoped registration acceptance.
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
