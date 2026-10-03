# Security Audit and Progressive Enforcement

## Status

Approved product and architecture design. This document refines the security
capabilities built on top of the ActPlane integration described in
[`actplane-enforcement-integration.md`](actplane-enforcement-integration.md).
Implementation is out of scope for this document.

## Summary

AgentSight will provide a self-contained security audit and progressive
enforcement loop without a runtime dependency on AgentSecCore. AgentSight owns
Agent and session identity, policy coordination, evidence correlation, storage,
and the product surface. A separately supervised `agentsight-enforcer` owns the
privileged ActPlane runtime. ActPlane performs BPF-LSM observation, process-level
taint propagation, temporal policy evaluation, and kernel enforcement.

The first product scenario is credential exfiltration: detect a process tree that
reads a sensitive credential and then connects to an untrusted, globally
routable public IPv4 endpoint.
The same policy progresses through `observe`, `audit`, and `enforce` modes before
automatic denial is enabled.

## Goals

- Close the audit-to-enforcement loop within AgentSight and ActPlane.
- Avoid a runtime or availability dependency on AgentSecCore.
- Prove one explainable sensitive-read-to-network-sink security scenario.
- Use ActPlane taint and temporal semantics for deterministic kernel decisions.
- Preserve evidence that explains what happened, why a rule matched, and whether
  the kernel actually blocked the operation.
- Roll out enforcement gradually, with explicit policy revisions and reversible
  state transitions.
- Keep sensitive file contents, credentials, and full network payloads out of the
  default audit record.

## Non-goals

- Byte-level or cross-host information-flow tracking.
- Semantic risk inference or model-based security decisions.
- AgentSecCore integration in the first delivery.
- Full TLS payload retention, general SIEM functionality, or an automated
  incident-response orchestrator.
- Automatic publication of exceptions derived from user feedback.
- Migrating the complete `oslevel-harness` API or running it as a service.
- Supporting ActPlane `kill` effects in the first delivery.

## Current Capability Gap

AgentSight currently records LLM calls and process actions in its audit store. Its
file probes are specialized for Agent session JSONL files, and its network probes
are specialized for LLM HTTP traffic, DNS-assisted discovery, and configured
plain-HTTP targets. It does not yet expose normalized general-purpose
`file_action` or `network_action` audit events.

This design does not add a second general file and network observation stack to
AgentSight. Security-sensitive file and network facts originate from ActPlane.
AgentSight continues to own its existing process, session, LLM, and discovery
probes and enriches ActPlane events with that context.

## Considered Approaches

### AgentSight coordination with ActPlane kernel decisions

AgentSight controls policy intent and evidence; ActPlane evaluates security facts
and effects in the kernel. This is the selected approach because it keeps the
synchronous decision deterministic while preserving a unified AgentSight product
surface.

### ActPlane-owned policy and audit product

ActPlane could own configuration, policy state, and violation history while
AgentSight only links to its output. This is simpler initially, but creates two
user-facing control planes and duplicates Agent, session, and policy semantics.

### User-space risk correlation before enforcement

AgentSight could observe a file read and later ask ActPlane to block a network
operation. This is flexible but introduces latency and a time-of-check-to-time-of-
use gap. User-space correlation remains useful for case aggregation and policy
recommendations, not for the synchronous decision.

## Architecture

```text
Agent and tool process tree
          |
          | process, session, and LLM observation
          v
AgentSight security coordinator
  identity / desired policy / evidence / audit / API
          |
          | versioned UDS protocol
          v
agentsight-enforcer
  peer authorization / actual policy state / ActPlane adapter
          |
          | pinned official ActPlane API
          v
ActPlane
  file and network facts / taint / since-unless / BPF-LSM decision
          |
          v
Linux kernel: allow / audit / deny
          |
          | normalized security events
          v
AgentSight security store and Dashboard
```

### AgentSight

AgentSight answers who performed an action. It discovers the Agent root process,
maintains the process tree and session association, resolves policy bindings,
persists desired and reported state, correlates evidence into risk cases, and
serves the Dashboard, API, and CLI.

AgentSight does not load ActPlane programs or make synchronous allow/deny
decisions in user space.

### `agentsight-enforcer`

The enforcer owns privileged execution. It validates the UDS peer, verifies PID
identity using PID plus process start time, serializes lifecycle mutations,
contains the sole ActPlane adapter, and returns normalized state and events.

Running the enforcer separately ensures that an AgentSight API failure does not
remove a confirmed kernel policy and that an ActPlane loader failure does not
take down the observability API.

### ActPlane

ActPlane observes security-sensitive file and network operations, associates
labels with protected process trees, propagates labels across `fork`, `clone`, and
`exec`, evaluates temporal conditions, applies `allow`, `audit`, or `deny`, and
emits the facts and decision result required for evidence.

### AgentSecCore

AgentSecCore is not installed, called, or required by this design. A later
optional integration may contribute semantic findings through a versioned event
interface, but it must not become part of the synchronous enforcement path or a
prerequisite for AgentSight security.

## Credential Exfiltration Policy Model

The policy combines a sensitive source, a process-level label, a destination
sink, and a bounded temporal relationship. A sensitive read by itself is not an
automatic denial.

### Sources

The first built-in resource classes are:

- `credential`: `~/.aws/credentials` and equivalent credential stores.
- `private_key`: SSH keys and `*.pem` or `*.key` files.
- `project_secret`: `.env`, `.env.*`, and configured secret files.
- `cloud_credential`: Kubernetes and registry authentication files.

Administrators can add path patterns. Audit output stores a redacted path and
resource class, not file contents.

### Taint state and propagation

Reading a configured source adds a label such as `credential` or `private_key` to
the protected process tree. Each label records its class, redacted origin,
originating process identity, first observation time, and expiry time.

The first release uses process-level taint:

- The current process retains the label.
- Descendants inherit it across `fork`, `clone`, and `exec`.
- Labels do not propagate from a child back to its parent.
- Labels expire after a configured TTL or when the protected process tree exits.
- Labels are scoped to an Agent binding and cannot contaminate an unrelated
  Agent that reuses a PID.

Byte-level proof that particular bytes reached a socket is not required. The
claim is narrower and explainable: a process tree that accessed a sensitive
resource subsequently attempted an external connection.

### Network sinks

Destinations are classified as:

- `trusted`: configured model endpoints, internal services, and approved package
  sources.
- `unknown_public`: a globally routable public IPv4 destination without an
  explicit trust decision.
- `high_risk`: configured anonymous upload services, dynamic endpoints, or
  prohibited ports.
- `local`: loopback and Unix sockets, which do not trigger exfiltration policy by
  default.

Domain identity is preferred when DNS evidence exists. The destination IP and
port remain in evidence so a decision is reproducible after DNS changes.
The initial versioned policy contract requires `public_ipv4`. IPv6 and an
all-family public scope are rejected before attachment because the pinned
runtime emits only IPv4 connect evidence. Special-purpose IPv4 ranges—including
unspecified, private, loopback, link-local, multicast, broadcast,
documentation, benchmarking, shared-address, and reserved ranges—are excluded.

### Temporal relationship and exceptions

At the product level, the rule means:

```text
When an Agent process tree has read a credential-class resource,
and connects to a globally routable public IPv4 destination within five minutes,
unless the destination is in the trusted endpoint set,
produce a credential-exfiltration decision.
```

The AgentSight policy model is stable and versioned. The ActPlane adapter
translates it into the pinned upstream DSL, including its taint and `since` /
`unless` semantics. Dashboard and storage types do not expose upstream AST or
runtime types.

## Progressive Enforcement

Every policy revision has exactly one deployment mode:

| Mode | Kernel behavior | Product behavior |
|------|-----------------|------------------|
| `observe` | Allow | Count matches and resource cost; no formal risk case |
| `audit` | Allow | Create a risk case and evidence chain |
| `enforce` | Deny a matching operation | Create a risk case with the observed kernel result |

The initial defaults are:

- Sensitive read without an external sink: audit only.
- Sensitive read followed by a trusted model endpoint: audit only.
- Sensitive read followed by an unknown public IPv4 destination: audit until the
  rule meets the rollout criteria, then deny.
- New attachment failure: fail open and emit `enforcement_unavailable`.
- A previously confirmed kernel policy continues independently of Dashboard or
  API availability.

Risk scores help operators prioritize cases but do not independently cause a
deny. The synchronous result is determined by explicit policy conditions. A
default deterministic score can combine source severity, confirmed inheritance,
destination class, temporal proximity, and suspicious port or tool evidence.

## Security Event Contract

Events are divided into facts, state transitions, decisions, and enforcement
health. This prevents a policy conclusion from replacing the evidence that led
to it.

### Shared identity

Every security event contains:

- `event_id`, kernel occurrence time, and AgentSight observation time.
- `agent_id`, `agent_name`, and optional session, conversation, and tool-call IDs.
- PID, process start time, optional parent PID, and cgroup ID.
- Binding, policy, and immutable policy revision IDs.
- Source component, enforcer version, ActPlane revision, and protocol version.

PID and process start time are always evaluated together.

### Event kinds

`file_action` records an operation, redacted path, resource class, and observed
result. It does not contain file data.

`taint_transition` records add, inherit, expire, or clear actions, including the
source and target process identities and the transition reason.

`network_action` records connect direction, destination identity, destination
class, protocol, and observed result.

`policy_decision` records the policy and revision, mode, requested effect,
observed `blocked` and `killed` flags, optional errno, rule explanation, and risk
score. Requested effect and observed result remain distinct.

`enforcement_state` records compilation failures, loader failures, disconnects,
reconciliation results, unsupported kernel features, and bounded-buffer loss.

## Risk Cases and Evidence

AgentSight correlates related immutable events into a risk case. A credential
exfiltration case contains the source read, taint additions and propagation,
network sink, policy decision, and kernel outcome.

Risk cases support `open`, `confirmed`, `false_positive`, `accepted_risk`, and
`resolved` states. Marking a case as a false positive never changes a kernel
policy automatically. AgentSight may generate a proposed exception, but a user
must review and publish a new policy revision.

The case view must answer:

1. What happened?
2. Was the operation actually blocked?
3. Which evidence and temporal relationship caused the decision?
4. Which Agent, session, process tree, resource, and destination were affected?

### Notify path and retry deduplication

The credential-exfiltration profile uses complementary kernel paths. BPF-LSM
`file_open` records credential reads and propagates taint, while BPF-LSM
`socket_connect` evaluates deny rules. Observe and audit decisions are emitted
by the notify-only `sys_enter_connect` path so they remain visible without
blocking. The notify path does not emit a second event for enforce mode, and the
profile does not enable the verifier-heavy file-open exit tracepoint.

One short exfiltration burst produces one case even when a client retries
several resolved addresses. The correlation key contains the binding, policy
revision, source resource, source and sink process identities, and a five-second
occurrence bucket; destination addresses are deliberately excluded. Every
immutable event and evidence link remains stored in order. Observe mode stores
facts without opening a case, audit opens an unblocked case, and enforce opens a
blocked critical case. Replayed event IDs remain idempotent, while incomplete
source or sink evidence remains a typed ingestion error.

## Storage and Query Surface

The current untagged audit payload remains focused on LLM and process actions.
Security data uses separate physical tables and a unified query surface:

- `security_events`: immutable normalized security events.
- `risk_cases`: correlated conclusions and review state.
- `risk_evidence_links`: many-to-many case-to-event relationships.
- `policy_bindings`: desired and reported enforcement state.
- `policy_revisions`: immutable policy content and publication metadata.

The first API surface is:

```text
GET  /api/audit/events
GET  /api/audit/cases
GET  /api/audit/cases/{case_id}
GET  /api/security/policies
GET  /api/security/bindings
POST /api/audit/cases/{case_id}/review
```

Existing audit APIs remain compatible. A combined timeline query can join
process, LLM, and security events without migrating existing rows in the first
release.

## Dashboard

The system audit page provides two entry views that converge on one detail page:

- Risk view for security operators, prioritized by severity and review status.
- Session view for developers, organized around Agent sessions and tool calls.

The detail header states the conclusion, observed enforcement result, reason, and
impact. The evidence area displays a chronological source-read, taint,
propagation, network-sink, and decision chain alongside the process tree. Policy
details explain the matched source, sink, `since`, and `unless` conditions.

The UI must distinguish all of the following:

- Rule matched and the operation was allowed in audit mode.
- Denial was requested but enforcement failed.
- The kernel confirmed that the operation was blocked.
- The enforcer was unavailable and no enforcement decision was possible.

Review actions are confirm risk, mark false positive, accept risk, resolve, and
create an exception proposal. Publishing a policy change is a separate explicit
operation.

## Policy Lifecycle

AgentSight persists desired state and the last reported actual state:

```text
draft -> validating -> pending -> enforced
                       |             |
                       v             v
                     failed       degraded
                                     |
                                     v
                                reconciling

enforced -> detaching -> detached
```

Only `enforced` means that ActPlane confirmed the policy attachment. A revision
update loads and validates the new revision before switching the binding so that
a failed update preserves the old policy.

After reconnect, AgentSight lists actual bindings and reconciles them against
desired state. It validates process identity before reapplying a missing binding.
Unknown bindings are reported and handled according to recorded ownership; they
are never silently adopted.

## Failure Semantics

| Failure | Required behavior |
|---------|-------------------|
| Dashboard or API exits | Confirmed kernel policy continues |
| Enforcer restarts | Reconcile actual and desired state before reporting healthy |
| New policy compilation fails | Preserve the old revision; mark the new revision failed |
| Coordinator loses the UDS connection | Mark bindings degraded and emit a high-priority event |
| PID exited or was reused | Reject the binding using PID plus start-time validation |
| Audit storage is unavailable | Continue kernel decisions and use a bounded replay buffer |
| Required kernel feature is unavailable | Fail the binding open and report an actionable error |
| Event buffer overflows | Record loss counters and expose degraded evidence quality |

Errors, DSL fragments, and event frames are size-bounded. UDS peer credentials
and filesystem permissions protect privileged operations. Lifecycle requests are
idempotent and include protocol version, request ID, and binding ID.

## Data Minimization

Default evidence includes redacted resource paths, resource class, operation and
result, destination domain or IP and port, process and session identity, and
policy metadata. File size and an asynchronous hash may be enabled separately.

Default evidence excludes file contents, TLS plaintext, complete request
payloads, API keys, cookies, authorization headers, and process memory buffers.
An explicitly authorized incident may enable bounded, expiring enhanced evidence
collection for that case; it is not part of the first delivery.

## First Delivery

The first delivery implements one policy and one complete flow:

```yaml
id: credential-exfiltration
sources:
  - ~/.ssh/id_rsa
  - ~/.ssh/id_ed25519
  - ~/.aws/credentials
  - "**/.env"
taint: credential
propagation:
  process_tree: true
  ttl: 5m
sinks:
  network: public_ipv4
unless:
  destination_in: trusted_endpoints
mode: audit
```

This YAML is the product-level policy model, not a promise of exact ActPlane DSL
syntax. The adapter owns the version-specific translation.

Delivery proceeds through observe, audit, and enforce deployments. `allow`,
`audit`, and `deny` are supported effects; `kill` is deferred.

## Acceptance Criteria

The feature is accepted only after a real Linux test proves all of the following:

1. Ordinary file reads and trusted model calls continue normally.
2. Reading a test credential followed by a trusted destination produces audit
   evidence without denial.
3. Reading the test credential followed by an unknown public IPv4 destination creates
   a complete risk case in audit mode while allowing the operation.
4. Unsupported all-family scope and enforce mode fail before kernel attachment.
5. Dashboard evidence shows source, propagation, sink, policy revision, and the
   observed kernel result.
6. A child process inherits the label; an unrelated Agent process does not.
7. Label expiry restores normal network behavior after the configured TTL.
8. Enforcer restart reconciliation never reports unknown state as enforced.
9. Existing AgentSight process, session, and LLM observation remains functional.
10. Resource use and event loss remain within the initial operating budget.

The initial budget is less than 30 MiB additional idle resident memory, less than
1% additional idle CPU, and less than 2 ms P99 policy-decision overhead. The
numbers are release gates for the first supported platform and must be recalibrated
with recorded measurements before expanding kernel support.

## Delivery Sequence

1. Extend the versioned enforcer protocol with normalized security event types.
2. Add deterministic mock-backend policy, taint, and evidence tests.
3. Add the ActPlane adapter translation for the credential-exfiltration policy.
4. Persist policy revisions, bindings, security events, and risk cases.
5. Add audit and policy API endpoints and the unified detail view.
6. Run observe and audit soak tests and classify false positives.
7. Extend the pinned runtime before enabling deny or IPv6 collection; neither
   capability is implicit in the `public_ipv4` audit contract.
8. Run denial, expiry, restart, degradation, and regression acceptance tests on a
   real Linux host.

Each step must preserve existing AgentSight observability and must not introduce
an `oslevel-harness` or AgentSecCore runtime dependency.
