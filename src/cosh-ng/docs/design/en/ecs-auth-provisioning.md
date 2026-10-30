# cosh-ng ECS Auth Provisioning Design

Date: 2026-09-17

Related documents: [runtime contracts](runtime-contracts.md)

## Summary

First-time provider configuration and ECS RAM Role authentication run over the
`cosh-core` registry request/response wire, driven by a state machine in
`cosh-shell`. `cosh-shell` reads one JSON request/response per registry call.
For the cancellable read-only operations (`prepare` / `verify`) it spawns a
dedicated short-lived `cosh-core --registry` process it exclusively owns; the
`configure` save prefers the shared live core and only falls back to a
short-lived `cosh-core --registry` process when no live runtime exists. This
document fixes the parts of that surface that are a cross-component contract: the
`auth` domain `prepare` / `verify` / `configure` wire shapes, the shell-side
polling lifecycle and its time budgets, and the signal-safety invariant the
isolated probe relies on. shell and core must be upgraded together, because the
`verify` response shape changed in a non-backward-compatible way.

## Registry `auth` wire contract

The transport is JSONL: each request and response is exactly one JSON object
terminated by a newline. Every action uses the same correlation envelope:

```json
{
  "type": "registry_request",
  "request_id": "reg-42",
  "domain": "auth",
  "action": "prepare",
  "params": { "provider_type": "aliyun" }
}
```

```json
{
  "type": "registry_response",
  "request_id": "reg-42",
  "success": true,
  "data": { "mode": "manual" }
}
```

Every request sets `type: "registry_request"` with a `request_id`, and cosh-core
echoes that same `request_id` on the matching `registry_response`; input lines
that are not a `registry_request` are ignored, so a malformed request fails
closed as a shell-side timeout. How strictly the shell enforces the response
envelope depends on the path: the live core transport and the isolated ECS probe
reject a response whose `type` or `request_id` does not match the pending request
(`cosh_core_service/process.rs`, `adapter/ecs_probe.rs`), while the short-lived
fallback used by `configure` when no live runtime exists parses the first
non-empty stdout line on `success` / `data` / `error` alone and does not re-check
the correlation fields (`adapter/cosh_core_registry.rs`); its dedicated child
carries exactly one request, so there is no second response to confuse it.

`params` is action-specific; the core deserializes it with `#[serde(default)]`,
so an omitted or `null` value is accepted and read the same way as an empty
object. A failed response sets `success: false`, may still carry structured
`data` (for example `error_code`), and carries the developer-facing `error`
string. Optional `data` / `error` fields are omitted when absent.

### `prepare` — decide the entry mode

Input: `{ "provider_type": "<type>" }`. Only `aliyun` triggers ECS detection;
every other type returns manual mode.

Output `data`:

- Manual providers, or aliyun without an ECS challenge:
  `{ "mode": "manual" }`.
- aliyun on an ECS instance with a RAM Role challenge:
  `{ "mode": "ecs_ram_role", "instance_id": "...", "console_url": "...",
  "values": { "auth_source": "ecs_ram_role" } }`.
- Detection distinguishes "not an ECS instance" from "ECS but probe failed" by
  where the metadata request fails. If the IMDSv2 token fetch is `Unreachable`
  or times out, detection returns `{ "mode": "manual" }` — that is the expected
  fallback in non-ECS environments, not a protocol violation. Only after a valid
  token has established ECS identity does a subsequent instance-id/zone GET
  failure propagate as the `verify` error shape below (`success: false`,
  `error_code`); such a confirmed-ECS failure is never downgraded to manual.

`prepare` is a read-only detection. It never verifies credentials or saves
configuration.

### `verify` — classify credential readiness

Input: `{ "provider_type": "<type>", "auth_source": "<source>" }`. Only
`provider_type == "aliyun"` with `auth_source == "ecs_ram_role"` performs an
IMDSv2 metadata probe; other combinations return `{ "authorized": true }`
unchanged.

For the ECS path the response replaces the former `authorized: bool` with a
three-way classification:

| Result | `success` | `data` | `error` |
|--------|-----------|--------|---------|
| Complete unexpired credentials | `true` | `{ "status": "ready" }` | absent |
| Reachable but not usable yet | `true` | `{ "status": "not_ready", "reason": "<reason>" }` | absent |
| Probe failed | `false` | `{ "error_code": "<code>" }` | developer message |

`reason` ∈ `role_missing`, `credentials_expired`. `error_code` ∈
`metadata_access_denied`, `invalid_metadata_response`, `metadata_unreachable`,
`metadata_timeout`, `metadata_http_error`. The probe verifies only that the
named role currently exposes complete, unexpired credentials; it does not claim
to validate SysOM service permissions, quota, or the inference path, and never
returns credential material or upstream response bodies.

### `configure` — persist one provider

Input: `{ "provider_id": "...", "provider_type": "...", "values": { ... } }`.
Rejects an empty id/type (`missing provider_id or provider_type`) and a
non-editable provider (`provider is not editable`). On success returns
`{ "provider_id": "..." }`; on preflight failure returns `success: false` with
`{ "error_code": "<code>" }`.

## Shell polling lifecycle

`cosh-shell` owns an `EcsFlow` state machine (`auth/ecs_poll.rs`) whose stages
are `Preparing`, `Checking`, `Waiting`, `Submitting`, `Cancelling`, `TimedOut`,
`Failed`, `Unknown`, `Editing`. Its `Operation` is one of two kinds with
different lifecycles:

- `Operation::Probe` (`prepare` / `verify`) is an exclusive, cancellable
  `cosh-core --registry` probe owned by `EcsProbeTask` (`adapter/ecs_probe.rs`);
  the shared live core is never borrowed to carry a cancellable probe.
- `Operation::Configure` (the save) is **not** a cancellable probe. It runs a
  `cosh-auth-ecs-save` worker calling `core_auth_configure` →
  `registry_query_classified`, which prefers the live core and only spawns a
  short-lived `cosh-core --registry` process when no live runtime exists.
  `cancel()` returns early while an `Operation::Configure` is in flight or the
  stage is `Submitting`: a save in progress is not interrupted.

Normal flow: after confirming an ECS challenge the shell checks credentials
first; only when they are not ready does it show a cancellable waiting panel and
poll. A single `ready` result auto-submits once (`configure`); it does not ask
for a name or an "I have authorized" confirmation. `not_ready` schedules the
next check about one interval after the current operation is fully reaped.

### Time budgets

| Budget | Value | Scope |
|--------|-------|-------|
| Poll interval | 2 s (`INTERVAL`) | Gap between checks after a `not_ready`, measured after reap |
| Operation budget | 5 s (`OPERATION_LIMIT`) | One prepare/verify probe: spawn, write, read, reap |
| Authorization wait | 200 s (`WAIT_LIMIT`) | Total budget from the first role check; expiry ⇒ `TimedOut` |
| Configure observation | 12 s | One save operation; on expiry the result is unknown |
| Cancellation reap | 5 s | From a cancel request; not a reset of an expired operation budget |

The main event pump independently decides operation failure on budget expiry and
starts cancellation reap; it is never blocked on a worker `join`. A `metadata`
request (token + GET) has its own 3 s total deadline inside the probe.

### Cancellation completion (zero residue)

Cancellation has two phases that must not be conflated: *request cancel* and
*cancel complete*. On ESC/Ctrl+C the flow stops further scheduling, revokes
auto-submit eligibility, and signals cancellation; a same-batch cancel takes
priority over a `ready`. "Cancel complete" requires all of: the round's worker
and reader threads joined; the round's exclusive child reaped and its pipes
closed; no queued operation or future probe; and no result of the round can
still trigger a submit. Only then does the shell show `Cancelled` and restore
the prompt. Detach, dropping the `JoinHandle`/`Receiver`, or spawning another
background reclaimer are not acceptable cancellation implementations. If reap
overruns or fails, the shell shows a resource-reclaim failure and keeps
ownership rather than reporting success.

### Unknown save outcome

If the save worker does not report a confirmed result within the 12 s
observation budget, the flow enters `Unknown`: it does not auto-resend the save,
and only after the operation's resources are reclaimed does it offer "Return to
provider management", which re-reads configuration. `ActiveRun` never claims a
save landed on disk that it could not confirm.

## Probe signal-safety invariant

The isolated probe child runs in its own process group (`process_group(0)`) and
is reclaimed by `ProbeChild` with `try_wait()` followed, only while the child is
still unreaped, by `kill(-pid, SIGKILL)` on the group.

Signalling a recorded numeric PID/PGID during cleanup is sound only while this
process is the **sole reaper** of the probe child: once the child is reaped the
number is freed and can name an unrelated group. That invariant rests on two
production constraints:

1. cosh-shell installs no `SIGCHLD` reaping handler and performs no wildcard
   `waitpid(-1)` — every wait targets a specific `Child`.
2. A caught `SIGCHLD` handler cannot be inherited across `execve`, so the
   inherited disposition is only ever `SIG_IGN` or `SIG_DFL`.

The only competing reaper is therefore the kernel under an inherited
`SIGCHLD=SIG_IGN`, which auto-reaps with no zombie. `EcsProbeTask` normalizes
that one case back to `SIG_DFL` before spawning any child, so terminated probe
children stay waitable until `ProbeChild` reaps them. A non-ignore disposition
is left untouched so an in-process handler (for example a test's `wait-timeout`)
is not clobbered. Introducing any reaping `SIGCHLD` handler or wildcard wait in
production would break the sole-reaper invariant and require a pidfd-based path
instead (`pidfd_send_signal` process-group signalling requires Linux 6.9+ and a
pidfd does not by itself prevent PID-number reuse).

The normalization itself reads the disposition and then writes `SIG_DFL`, so the
two calls are not atomic. That window cannot affect a probe child: the
normalization runs synchronously on the caller thread of `start_ecs_probe` and
returns before the worker thread is spawned, so no probe child can exist while it
is in progress. Whether a child is auto-reaped depends on the parent's
disposition when that child exits, not on a value captured at spawn time, so a
concurrently spawned unrelated child is only ever reclaimed under the host's own
pre-existing semantics. Locking the normalization would not strengthen this
argument, because unrelated `Command::spawn` callers do not take that lock.
