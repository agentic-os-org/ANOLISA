# AgentSight integration reference

[中文版](../../zh/aw/agentsight.md)

This reference is for integration and maintenance. For installation and daily use,
see the [user guide](../../../user-guide/en/aw/observability.md).

Configure an optional AgentSight source once, then use `aw run` to associate native
callbacks and collect bounded evidence automatically. No action Provider or eBPF
is required. AW keeps its local audit when the optional source is unavailable.
Semantic evaluation is not enabled.

Automatic observation and source identity in this slice cover Qoder, OpenClaw,
QwenPaw and Hermes. Pi keeps its native policy support; observation-only callbacks
and trusted source association for Pi require a separate Adapter contract.

## Prerequisites and configuration

Install AW using the [Preview installation guide](../../../user-guide/en/aw/installation.md). Client, callbacks
and daemon must come from the same build supporting `aw-service/v1alpha4`. Before
upgrading, exit owned sessions and stop old daemons with the old binary; keep their
Journal history. Provider stdio remains `aw-provider/v1alpha1`.

Use an independently managed AgentSight query service with authentication enabled.
Both transports validate the common auth-status response and ATIF-v1.7; neither
calls `/health` or gates on product version. Source product version is unknown (null). A running query
service does not prove its collector or eBPF probes are ready. See
[AgentSight deployment](../../../user-guide/en/agentsight/deployment.md).

Add the following block under the existing `spec.agents.<target>` entry. The
source configuration belongs to that target; callback requests cannot replace it.
All fields below except `mode` are required. Omitted mode means `auto`. HTTPS
also requires `token_file`; loopback HTTP does not.

```yaml
evidence:
  mode: auto
  source_id: local-sight
  endpoint: http://127.0.0.1:7396
  query_timeout_ms: 1000
  query_bytes: 1048576
  max_items: 32
  max_bytes: 262144
```

Remote origins require HTTPS with normal certificate/hostname verification. HTTP
accepts only literal loopback addresses. Use an origin without `/api`, userinfo,
query or fragment. Loopback HTTP neither reads a token file nor sends Authorization,
even if `token_file` is supplied. Its mode is `loopback_unauthenticated`.

For HTTPS, add `token_file: /absolute/private/agentsight-token`. It must name a
regular file owned by the current user without group/other access. Symlinks,
special files and tokens larger than 4096 bytes are rejected. Never place token
contents in arguments. `bearer_supplied` records that the header was sent; neither
mode proves source process identity or a source-issued read-only role.

| Setting | Bound |
| --- | --- |
| `mode` | `auto` or `off`; `required` is rejected until capability admission is implemented |
| `source_id` | 1..128 letters/digits/dots/underscores/hyphens; a label, not process identity |
| `query_timeout_ms` | 1..60000 ms; automatic startup/observation reads additionally cap at 1000 ms |
| `query_bytes` | 1..4194304 aggregate response bytes, including compatibility probes |
| `max_items` | 1..128 projected records, including the native candidate |
| `max_bytes` | 4096..2097152 bytes for the complete projected context |

## Run and inspect

Start the configured target normally, then inspect the same service from another
terminal. This example uses an existing Qoder target; other frameworks keep their
normal [native launch arguments](../../../user-guide/en/aw/index.md).

```bash
aw run --config ./aw.yaml --agent qoder
aw status --config ./aw.yaml
```

Startup reports an observation audit key and source query state. Status exposes
active observations, native association, source results and explicit gaps. Query
`ready` means the auth-status contract responded; storage, collection, eBPF and complete coverage remain
`unknown`. Native session IDs become associated when corroborated callbacks arrive.
A run with no callbacks can exit successfully with no associated session.

AW registers pure observation callbacks when an event has no action steps. Where
actions already exist, their callbacks are reused. Pure observation returns the
native no-op response. Background source reads do not delay Hook acknowledgement;
required local metadata writes still complete before acknowledgement.

Each instance has one worker, two queued candidates and one retained bounded
projection. At most 256 callbacks are observed per instance; overflow is visible
in `gaps` and existing actions continue. Queries also retain the original callback
cutoff. Duplicate callbacks do not schedule additional reads. History is matched
by exact native session and the [explicit framework-name contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/runtime/evidence-context.md#identity-association), without inferring process identity.
A concurrent conflicting instance gets `source_session_conflict`; after the first
instance drains, a resumed run may associate that session as historical evidence.

Normal exit, handled signals and launch failure release only owned work and
registrations. Up to one running and two queued reads finish within their bounds;
the launcher has a five-second release deadline. Source state and the audit key
are reported at exit. The shared AW daemon and external AgentSight continue.
An abruptly killed launcher can leave an incomplete instance; automatic orphan
recovery is deferred. Native Stop/continue within a live process does not end the
AW instance. Coverage is limited to supported tool callbacks, not every model turn.

## Missing source or eBPF

`auto` keeps AW execution and local audit usable when credentials, source service,
storage or compatible data are missing. Reasons remain visible. `off` makes no
source requests and registers no additional observation callbacks; configured
actions retain their behavior. Remove the block or set `off`, then restart the
owned daemon with matching configuration to disable observation.

User-space trajectories can be available without eBPF when the independent
collector supports the Agent. Query success cannot prove eBPF readiness. AW does
not install packages, elevate privileges or start/stop AgentSight workers.
Full capability requirements and loss/recovery policy follow in #7623; public
incremental audit and consumption follow in #7624 and #7625.

The default user-space trajectory collector waits 30 seconds between scans. GET
reads stored history; querying again immediately does not guarantee fresh data.
AW reports unverified source time, keeps Hook responses independent of these reads,
and never rewrites completed decisions with late evidence.

## Freeze evidence for an active AW event

Embedding clients can still freeze the original candidate, constraints and
selected evidence of an active action event. This advanced operation is not
required for normal `aw run`. Copy the original handle into `operation.json`:

```json
{
  "method": "freeze_context",
  "event_id": "<active event ID>",
  "instance_id": "<owning instance ID>",
  "candidate_revision": "<candidate revision from the event handle>"
}
```

```bash
aw request --socket /absolute/private/state/aw.sock --timeout-ms 5000 < operation.json
```

Plain `open_event` identity is unverified and cannot select source history.
A successful repeated request reuses the same immutable snapshot. Closure,
expiry, concurrent requests and stale revisions remain explicit failures; no new
budget is created. Native completion cannot replace the original candidate.
Automatic observation has its own bounded lifecycle and cannot reopen an event.

The result uses `aw.evidence-context/v1` and `aw.evidence/v1`, with content digests,
subject/revision checks and gaps. Model echoes do not add executions; source
process identity and unproven coverage stay unknown. The private same-user socket
does not isolate hostile same-UID programs. Returned content is not automatically
redacted. Journal keeps metadata only; cached content is released with its owning
event or observation instance. Caller-saved responses remain caller-managed.
Without a source block, explicit freezing returns AW-only material with
`source_not_configured` and default limits of 32 items / 262144 bytes.
