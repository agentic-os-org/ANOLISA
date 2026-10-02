# ActPlane Enforcement Integration

## Status

Approved design for an experimental end-to-end integration between AgentSight and
ActPlane. Implementation is intentionally staged, but the first usable milestone
must perform a real BPF-LSM denial on Linux and return the resulting violation to
the AgentSight audit API.

## Summary

AgentSight remains the user-facing observability, audit, and policy-coordination
component. A separately supervised `agentsight-enforcer` process owns the
privileged ActPlane runtime and exposes a small, versioned Unix domain socket
protocol. ActPlane remains the kernel enforcement engine. The workspace declares
a pinned upstream Git revision (`a62e5d9d`) as the canonical dependency, with a
`[patch]` override pointing to vendored copies under `crates/`. This dual-track
approach preserves the attested build path (which fetches from the git source and
applies `patches/actplane/*.patch`) while giving workspace builds and the
compile-time ABI guard direct access to the local source tree.

This is product-level consolidation with process-level privilege isolation and a
protocol-level anti-corruption boundary.

## Goals

- Give users one AgentSight product surface for observation and enforcement.
- Prove a complete `Exec -> attach -> BPF-LSM deny -> audit event` path.
- Keep BPF loading and policy execution outside the AgentSight API process.
- Consume official ActPlane without maintaining an ANOLISA fork.
- Preserve AgentSight's existing libbpf-based observation probes unchanged.
- Make execution state explicit so a requested policy is never presented as an
  active policy until ActPlane confirms attachment.
- Provide restart reconciliation and actionable degraded-state reporting.

## Non-goals

- Rewriting AgentSight probes from libbpf-rs to Aya.
- Sharing BPF maps, ring buffers, loaders, or generated artifacts with ActPlane.
- Migrating ResourceGov, learned baselines, MCP project initialization, or the
  complete `oslevel-harness` API in the first milestone.
- Automatically deriving blocking policy from AgentSecCore findings.
- Changing the existing AgentSecCore UDS integration.
- Splitting AgentSight trace and HTTP serving into separate systemd units.

## Current State

AgentSight runs a libbpf-rs observation pipeline and exposes an Actix API on port
7396. Its systemd unit starts `agentsight trace` and `agentsight serve` together.
It already discovers Agent processes from procmon events and can associate system
activity with Agent metadata.

`oslevel-harness` currently adds Agent registration, policy binding, Agent scope,
ActPlane loading, violation conversion, feedback, and REST/SSE endpoints. It also
contains a modified ActPlane source snapshot. Keeping that arrangement would
duplicate AgentSight process tracking and retain a long-lived downstream fork.

Official ActPlane 0.1.8 exposes compiler, runtime, and BPF engine library crates.
The experimental integration will initially pin upstream revision
`a62e5d9d96f91101cda019519053e950d532380a`. Upgrades require an explicit revision
change and Linux regression run.

## Considered Approaches

### Direct official library integration

Link the pinned official ActPlane crates only from `agentsight-enforcer` and hide
their types behind an internal backend trait. This provides typed errors and the
shortest violation path. It is the selected approach.

### Official CLI subprocess

Run the `actplane` binary per binding and translate its files or process output.
This strengthens binary isolation but complicates process supervision, detach,
event delivery, and failure attribution. It remains a fallback if the upstream
library cannot expose a required lifecycle operation.

### Retain `oslevel-harness` as a sidecar

Connect AgentSight to the existing harness REST/SSE API. This is the fastest
demonstration but preserves duplicate control planes and user-visible components,
so it is rejected as the target architecture.

## Architecture

```text
agentsight.service
  trace / procmon / discovery / session / audit / API
                         |
                         | versioned UDS protocol
                         v
agentsight-enforcer.service
  peer authorization / desired-vs-actual state / ActPlaneAdapter
                         |
                         | pinned official Rust API
                         v
Official ActPlane
  DSL compiler / runtime domain / BPF-LSM / violation stream
                         |
                         v
Linux kernel
```

AgentSight decides which policy should apply to an Agent and records the result.
The enforcer owns privileged execution state. ActPlane makes the kernel decision.
No layer imports the private types of the layer below except the single adapter.

## Component Boundaries

### AgentSight enforcement coordinator

The coordinator consumes Agent lifecycle information, resolves a configured
policy binding, sends idempotent requests to the enforcer, and persists desired
and reported state. It does not load BPF programs or interpret ActPlane maps.

The first milestone also exposes authenticated localhost API operations for an
explicit attach, detach, status query, and violation query. Explicit attach makes
the Linux acceptance test deterministic; automatic binding from Agent discovery
uses the same coordinator operation.

### Enforcement protocol

The protocol is an AgentSight-owned crate with no ActPlane dependency. Messages
use bounded, newline-delimited JSON over a Unix stream. Every request includes:

- `protocol_version`
- `request_id`
- `binding_id`
- one typed command

The first protocol version supports:

- `Health`
- `ApplyPolicy`
- `DetachAgent`
- `ListBindings`
- `SubscribeViolations`

`ApplyPolicy` carries `agent_id`, optional `session_id`, `root_pid`,
`process_start_time`, `policy_id`, `policy_revision`, and policy DSL. The
`binding_id` is the idempotency key. Repeating an identical request returns the
current binding; reusing it with different content returns a conflict.

Request/response traffic and the long-lived violation subscription use separate
connections so a slow event consumer cannot block lifecycle operations.

### agentsight-enforcer

The enforcer validates requests, checks peer credentials, verifies PID identity,
serializes ActPlane lifecycle mutations, and translates upstream errors and
violations into stable protocol types. Its backend trait supports a deterministic
mock implementation for non-Linux tests and one production ActPlane adapter.

Only the production adapter imports ActPlane crates. The initial pin is a full
Git SHA, never a moving branch. If the required arbitrary-PID lifecycle operation
is not available as a stable upstream function, the adapter may use the official
engine crate internally while an upstream API proposal is prepared; the usage
must remain confined to that file.

### ActPlane

ActPlane compiles policy DSL, creates the runtime domain, seeds the Agent root
process, attaches BPF-LSM hooks, performs `notify`, `block`, or `kill`, and emits
the raw violation. ActPlane does not own AgentSight sessions, HTTP APIs, storage,
or user-facing audit semantics.

## Binding State Model

AgentSight persists desired state and the last reported actual state:

```text
pending -> enforced -> detaching -> detached
    |          |
    v          v
  failed    degraded
```

- `pending`: request accepted locally but no ActPlane confirmation exists.
- `enforced`: ActPlane confirmed policy attachment for the verified process.
- `failed`: compilation, validation, or initial attachment failed.
- `degraded`: a previously enforced binding cannot currently be verified.
- `detaching`: removal was requested but not confirmed.
- `detached`: ActPlane confirmed removal or the process no longer exists.

Only `enforced` means the policy is active. HTTP responses and the Dashboard must
not infer enforcement from request delivery alone.

## End-to-End Flows

### Attach

1. Procmon reports `Exec`, or an authenticated localhost API requests attachment.
2. AgentSight captures PID and process start time and creates a pending binding.
3. The coordinator sends `ApplyPolicy` over UDS.
4. The enforcer re-reads process identity to reject stale or reused PIDs.
5. ActPlane compiles the DSL, creates a domain, seeds the root process, and loads
   required hooks.
6. The enforcer returns the actual policy revision and attachment status.
7. AgentSight changes the binding to `enforced` and writes an audit event.

### Violation

1. A protected process attempts an operation.
2. An ActPlane BPF-LSM hook evaluates policy before the operation commits.
3. A block returns `EPERM`; a kill sends `SIGKILL`; notify allows the operation.
4. ActPlane emits a raw violation.
5. The adapter produces a protocol `ViolationEvent`.
6. AgentSight enriches it with Agent and session context and stores it in SQLite.
7. The audit API returns the event, including whether the operation was actually
   blocked or killed.

### Detach and exit

Explicit detach and Agent exit use the same idempotent operation. The coordinator
marks the binding `detaching`, the enforcer removes its ActPlane domain state, and
AgentSight records `detached` only after confirmation. A missing process is a
successful terminal condition if no live PID with the recorded start time exists.

### Restart reconciliation

After reconnect, AgentSight requests `ListBindings` and compares actual bindings
with SQLite desired state. Missing desired bindings are reapplied only after PID
identity validation. Unknown actual bindings are reported and safely detached
according to their recorded ownership; they are never silently adopted.

## Violation Event Contract

The stable event contains:

- identity: `event_id`, `binding_id`, `agent_id`, optional `session_id`
- policy: `policy_id`, `policy_revision`, `rule_id`, optional rule name/reason
- process: `pid`, `process_start_time`, optional parent PID and command
- operation: normalized operation and target
- decision: requested effect, `blocked`, and `killed`
- timing: kernel occurrence time and AgentSight observation time
- source: protocol version, enforcer version, and ActPlane revision

An event must preserve the distinction between requested effect and observed
result. A block rule on a host without active BPF-LSM is not a blocked event.

## Failure and Security Semantics

The first milestone is explicitly fail-open for new attachment failures. Agent
execution continues, while AgentSight records a prominent
`enforcement_unavailable` or `enforcement_failed` event. Existing policy state is
reported from ActPlane rather than assumed from enforcer process health.

The enforcer rejects malformed frames, unsupported protocol versions, oversized
messages, stale PIDs, conflicting idempotency keys, and unauthorized peers. The
socket is created under `/run/agentsight/enforcer.sock`, owned by root with group
`agentsight` and mode `0660`. Linux peer credentials are checked in addition to
filesystem permissions.

Policy DSL and errors are size-bounded before logging. Secrets or captured file
contents are not included in lifecycle logs or violation events.

## systemd Packaging

The first milestone keeps the existing AgentSight trace/serve launcher and adds a
separate unit:

```text
agentsight-enforcer.service
  Before=agentsight.service
  PartOf=agentsight.service
  RuntimeDirectory=agentsight
```

The enforcer runs with the minimum capabilities verified by the Linux test. The
unit applies restart limits and hardening without preventing BPF and LSM access.
AgentSight reports the enforcer health through its existing health surface.

## oslevel-harness Migration Boundary

The first milestone reimplements only the small, stable semantics needed for the
new boundary:

- Agent-scoped binding and PID identity validation
- idempotent attach/detach lifecycle
- violation normalization
- corrective failure messages suitable for AgentSight audit

Process discovery remains in AgentSight. REST/SSE is replaced by AgentSight API
and internal UDS. The local ActPlane snapshot is not copied. ResourceGov,
baseline scoring, and project initialization remain out of scope. The untracked
`oslevel-harness` directory is treated as reference material and is not modified.

## AgentSecCore Compatibility

The existing AgentSecCore query integration remains unchanged. Enforcement
events reserve `session_id` and correlation fields so a later milestone can join
semantic findings with system decisions. AgentSecCore will not directly load an
ActPlane policy in this milestone; future policy recommendations must pass
through the AgentSight coordinator and its audit trail.

## Testing Strategy

### Platform-independent tests

- Protocol request, response, and event round trips.
- Unsupported-version and oversized-frame rejection.
- Binding idempotency and conflict detection.
- Coordinator transitions for success, timeout, disconnect, and stale PID.
- Mock backend attach, detach, list, and violation subscription.
- Violation conversion and SQLite persistence.

### Linux integration tests

The acceptance host is `11.164.2.206`, currently running Linux 5.10 with BTF,
BPF-LSM, root SSH access, and systemd. Tests must not modify unrelated host
services or paths.

The acceptance scenario uses a dedicated temporary directory and policy:

1. Build AgentSight and the pinned ActPlane revision.
2. Install and start the enforcer and AgentSight units.
3. Start a dedicated test process and record its start time.
4. Attach a policy that blocks an operation under the temporary directory.
5. Verify the operation fails with `EPERM`.
6. Verify AgentSight returns one matching event with `blocked=true`.
7. Detach and verify the same operation succeeds.
8. Restart the enforcer and verify health and reconciliation behavior.
9. Submit invalid DSL and a stale PID and verify neither is marked enforced.

Every remote mutation must be confined to a named test directory, installed
AgentSight artifacts, and AgentSight systemd units. Test cleanup removes those
artifacts and restores the pre-test service state.

## Delivery Sequence

1. Add the protocol crate, mock backend, and protocol tests.
2. Add the enforcer daemon, UDS authorization, and systemd unit.
3. Add the pinned official ActPlane adapter and Linux backend tests.
4. Add the AgentSight coordinator, binding persistence, and client.
5. Add audit ingestion and authenticated enforcement API operations.
6. Run the real Linux denial, detach, error, and restart acceptance scenarios.

Each step must remain independently testable and must not require committing the
vendored `oslevel-harness` or ActPlane snapshots.

## Acceptance Criteria

- Users install and operate AgentSight without an `oslevel-harness` service.
- AgentSight can attach and detach one policy for a verified Agent process.
- A real BPF-LSM denial returns `EPERM` on the Linux acceptance host.
- The same denial is queryable from AgentSight with `blocked=true` and matching
  process and policy identity.
- Duplicate attachment requests are idempotent.
- Enforcer absence and ActPlane failures are visible and never reported as
  successful enforcement.
- The existing AgentSight observation pipeline continues to operate unchanged.
- The repository contains no new vendored ActPlane source copy.
