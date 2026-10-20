# Policy reload and native attachment

[中文版](policy-reload_zh.md)

This design tracks [issue #8459](https://github.com/agentic-os-org/ANOLISA/issues/8459).
The implemented slice is offline policy-scope comparison through
`aw validate --config CANDIDATE --against BASELINE`. Runtime activation and
external attachment remain planned; existing launch and service behavior applies.

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

## Planned: prewarming and activation

Prewarming installs stable before/after bridge entrypoints even if the default
policy has no steps. Concrete step lists must no longer be fixed exclusively in
launch-time native configuration. Adapter-owned dispatch must preserve native
sequential/concurrent scheduling and existing user Hooks.

Separate the service generation, native instance identity, attachment ownership
and immutable policy revision. Current service resolution hashes the full input
document; locating an existing instance for an update must not accidentally
select a new daemon from the candidate's digest. Each instance has its own active
policy; updating one instance must not change another.

The first runtime release targets infrequent explicit control-plane updates,
primarily prewarmed containers: **explicit update, idle-only activation, busy
rejection**. Start the container, AW and native Agent under the default policy;
confirm the stable bridge and native readiness. Before dispatching a request
that needs a new policy, submit the update and wait for the activated revision.
The native entrypoint must support reuse across requests; process liveness alone
is not request readiness.

1. Compare policy scope and the expected active revision.
2. Prepare a candidate through existing capability admission and Host preparation.
3. Under the admission gate shared with incoming requests, recheck the revision
   and obtain Adapter-supported whole-request idle confirmation, with no active
   tool invocations.
4. If busy, return `busy` without waiting, queuing or scheduling later activation.
   Release candidate resources and keep the old revision. If the Adapter cannot
   establish idle, return `unsupported`.
5. Otherwise reserve that idle boundary, atomically publish and audit the
   revision, then reopen admission and return the revision actually activated.

An early busy check may avoid preparation, but cannot replace the final gated
check. Preparation failure, revision conflict or cancellation keeps the previous
revision active and releases owned resources. Preparation and gate ownership
remain bounded; this release has no tool-drain wait. A caller requiring a new
revision must not submit its request under the old revision.

Whole-request idle is stricter than zero service events or zero tool calls.
An active Agent request, including a remote model wait, is busy in this release.
A tool may still execute after its before callback returns. Native request
lifecycle evidence must establish that success, failure, denial or cancellation
has left no tool invocations active before declaring idle. A tool invocation
retains one revision through before/after; in-flight tool or middleware
replacement is outside this release.

Service protocol changes need an explicit version and stale-binding rejection.
Keep original event inputs, once-only step claims and budgets; a policy update
must not renew an existing call or replace its pinned evidence context. Optional
observation must not become a dependency of process association or enforcement.

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

## Planned: Qoder

AW-launched instances can retain temporary `--settings` for stable bridge
registration. Policy changes then occur in AW without relying on that file's
native watcher behavior.

External attachment instead needs a native settings source that reloads Hooks.
Require a verified version/entrypoint and an exclusive configuration scope:
shared HOME or project settings cannot establish PID-specific attachment.
Check trust, disabled Hooks and configuration sources before registration.
Preserve unrelated Hooks and concurrent edits; detach removes only AW-owned
entries rather than restoring an entire old settings file.

Writing settings is not readiness. Require a correlated handshake with the
target process and separate native adoption evidence. Complete terminal-path
tracking: the current Adapter maps successful `PostToolUse`, while failure,
denial and cancellation need completion evidence. Lifecycle-only notifications
do not expand the structured Provider event catalog automatically.
Print and TUI need separate pinned native acceptance.

## Planned: QwenPaw

Preinstall a stable middleware bridge and select the policy at an admitted
request boundary. For external instances, use the target App's plugin-management
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
2. Stable bridges, instance snapshots, readiness and explicit idle-only activation
   for prewarmed Qoder/QwenPaw, with busy rejection and process association.
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
