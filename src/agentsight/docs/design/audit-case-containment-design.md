# Audit Case Containment Design

## Status

Approved design for AgentSight case containment.

## Goal

Turn a confirmed credential-exfiltration audit case into a bounded ActPlane enforcement
binding from the case detail page. The workflow must preserve process identity, default to a
15-minute temporary block, prove containment with a real kernel denial, and retain every
response action for later review.

The user-facing flow is:

```text
audit case
  -> containment confirmation
  -> enforce binding
  -> kernel block evidence
  -> contained case
  -> manual resolution
```

## Non-goals

- Accepting arbitrary ActPlane DSL from the Dashboard.
- Automatically resolving a case when a binding is created.
- Binding policies to every future process with the same Agent name.
- Adding a distributed response queue or a separate response service.
- Expanding beyond the existing credential-exfiltration policy template.

## Design Decisions

### Backend-owned orchestration

Containment is one case-level backend operation. The Dashboard must not independently call the
binding and review APIs because partial frontend orchestration could create an active policy
without updating the case, or update the case without attaching the policy.

The backend derives the enforcement policy from the reviewed case, its persisted original
binding, and an approved policy template. Evidence paths are display-redacted and must never be
expanded or guessed for enforcement. It persists the desired response action before crossing the
enforcer boundary, then reconciles the acknowledgement into the action state.

### Review state and containment state remain separate

The existing case review state retains its current meaning:

- `open`: awaiting review.
- `confirmed`: reviewed as a real risk.
- `false_positive`: reviewed as benign.
- `accepted_risk`: accepted without remediation.
- `resolved`: manually closed after remediation.

Containment has an independent lifecycle:

- `pending`: persisted but not acknowledged by the enforcer.
- `active`: the enforce binding is attached.
- `expiring`: detachment is due or in progress.
- `expired`: detachment was acknowledged.
- `failed`: attachment or detachment could not be completed after bounded retries.

An action is displayed as **contained** only after AgentSight receives a normalized security
event for its binding with `blocked=true`. Creating a binding alone does not prove containment.

### Process identity is PID plus start time

The original binding remains eligible only when its root PID is alive and its Linux process
start time matches the stored value. This prevents a recycled PID from inheriting a policy.

When the original process is stale, the containment plan requires the user to select a live
Agent process. The Dashboard sends only the selected PID; the server reads the start time from
`/proc/<pid>/stat`, and the enforcer validates the same identity again while attaching.

### Temporary enforcement is the safe default

The confirmation dialog defaults to 900 seconds. A user may explicitly choose a persistent
binding. Temporary bindings are detached by AgentSight after their deadline; restarting
AgentSight must not lose pending expiry work.

## Data Model

Add a `containment_actions` table owned by the security store:

| Column | Purpose |
|---|---|
| `action_id` | Stable idempotency identifier. |
| `case_id` | Source risk case. |
| `binding_id` | Enforcer binding created by the action. |
| `agent_id` | Product Agent identity used for display and correlation. |
| `root_pid` | Selected process-tree root. |
| `process_start_time` | Linux start time used to reject PID reuse. |
| `source_path` | Canonical absolute sensitive source path from the case. |
| `duration_secs` | Temporary duration; `NULL` means explicitly persistent. |
| `expires_at_ns` | Detachment deadline for temporary actions. |
| `lifecycle_state` | Pending, active, expiring, expired, or failed. |
| `blocked_at_ns` | First confirmed kernel denial for this binding. |
| `requested_by` | Authenticated principal, or `dashboard-token` until user identity exists. |
| `failure_stage` | Attach, detach, or reconciliation stage when an action fails. |
| `failure_reason` | Sanitized actionable failure detail. |
| `attempt_count` | Bounded reconciliation attempt counter. |
| `next_retry_at_ns` | Persisted retry deadline used across restarts. |
| `created_at_ns` | Action creation time. |
| `updated_at_ns` | Last lifecycle update time. |

The latest action is returned with case detail. Review state remains stored in `risk_cases`;
containment is derived from the latest action and immutable security events.

## API

### Build a containment plan

```http
GET /api/audit/cases/{case_id}/containment-plan
```

The response contains:

- Case and approved policy-template identifiers.
- Canonical sensitive source path recovered from the persisted original binding.
- Original binding and root process identity.
- Whether the original process identity is currently valid.
- Eligible live Agent processes when replacement is required.
- A default duration of 900 seconds and a permitted temporary range of 60 to 86,400 seconds.
- Any active or pending containment action that makes a new action redundant.

The plan never returns sensitive file contents or raw network payloads.

### Create containment

```http
POST /api/audit/cases/{case_id}/contain
Content-Type: application/json

{
  "root_pid": 1832,
  "duration_secs": 900
}
```

The server performs these steps:

1. Load the case and reject cases marked false positive, accepted risk, or resolved.
2. Recover the canonical source path from the persisted original binding and derive the
   credential-exfiltration enforce policy from the approved template.
3. Validate the selected PID and read its current start time.
4. Persist an idempotent pending containment action and binding identifier.
5. Submit the desired binding through the existing enforcement coordinator.
6. Mark the action active only after the enforcer acknowledges attachment.
7. Mark the case confirmed only after the action becomes active.

Repeated requests for the same case, process identity, and active policy return the existing
action rather than attaching another binding.

The pending action reserves foreground ownership through the complete bounded health, apply, and
post-apply health sequence, plus one configured request-timeout margin. Each ordinary enforcer
request uses one absolute deadline across connect, frame write, and frame read. Restart
reconciliation may claim a pending action only after its ownership deadline; a temporary action
expiry cannot preempt foreground ownership.

### Case detail

The existing case-detail response gains a containment summary containing the action lifecycle,
binding identifier, selected process identity, expiry time, first block time, and sanitized
failure reason.

## Expiry and Reconciliation

AgentSight runs a bounded reconciliation loop inside the existing enforcement coordinator:

1. Find temporary active actions whose deadline has passed.
2. Mark the action expiring and request binding detachment.
3. Mark it expired only after the enforcer acknowledges detachment.
4. Retry the first four transient failures with 1, 2, 4, and 8 second backoffs; the fifth failed
   detach attempt is terminal and transitions the action to `Failed`.
5. Mark terminal failures visible instead of silently treating them as detached.

At startup, the same loop reconciles pending, active, and expiring actions against
`ListBindings`. This handles lost HTTP responses, AgentSight restarts, and an enforcer restart
without duplicating bindings.

Containment never bypasses the enforcement coordinator's required-subscription lease. Each
apply, persistence transition, and binding snapshot is fenced by the current ingestion
subscription and enforcer generation. A disconnect, re-acknowledgement, or generation change
invalidates the operation before AgentSight can mark an action active. Recovery is idempotent:
the persisted binding identifier is reused and stale snapshots cannot revive a superseded
generation.

## Event Correlation

The security coordinator matches normalized enforcement events by `binding_id`. When the binding
belongs to a containment action, its file, taint, network, and decision evidence is appended to
the action's source audit case instead of opening a second case. The first event with
`blocked=true` sets `blocked_at_ns`, upgrades the source case to critical and blocked, and raises
its risk score to the enforce decision score. Duplicate DNS attempts and repeated kernel events
remain immutable evidence but do not create additional containment actions or cases.

The case detail displays four product states:

```text
awaiting review -> confirmed/active -> contained -> manually resolved
```

An expired action can remain contained because expiry describes policy lifecycle while
containment describes an observed outcome.

## Dashboard Interaction

The case detail page adds an **Upgrade to enforcement** action for eligible high-risk audit
cases. Its confirmation dialog shows:

- Agent identity and selected PID.
- PID identity validity.
- Sensitive source path.
- Public IPv4 destination policy scope.
- ActPlane deny effect.
- A selected 15-minute temporary duration.
- An explicit option for persistent enforcement.

If the original PID is stale, the dialog replaces it with a required live-Agent selector backed
by AgentSight discovery and health data while preserving the case, path, and policy context. If
the original binding is unavailable, containment is ineligible because the redacted evidence
path is not safe policy input. The case page then shows the binding state, countdown, response
history, first blocked target and errno, expiry, and failures. Until Dashboard authentication has
named users, response history attributes actions to `dashboard-token` rather than claiming a
human identity that AgentSight cannot prove.

## Failure Handling

- A stale process before submission returns `root_process_stale` with eligible replacements.
- A missing original binding returns `source_policy_unavailable`; AgentSight never expands a
  redacted evidence path into an enforcement target.
- A process that exits during attachment leaves the action failed and the case unchanged.
- An unavailable enforcer returns service unavailable and does not confirm the case.
- A lost acknowledgement is reconciled through the persisted binding ID and `ListBindings`.
- A failed automatic detach remains visible as expiring or failed and is retried only within a
  bounded policy.
- Duplicate containment requests and duplicate block events are idempotent.
- All API errors are sanitized; file contents, policy internals, and raw payloads are excluded.

## Security Boundaries

- The Dashboard cannot submit raw policy DSL.
- Only reviewed, server-owned templates can produce an enforce binding.
- The server validates the source case, selected process identity, duration bounds, and current
  enforcement health.
- A persistent binding requires an explicit user selection in the confirmation dialog.
- Case resolution remains a separate manual action.

## Verification

### Unit tests

- Extract a containment plan from an audit case.
- Reject stale and recycled PIDs.
- Validate temporary and persistent duration inputs.
- Verify action lifecycle transitions and idempotency.
- Compute expiry deadlines without wall-clock-dependent tests.

### API integration tests

- Attach successfully and confirm the case only after acknowledgement.
- Require PID replacement when the original identity is stale.
- Preserve case state after attachment failure.
- Reconcile a lost response without creating a second binding.
- Restore expiry work after restart and acknowledge automatic detachment.

### Dashboard tests

- Open the confirmation dialog with case-derived values.
- Switch from a stale process to an eligible online Agent.
- Select temporary or persistent enforcement.
- Display countdown, active, contained, expiring, expired, and failed states.
- Prevent duplicate submission while an action is pending.

### Linux end-to-end test

1. Run Hermes under an audit binding and create one unblocked exfiltration case.
2. Upgrade the case using the minimum 60-second temporary duration.
3. Repeat the file-read and external-connect action.
4. Verify the kernel returns `EPERM`, the event reports `blocked=true`, and the case displays
   contained.
5. Wait for automatic detachment and verify the same operation is allowed again.

## Success Criteria

- One case-page operation creates a time-bounded kernel enforcement binding.
- PID reuse cannot redirect a case response to an unrelated process.
- A case is shown as contained only after a real kernel denial.
- Temporary enforcement survives AgentSight restart and detaches at expiry.
- Every response transition and failure remains visible and auditable.
