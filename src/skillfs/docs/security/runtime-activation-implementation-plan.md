# SkillFS Runtime Activation Implementation Plan

Status: core daemon-activation path implemented; trusted-writer tightening and provider lifecycle consistency remain planned

This document records the production-oriented path from the current
`--decision-command scan -> resolve` integration toward a daemon-driven
runtime activation contract. It is a SkillFS implementation plan. The
external security provider owns scanning, policy evaluation, versioning,
and activation decisions; SkillFS owns filesystem observation, safe target
validation, event delivery, and view exposure.

## Current State

SkillFS currently supports two security integration paths.

The compatibility path keeps the external decision command in the SkillFS
process:

```text
FUSE mutation
  -> per-skill debounce
  -> <decision-command> scan <skill_dir> --json
  -> <decision-command> resolve <skill_dir> --json
  -> update in-memory ActiveSkillResolver
  -> expose /skills/<name> as current, fallback, or hidden
```

The production-oriented path delegates security work to an external daemon and
has SkillFS consume only the resulting activation state:

```text
FUSE mutation
  -> per-skill debounce
  -> append protocol event log
  -> notify external daemon over Unix socket

External daemon
  -> debounce/reconcile/check/scan/policy
  -> write .skill-meta/activation.json and optional activation xattr

SkillFS
  -> consume activation state
  -> update ActiveSkillResolver
  -> expose /skills/<name> as snapshot or hidden
```

The decision-command path remains useful for CLI-based integration and demo
validation. The activation path is the preferred shape for daemon integration
because activation state is durable outside the SkillFS process.

## Design Principles

- Directory name remains the canonical SkillFS identity. `SKILL.md name:` is
  metadata only and must not create an alias.
- SkillFS must fail safe. Invalid, missing, inconsistent, or unsafe activation
  state hides the skill instead of exposing live source.
- SkillFS must not parse policy, findings, scan status, or ledger internals.
  It only consumes the runtime activation target.
- Read paths should continue using the in-memory `ActiveSkillResolver`; do not
  read activation files or xattrs on every FUSE read.
- Mutating writes still land in the source/current workspace. Snapshots are
  served read-only through active mapping and fd pinning.
- `.skill-meta/**` changes must not create notification loops.
- The existing `--decision-command` path remains available until daemon mode is
  fully validated and explicitly deprecated.

## Runtime Activation Contract

The primary file contract is:

```text
<skill_dir>/.skill-meta/activation.json
```

The JSON payload is intentionally small:

```json
{
  "schemaVersion": 1,
  "target": ".skill-meta/versions/v000001.snapshot"
}
```

No active runtime version is represented as:

```json
{
  "schemaVersion": 1,
  "target": null
}
```

SkillFS validation rules:

- `schemaVersion` must be exactly `1`.
- `target = null` maps to `ActiveTarget::Hidden`.
- Non-null `target` must be a relative path under
  `.skill-meta/versions/<version>.snapshot`.
- Reject absolute paths, empty strings, `.` / `..` traversal, non-snapshot
  targets, foreign roots, malformed JSON, unsupported schema versions, and
  unknown unsafe shapes.
- The resolved snapshot directory must exist and must stay within the owning
  `skill_dir`.
- Invalid activation maps to hidden and produces a diagnostic error; it must
  not panic or expose live source.

A2 implements the xattr activation contract. The xattr name is:

```text
user.agent_sec.skill_ledger.activation
```

The xattr is preferred when present; `activation.json` is the fallback when
the xattr is absent or the filesystem does not support user xattrs. If both
exist and disagree, SkillFS fails safe and hides the skill. If `lgetxattr`
returns an unexpected error (e.g. `EACCES`, `EIO`), SkillFS fails safe and
does not fall back to `activation.json`.

## Implementation Packages

### A1: Activation File Consumer

Goal: consume `.skill-meta/activation.json` and initialize or refresh
`ActiveSkillResolver` without invoking the external decision command.

Scope:

- Add `security::activation` with strict `ActivationRecord` parsing.
- Add helpers to validate target paths and convert activation into
  `ActiveTarget::Snapshot` or `ActiveTarget::Hidden`.
- Add startup loading behind an explicit opt-in CLI/config setting.
- Preserve existing `--decision-command scan -> resolve` behavior when the new
  setting is absent.
- Add unit and integration tests for valid, hidden, invalid, missing, and
  unsafe activation states.

Out of scope for A1:

- xattr activation consumption.
- daemon notify.
- event log schema changes.
- reconcile loop.
- policy computation or parsing ledger internals.

### A2: Activation Xattr Fallback

Status: **implemented**

Priority: immediate follow-on to A1. Required before N2/E1/R1.

Goal: consume `user.agent_sec.skill_ledger.activation` with
`activation.json` fallback.

Scope:

- Read the user xattr without following symlinks via direct `lgetxattr`
  libc call against the physical source directory (not the FUSE xattr
  callback path — no T3 interaction or loop risk).
- Parse the same `ActivationRecord` payload.
- Prefer xattr when present and valid.
- Fall back to `activation.json` only when xattr is absent (`ENODATA`)
  or the filesystem does not support user xattrs (`ENOTSUP`/`EOPNOTSUPP`).
- If xattr exists but is invalid (bad JSON, unsupported schema, bad
  target), fail-safe hidden with **no fallback** to `activation.json`.
- If both xattr and `activation.json` exist and are valid but their
  `target` fields disagree, fail-safe hidden.
- `bootstrap_activation` uses the prefer-xattr path by default.

Out of scope for A2:

- Daemon notify (`skill_ledger.skillfs_notify_change`).
- Protocol event log.
- Reconcile loop.
- CLI/config changes (no new flags or modes).
- FUSE read-path changes (still uses in-memory `ActiveSkillResolver`
  populated at startup; no per-read xattr calls).

### A3: Notify Change, Protocol Event Log, And Runtime Reload

Status: **implemented**

Goal: notify the external daemon that a skill source workspace may have
changed, record protocol-visible events, and refresh the active resolver after
the daemon writes activation state.

Scope:

- Add a Unix socket client for `skill_ledger.skillfs_notify_change`.
- Send one NDJSON request frame per debounced skill change.
- Socket notify v2 includes `schemaVersion=2`, `canonicalSkillDir`, the full
  `skillId`, `eventKind`, and relative `paths`.
- Derive `canonicalSkillDir` from the canonical source root. When the daemon
  needs the live source, it resolves that canonical path through the S1
  resolver instead of receiving a backing-root path in the notify payload.
- Treat successful send as event acceptance, not as security approval.
- On failure, write diagnostics and keep serving the existing trusted mapping.
- Add a separate JSONL writer for the N3 activation protocol event schema.
- Protocol event log v1 fields remain `schemaVersion`, `time`, `skillDir`,
  `skillName`, `eventKind`, `paths`, and optional `reloadOutcome`.
- In in-place/backing-root mode, protocol event `skillDir` uses the
  daemon/live backing root so existing event-log consumers can read the live
  source without traversing the FUSE over-mount.
- Keep it separate from the existing audit stream and security event stream.
- Do not rely on this log as the only source of truth; daemon reconcile must
  re-read current disk state.
- After successful notification, reload activation on an explicit trigger or
  bounded delay.
- On SkillFS startup, load activation for every managed skill.
- Provide an explicit refresh API for future daemon ack integration.
- Preserve fd-pinned read consistency for already opened handles.

### A4: Startup Reconcile And Reload Observability

Status: **implemented**

Goal: close the daemon-restart and missed-change gap with startup reconcile
notifications, and make activation reload outcomes visible in the protocol
event log.

Scope:

- Queue one startup reconcile notification per known skill after the mount is
  ready, and let the shared notify worker own delivery.
- Do not block mount startup on the daemon: enqueueing touches no socket, so a
  slow or unavailable daemon cannot delay startup.
- Retry a reconcile whose delivery failed transiently, with exponential
  backoff, jitter, and a capped maximum interval, until the daemon
  acknowledges it. See "Reconcile delivery durability" below.
- Add `reloadOutcome` to protocol events for activation reload results:
  `activation_updated`, `activation_unchanged`, `activation_timeout`, and
  `activation_invalid_hidden`.
- Provide explicit runtime reload helpers for one skill or known skills.
- Preserve fd-pinned read consistency: old file handles keep their open-time
  target; new opens observe the updated active mapping.

#### Reconcile Delivery Durability

The first A4 implementation sent each reconcile exactly once from a detached
thread and only logged a failed send. That lost the notification whenever
SkillFS won the startup race against the daemon, and nothing recovered it: the
A5 activation watcher consumes activation state the daemon has *already*
written, so it cannot make the daemon scan a skill it has never seen. A skill
absent from the daemon's `managedSkillDirs` would therefore stay fail-safe
hidden until an unrelated filesystem event or a manual reconcile arrived.

Reconcile delivery is therefore durable:

- Each skill enters the notify worker's `pending` map as an immediately due
  `eventKind="reconcile"` entry with empty `paths`, deduplicated by canonical
  Skill identity.
- A transient failure requeues the entry with exponential backoff, jitter, and
  a capped maximum interval. Retries continue at the capped rate for as long as
  the failure persists, so convergence does not depend on an attempt limit.
- An ambiguous authentication failure retries under one endpoint/controller
  budget shared by every skill. Transient transport failures do not consume
  this budget. See "Ambiguous handshake outcomes" below.
- A permanent failure is logged at `error` and dropped. Retrying an untrusted
  endpoint, a rejected authentication proof, or a daemon rejection can only
  reproduce the same result, so it needs an operator, not another attempt.
- A reconcile failure that proves the business request was never sent skips
  activation reload polling. Post-delivery failures retain the poll, while the
  activation watcher remains the fallback for late repair in either case.
- Shutdown forbids requeueing, and also abandons the remainder of an
  already-drained batch rather than walking it entry by entry — each entry can
  cost a full socket timeout plus an activation reload poll. The retry loop
  lives in the worker rather than in a thread holding an
  `Arc<NotifyController>`, so `Drop` still triggers shutdown even while a skill
  is unconverged.

Failures are classified by error variant, never by rendered message:

| Condition | Class |
| --- | --- |
| Parent directory or socket not created yet (`NotFound`) | retry |
| `ConnectionRefused`, `ConnectionReset`, `ConnectionAborted`, `BrokenPipe`, `NotConnected`, `UnexpectedEof` | retry |
| Read/write timeout, `WouldBlock`, `Interrupted` | retry |
| Authentication handshake I/O error, handshake timeout | retry |
| Peer closed the connection or sent an unparseable frame during authentication | endpoint-scoped bounded retry |
| Endpoint owner, mode, or file type unsafe (including `PermissionDenied` on stat) | permanent |
| Authentication proof verification reported failed, unusable key material | permanent |
| Complete malformed notify acknowledgement, notify schema mismatch, oversized authentication frame, daemon rejection | permanent |

##### Ambiguous Handshake Outcomes

One condition cannot be classified from the wire. sec-core refuses a bad
client proof by **closing the connection**, so the SkillFS client observes EOF
while waiting for `auth.ok`; `read_frame` maps that `Ok(0)` to
`AuthError::InvalidFrame`. A daemon restarting at any handshake phase produces
the same error variant. The client therefore never sees `VerificationFailed`
for a wrong notify authentication key — that variant only covers proofs the
peer explicitly answered.

Neither pure classification is acceptable: treating it as transient retries a
permanently wrong key forever, and treating it as permanent silently drops a
reconcile lost to a genuine restart — the exact failure mode this section
exists to remove. It is therefore its own class, retried with the normal
backoff under an endpoint/controller-level budget of eight inconclusive
handshakes, a nominal 31.75-second window before jitter. The budget is shared
by every startup reconcile that uses the same notify client, rather than being
multiplied by the number of skills. Another immediately due skill cannot bypass
the endpoint backoff and consume the budget in a tight loop.

Only an actual inconclusive authentication handshake consumes the ambiguous
budget. Socket lifecycle and other transient transport failures retain their
normal retry state but neither consume nor reset it. An acknowledged delivery
proves that authentication is working and resets the budget. If the budget is
exhausted, the worker emits one endpoint-level `error` naming the notify key as
the thing to check and abandons the current and remaining queued startup
reconciles without making one more authentication attempt per skill. This caps
the total wrong-key handshake cost for the controller, independent of skill
count, while still giving a daemon restart a bounded convergence window.
The exhausted gate applies to that reconcile generation rather than disabling
the controller forever: a later explicit startup-reconcile enqueue starts a
new generation and clears the gate and its ambiguous count. Any acknowledged
delivery also clears the gate and count; after exhaustion it advances the
generation so entries already drained from the abandoned cycle remain stale.

Retry is scoped to reconcile. Ordinary FUSE mutations keep their best-effort,
single-attempt semantics, because the daemon re-derives the same state from the
next reconcile or the next mutation in the same skill, and requeueing them
would change the latency behaviour of the hot write path.

Dedup handles the in-flight race. If a mutation for the same skill arrives
while its reconcile is being delivered, the requeue merges rather than
overwrites: the merged entry stays a reconcile with empty `paths`, because a
reconcile is a full rescan of current on-disk state and already covers any path
list.

Ordinary mutation scheduling is untouched by all of this. A mutation landing on
a queued reconcile leaves the entry completely alone — it neither downgrades
`eventKind` nor moves `fireAt`, because the reconcile already covers it.
Mutation-on-mutation keeps its original trailing-edge debounce, where each
mutation pushes the deadline out so a continuous write burst dispatches once
after it goes quiet rather than mid-write.

Delivery observability replaces the previous single emitted-event count:

| Metric | Type | Meaning |
| --- | --- | --- |
| `attempted` | counter | Delivery attempts, counting every retry separately. |
| `succeeded` | counter | Attempts the daemon acknowledged. |
| `failed` | counter | Attempts that failed, counting every retry separately. |
| `pending` | gauge | Skills currently queued for a delivery attempt. |

`pending` is not derivable from the counters. A skill whose reconcile fails
transiently increments `failed` **and** goes back into `pending`, because it is
queued for another attempt.

`pending` counts the queue, not in-flight work: the worker drains an entry
before dispatching it and only requeues it if the attempt failed retryably, so
`pending` reads 0 during the send window even when the skill has not converged.
Alert on it being non-zero across a window rather than on a single sample.

The counters describe actual delivery attempts. An inconclusive handshake
increments both `attempted` and `failed` once, regardless of which skill was
selected for that endpoint attempt. Reconciles abandoned after the shared
authentication budget is exhausted are not additional attempts and therefore
do not increment either counter; removing them from the queue updates
`pending`. The endpoint-level terminal `error` is the signal that those queued
skills were abandoned.

The daemon wire protocol is unchanged, and no Kubernetes startup ordering
constraint is introduced: SkillFS may start before or after the daemon.

### A5: Activation State Watcher And Continuous Convergence

Status: **implemented**

Goal: make SkillFS continuously converge its in-memory `ActiveSkillResolver`
to the daemon-owned activation state, even when the activation update was not
produced by the current mount's notify/poll cycle.

Problem statement:

The activation path currently uses startup bootstrap plus notify-triggered,
bounded reload polling. That is sufficient for the happy path, but it is not a
continuous subscription to the activation authority. SkillFS can keep serving a
stale hidden/current/fallback view when:

- SkillFS mounts before the daemon writes activation; daemon reconcile writes
  activation later, but the current mount never reloads it.
- A normal notify is delivered, but scan/resolve takes longer than
  `poll_reload_skill()` timeout; the later activation write is missed.
- Daemon startup reconcile, config change, manual operator action, or another
  control-plane flow updates activation without being triggered by this mount.
- Notify socket delivery fails, then the daemon later repairs state through
  its own reconcile path.
- Startup reconcile only sends notify; if no reload is attached to that
  reconcile, the activation written by the daemon is not reflected in memory.
- A source mutation notification is missed or filtered, but the daemon still
  writes a new activation through another path.

Scope:

- Add an activation-state observer for every managed skill. It watches the
  activation authority, not arbitrary source content.
- Observe `<skill>/.skill-meta/activation.json` mtime and the owning skill
  directory ctime, reusing the composite freshness model already used for
  xattr-aware reload.
- On freshness advance, call `load_activation_prefer_xattr()` and update the
  `ActiveSkillResolver` with the same fail-safe hidden semantics as A1/A2/A3.
- Treat notify-triggered `poll_reload_skill()` as the fast path, not the only
  path. A5 should eventually catch activation changes after poll timeout.
- Attach reload to startup reconcile: after emitting reconcile notifications,
  schedule reload/poll for the reconciled skills so the current mount can pick
  up daemon-written activation.
- Provide a fallback periodic activation reload when filesystem events are
  unavailable or unreliable. The interval should be configurable and low-frequency; this is
  an eventual-consistency repair loop, not a per-read check.
- Register newly discovered or inbox-installed skills into the activation
  observer set so new activations can be consumed without remounting.
- Emit protocol diagnostics for watcher reload outcomes where useful, without
  changing FUSE errno or mutating source data.

Out of scope for A5:

- Parsing `latest.json`, findings, policy, or scan status.
- Running scan/check inside SkillFS.
- Reading activation state on every FUSE read.
- Changing fd pin semantics. Already-opened handles keep their pinned target;
  new `lookup`/`open`/`readdir` operations use the refreshed resolver.
- Expanding source-tree watcher coverage beyond activation state convergence.

Acceptance criteria:

- If SkillFS starts hidden because activation is missing, and activation is
  written later, the mounted view converges without remount.
- If notify-triggered poll times out and activation is written after the
  timeout, the watcher or periodic repair loop still refreshes the resolver.
- If daemon reconcile or operator action updates activation without a FUSE
  mutation, SkillFS eventually observes the update.
- If the notify socket is unavailable and daemon repair happens later, SkillFS
  eventually observes the repaired activation.
- Startup reconcile can lead to a current-mount resolver refresh once daemon
  activation is written.
- Invalid or inconsistent xattr/json activation still hides the skill.
- Existing decision-command mode and activation reload fast path remain
  backwards compatible.

### A6/B1: Ledger Backing Root For Source-Side Security Work

Status: **implemented**

Goal: decouple the agent-visible FUSE view from the external ledger's
source-side working path, especially for in-place security mounts.

Problem statement:

In an in-place mount, the original skill source path is over-mounted by
SkillFS. That is the desired agent-facing boundary: reads can be hidden,
served from a fallback snapshot, or served from the current source depending
on activation state. The external security daemon, however, must scan and
version the live source tree. If it scans the same over-mounted path, a hidden
skill may be invisible and a fallback skill may appear as an older trusted
snapshot. The trusted-writer gate only controls selected `.skill-meta/**`
mutations through the FUSE path; it does not give another process access to
the original live source behind the in-place mount.

The implemented solution is a ledger backing root: a private source alias
prepared before the in-place mount becomes active. SkillFS continues to expose
the normal mount path to agents, while the external security daemon scans and
writes activation state through the backing root.

Conceptual layout:

```text
agent-visible path
  -> SkillFS FUSE view

ledger backing root
  -> private alias of the live source tree

external daemon
  -> scans <ledger-backing-root>/<skill>
  -> writes .skill-meta/activation.json and activation xattr

SkillFS
  -> sends socket notify v2 canonicalSkillDir under the canonical source root
  -> writes N3 protocol event skillDir under the ledger backing root
  -> bootstraps/reloads activation from the ledger backing root
  -> exposes hidden/current/fallback through the FUSE view
```

Scope:

- Add a first-class `ledger_backing_root` / `ledger_work_root` concept to the
  security mount configuration.
- For in-place mounts, require or automatically create a backing root before
  the FUSE over-mount, using a private bind mount or equivalent source alias.
  After the bind mount, immediately mark the backing root `MS_PRIVATE | MS_REC`
  to prevent host mount-propagation events (most critically the in-place FUSE
  over-mount) from leaking into the backing root. If make-private fails,
  startup fails closed: the bind mount is unmounted, any created temp directory
  is cleaned up, and `BackingRootError::MakePrivateFailed` is returned.
- For non-in-place mounts, allow the same concept to be used as a normalized
  daemon working path. It may point at the source directly or at a private
  alias, but SkillFS should present one consistent path shape to the daemon.
- Use the canonical source root for socket notify v2 `canonicalSkillDir`.
- Use the backing root for N3 protocol event `skillDir`, activation
  bootstrap, activation reload, startup reconcile event-log entries,
  activation watching, and any future source-side daemon-facing event
  payloads.
- Keep the agent-visible FUSE path unchanged. Agents should not need to know
  whether a backing root exists.
- Validate that the backing root is outside the agent-visible mount path and
  does not resolve through the FUSE view.
- Create or validate the backing root under a private parent directory with
  owner-only access. Treat it as a privileged management entry point, not as
  a user-visible path.
- On shutdown, clean up any bind mount and temporary directory that SkillFS
  created.
- Keep `.skill-meta/**` trusted-writer semantics separate. Trusted-writer
  controls the FUSE entry point; backing-root access is controlled by OS
  permissions and mount setup.

Out of scope for A6/B1:

- Changing activation JSON or xattr schema.
- Changing active mapping or fd pin behavior.
- Letting ordinary agents access the backing root.
- Replacing the trusted-writer gate.
- Passing pre-opened source fds to the daemon. That is a possible future
  hardening step, but it is more complex than the backing-root rollout.

Acceptance criteria:

- In-place security mount can expose an activated FUSE view while the daemon
  scans the live source through the backing root.
- A hidden skill remains hidden through the FUSE path but is still visible to
  the daemon through the backing root.
- A fallback skill serves the trusted snapshot through the FUSE path while
  the daemon still scans the live current source through the backing root.
- Socket notify v2 payloads use canonical `canonicalSkillDir` plus the full
  `skillId`; the daemon resolves live source paths through the S1 resolver and
  does not infer in-place vs non-in-place mount mode from notify fields.
- N3 protocol event log payloads keep schema v1 and use backing-root
  `skillDir` so existing event-log consumers can read the live source.
- Activation bootstrap, reload, reconcile, and activation watching all use
  the same backing root.
- Backing-root setup fails closed when ownership, parent permissions, path
  shape, identity, or bind-mount setup is unsafe.
- Non-in-place security mounts can opt into the same backing-root path shape
  for daemon consistency without changing ordinary passthrough semantics.

### I2: Configurable Installer Staging Compatibility

Status: **implemented**

Goal: support existing installers that stage skill files under the managed
skills root before publishing the final skill directory, without requiring
them to implement the SkillFS-native `/.skillfs-inbox/<skill>/.install-complete`
sentinel protocol.

Problem statement:

Some installers create temporary directories directly under the configured
skills root, write multiple files there, and later rename or move the staged
directory into the final skill name. OpenClaw, for example, uses top-level
directories shaped like `.openclaw-install-stage-*`. Under an in-place
SkillFS mount, top-level directories are normally interpreted as skill
names. Without a staging bypass, the installer workspace can be treated as a
skill candidate and can enter normal notify/activation handling before the
install is complete.

The existing inbox sentinel remains the precise SkillFS-native install
protocol, but it is too invasive for installers that already write directly
to the configured skills directory. I2 adds a compatibility layer for
well-known installer staging directories with a rename-boundary completion
model.

Scope:

- Add an installer staging configuration surface:

  ```toml
  [install]
  staging_patterns = [".openclaw-install-stage-*"]
  unactivated_visibility = "hidden"
  ```

- Keep pattern matching intentionally small and auditable. The initial shape
  supports exact names and prefix-star patterns such as
  `.openclaw-install-stage-*`; arbitrary regex is not supported.
- Match staging patterns only at the managed root level. A matching name
  inside a skill directory is ordinary passthrough content, not an installer
  workspace.
- Treat matching staging roots as installer-private workspaces, not skills:
  they must not participate in active resolver lookup, activation bootstrap,
  skill-discover, or normal skill notify. Staging roots are hidden from
  `/skills` readdir/opendir but remain accessible for exact-path access —
  lookup, stat, opendir, readdir, read, write, create, mkdir, rename, unlink,
  rmdir, and setattr all follow normal physical passthrough behavior. When an
  active resolver is attached, staging roots bypass the resolver hidden gate
  so installers can traverse and populate the staging directory through the
  FUSE mount. SKILL.md inside a staging root is served as a raw physical file
  (no compiler pass, no virtual size projection).
- Allow passthrough mutation inside the staging workspace, subject to the
  existing sensitive-path boundaries.
- Suppress all notify for intermediate writes inside staging. Staging root
  names are never sent to the daemon — not through normal notify, not through
  any timeout heuristic.
- When a staging root is renamed to a valid final skill directory name, treat
  that rename as the install-complete boundary and enqueue exactly one rename
  mutation notification through the background notify worker (protocol event
  log, socket send, activation reload poll, watcher registration). The
  notification uses `eventKind: "rename"` — not "install-complete" which is
  an internal/historical flush concept only. The rename notification is
  non-blocking: the FUSE reply returns immediately without waiting for
  socket I/O or activation reload.
- Reject rename targets that are invalid skill names, `.skill-meta`,
  `.skillfs-inbox`, lifecycle reserved roots, `skill-discover`, or other
  sensitive namespaces.
- Preserve fail-safe activation semantics by default: missing activation still
  maps to hidden in security activation mode. A compatibility setting that
  exposes unactivated source, if ever added, must be explicit and documented
  as fail-open.

An optional quiet-timeout heuristic supports installers that write files
directly into a final skill directory without using the staging rename
boundary or the `.install-complete` sentinel. When `quiet_timeout_ms` is
configured in the `[install]` section, SkillFS tracks mutation timestamps per
final skill name and emits an aggregated mutation notification (using the
last observed mutation kind, e.g. "write") after the configured quiet window
expires with no further writes. Multiple writes within the window are
collapsed into one notification. The quiet timeout does not apply to staging
roots — staging roots complete only through the rename boundary. The quiet
timeout is an installer compatibility notification, not a security approval;
activation still determines visibility.

**Protocol note:** "install-complete" is not a protocol-level `eventKind`.
It is a historical/internal flush reason only. The formal notify protocol
uses ordinary mutation event kinds (rename, write, create, unlink, rmdir,
truncate, reconcile). Debounce, quiet-timeout, and staging rename only
determine *when* to flush aggregated events — they do not change the event
facts.

Direct final-skill pending install is implemented in I3 below.

**Backing root requirement:** In in-place/security mode with activation or
notify enabled, the daemon-facing backing root must exist and be accessible
at startup. Activation bootstrap, activation reload, activation watcher, and
N3 protocol event-log `skillDir` use the backing root path. Socket notify v2
uses canonical `canonicalSkillDir` and the full `skillId`; it does not send the
backing root or the agent-visible FUSE over-mount path.

Out of scope for I2:

- Replacing the inbox sentinel protocol.
- Parsing installer-specific manifests.
- Treating arbitrary dot-prefixed directories as safe.
- Exposing unactivated skills by default.
- Running scan/check inside SkillFS.
- Deleting or rolling back failed installer staging directories.
Acceptance criteria:

- A configured `.openclaw-install-stage-*` root can be created and written
  under the managed root without being parsed as a skill.
- Intermediate writes inside a configured staging root do not trigger any
  notify — not normal skill notify and not install-complete. The staging
  root name is never sent to the daemon.
- Renaming a configured staging root to a valid skill name triggers exactly
  one rename mutation notify for the final skill (eventKind: "rename").
- Invalid final names and sensitive targets are rejected.
- Missing activation remains hidden by default after install notify; exposure
  still depends on daemon-written activation state.
- Existing inbox tests, normal skill mutation notify tests, activation reload
  tests, and lifecycle namespace tests continue to pass.

### I3: Direct Final-Skill Pending Install

Status: **implemented**

Goal: support installers that create the final skill directory first and
populate it in-place, without using a staging rename boundary or the
SkillFS inbox sentinel protocol.

Problem statement:

Some installers create the target skill directory directly under the managed
skills root and then write files into it progressively.  Under an in-place
SkillFS mount with activation mode, such a directory has no activation entry
in the active resolver and is therefore hidden from FUSE reads.  Worse, the
normal notify path may trigger a daemon scan before the skill is complete;
the daemon may reject the incomplete candidate and write a `target: null`
activation, permanently hiding the directory and causing subsequent
installer writes to fail.

Scope:

- Add a `PendingInstallController` that tracks newly created,
  not-yet-activated final skill directories.  A skill is eligible for
  pending tracking when the pending controller is attached and the active
  resolver has no entry for it.  Already-activated skills (current,
  fallback, or hidden) continue to use the normal mutation notify path.
- Pending skills are hidden from `/skills` listing (readdir/opendir) and
  agent discovery but remain accessible for exact-path access: lookup, stat,
  opendir, readdir, read, write, create, mkdir, rename, unlink, rmdir, and
  setattr all follow normal physical passthrough behavior.
- `SKILL.md` inside a pending skill directory is served as a raw physical
  file (no compiler pass, no virtual size projection).
- Intermediate mutations for pending skills do not trigger normal
  notify/refresh/quiet-timeout.  Mutations are recorded in the pending
  controller instead.
- After a quiet window expires, the controller checks whether the skill
  directory has a complete shape: directory exists, `SKILL.md` exists and
  is parseable by the existing parser (Ok or Degraded status).  If
  complete, one aggregated ordinary mutation notification is emitted
  (eventKind matches the last observed mutation kind).  If incomplete, the
  entry stays pending and waits for the next mutation to restart the
  window.
- Staging roots, lifecycle reserved names, `skill-discover`,
  `.skill-meta`, and `.skillfs-inbox` are excluded from pending tracking.
- The quiet timeout for pending installs reuses the same duration as I2
  quiet-timeout but operates independently.
- "install-complete" is not emitted as a protocol-level event kind.
  The notification uses ordinary mutation kinds (write, create, mkdir,
  etc.).
- Activation still determines final visibility.  The pending notification
  triggers daemon processing; the daemon writes activation state; SkillFS
  reloads activation and exposes or hides the skill accordingly.

Out of scope for I3:

- Replacing the inbox sentinel protocol.
- Replacing the staging rename boundary.
- Parsing installer-specific manifests.
- Running scan/check inside SkillFS.
- Exposing unactivated skills by default.

Acceptance criteria:

- `mkdir /skills/new-skill` does not immediately notify when the skill has
  no activation entry.
- Pending skill does not appear in `/skills` listing.
- Pending exact path (metadata, readdir, write, create, mkdir) is
  accessible.
- `SKILL.md` missing at quiet timeout expiry: no notification.
- `SKILL.md` unparseable at quiet timeout expiry: no notification.
- `SKILL.md` complete at quiet timeout expiry: one ordinary mutation
  notification.
- Multiple writes within the quiet window collapse to one notification.
- Activation write makes the skill visible as current/fallback/hidden
  through the normal resolver path.
- Already-activated skill updates still use normal notify, not pending.
- Staging and inbox existing tests do not regress.

### Control Socket Meta Write API

Status: **implemented**

Goal: move activation writes out of the FUSE mount path. The external
security daemon should use the trusted control socket to ask SkillFS to update
activation state, instead of writing `.skill-meta/**` through the agent-visible
filesystem view.

Problem statement:

The current production identity work can prove that a peer is a trusted daemon
by combining `SO_PEERCRED` with executable identity checks. However,
activation writes still rely on file-path mutation surfaces when the daemon
writes `.skill-meta/activation.json` or activation xattrs directly. That keeps
the runtime contract dependent on mount-path trusted-writer behavior and makes
it harder to tighten `.skill-meta/**` for ordinary FUSE users.

Scope:

- Extend the control socket protocol with activation-only write methods:

  ```json
  {"schemaVersion":"1","method":"meta.writeActivation","skillName":"demo-weather","activation":{...}}
  {"schemaVersion":"1","method":"meta.setActivationXattr","skillName":"demo-weather","activation":{...}}
  ```

- Accept requests only after the peer passes the existing control socket
  `SO_PEERCRED` + executable identity verification chain.
- Validate `skillName` as a canonical directory name. Directory name remains
  authoritative; `SKILL.md name:` must not create aliases.
- Restrict write targets to exactly:
  - `<skill_dir>/.skill-meta/activation.json`
  - `user.agent_sec.skill_ledger.activation` on the physical skill directory
- Reuse the same `ActivationRecord` parser and path validation used by the
  activation consumer. Invalid payloads must be rejected before writing.
- Write `activation.json` atomically by writing a temporary file in
  `.skill-meta`, fsyncing as appropriate, and renaming into place.
- Set activation xattr with no-follow semantics against the physical skill
  directory.
- After a successful write, trigger activation reload or watcher immediate
  check for the affected skill.
- Emit a protocol event that records the control-plane write and resulting
  reload outcome where available.

Out of scope for control socket meta writes:

- General-purpose `.skill-meta/**` file writes.
- Writing manifests, findings, signatures, version snapshots, or scan output.
- Allowing untrusted FUSE clients to invoke the meta write path.
- Replacing daemon-owned policy computation.
- Changing activation schema.

Acceptance criteria:

- A trusted peer can write activation JSON through the control socket and new
  FUSE opens observe the updated active mapping.
- A trusted peer can set activation xattr through the control socket and the
  xattr-preferred activation path observes it.
- Mismatched uid/gid/executable identity is denied before any write.
- Invalid skill names, missing skills, unsafe activation targets, and malformed
  activation payloads are rejected without mutating disk state.
- Activation JSON writes are atomic: readers never observe partial JSON.
- Successful writes produce protocol events and schedule reload.
- Existing direct activation-file and activation-xattr consumers keep working.

### Mount-Path Trusted Writer Tightening

Status: **planned**

Goal: make the control socket the primary production path for activation
writes, and reduce mount-path trusted writer to an explicit compatibility
fallback.

Scope:

- Update production documentation to recommend the control socket meta write
  API for activation updates.
- Keep ordinary FUSE mount-path mutation of `.skill-meta/**` denied by
  default.
- Reclassify `--trusted-writer-exe` as a fallback or compatibility mechanism
  for deployments that cannot yet use the control socket.
- Keep process-name-only `--trusted-writer` documented as compatibility-only
  because process `comm` is spoofable.
- Add tests proving that disabling mount-path trusted writer does not block
  control socket activation writes.

Out of scope for mount-path trusted writer tightening:

- Removing the existing trusted-writer code path immediately.
- Changing read-side activation behavior.
- Adding general-purpose metadata write methods.

Acceptance criteria:

- Production examples use `--control-socket` and `--trusted-peer-exe` for
  activation writes.
- Compatibility examples clearly mark `--trusted-writer-exe` as fallback.
- `.skill-meta/**` remains denied for ordinary FUSE clients.
- Control socket activation writes still work when no mount-path trusted
  writer is configured.

### Provider Lifecycle And Activation Consistency

Status: **planned**

Goal: make activation updates idempotent, observable, and consistent across
JSON, xattr, reload, daemon restart, and watcher repair paths.

Scope:

- Define idempotent behavior for repeated writes of the same activation target.
- Preserve the xattr-first / JSON-fallback contract:
  - xattr valid + JSON absent or invalid: xattr wins;
  - xattr and JSON valid but disagree: fail-safe hidden;
  - unexpected xattr read errors: fail-safe hidden.
- Add control socket method outcomes that distinguish updated, unchanged,
  rejected, and reload-failed cases.
- Make active mapping state observable enough for integration tests and
  operations diagnostics without exposing policy internals.
- Expand tests for daemon restart reconcile, missed notifications, watcher
  convergence, and control socket activation writes.

Out of scope for provider lifecycle and activation consistency:

- Parsing scan findings or risk policy inside SkillFS.
- Making activation reload synchronous on every FUSE read.
- Changing fd pin semantics.

Acceptance criteria:

- Rewriting the same activation is safe and produces an unchanged outcome.
- JSON/xattr mismatch still hides the skill.
- Reload after control socket write is observable through protocol events.
- Watcher convergence still repairs activation state when a daemon writes
  activation outside the current notify cycle.
- Existing activation-mode, notify, reload, and watcher tests remain green.

## CLI And Config Direction

The current CLI surface is:

```text
--security
--decision-command <COMMAND>
--activation-mode off|file
--notify-socket <PATH>
--activation-events-log <PATH>
--activation-reload-mode off|poll
--events-log <PATH>
--control-socket <PATH>
--trusted-peer-exe <PATH>   # production: socket peer executable identity
--trusted-peer-uid <UID>
--trusted-peer-gid <GID>
--trusted-writer-exe <PATH> # fallback: mount-path .skill-meta writer
--trusted-writer <NAME>     # deprecated / compatibility; process comm is spoofable
--ledger-backing-root <PATH>
--config <PATH>
```

The production activation path should be explicit and opt-in while it is being
introduced. Suggested configuration shape:

```toml
[activation]
mode = "file"        # off | file
reload = "poll"      # off | poll
reload_interval_ms = 250
reload_timeout_ms = 5000

[notify]
mode = "off"         # off | unix-socket
socket_path = "/run/user/1000/agent-sec-core/daemon.sock"

[activation_events]
log_path = "/var/log/skillfs-activation-events.jsonl"

[control_socket]
path = "/run/skillfs/control.sock"
trusted_peer_exe = "/usr/local/bin/agent-sec-cli"
trusted_peer_uid = 1000
trusted_peer_gid = 1000

[ledger]
backing_root = "/run/skillfs-ledger/<mount-id>/source"

[install]
staging_patterns = [".openclaw-install-stage-*"]
unactivated_visibility = "hidden"
# quiet_timeout_ms = 5000
```

Do not silently switch existing `--security --decision-command` users to the
activation path. Compatibility is important during security-side rollout.

### I4: Installer Post-Publish Grace Window

After staging rename or direct-write pending install completion, installers
like OpenClaw continue writing metadata (e.g. `.openclaw/.fs-safe-replace...tmp`)
into the final skill directory. If the Ledger has already written activation and
SkillFS switched the skill to hidden/fallback, these exact-path writes are
blocked. The post-publish grace window is a conservative, explicit-whitelist
mechanism that allows these writes for a limited time.

Configuration (both required together, default off):

```toml
[install]
post_publish_grace_ms = 5000
post_publish_write_patterns = [".openclaw/**"]
```

Design:
- Grace session starts immediately after staging rename completes or
  after pending install fires its completion notification.
- Only exact-path writes matching `post_publish_write_patterns` bypass
  the active resolver's hidden/fallback view.
- `.skill-meta/**` is always rejected regardless of grace.
- Lifecycle reserved roots, skill-discover, absolute paths, and `..`
  traversal are rejected at pattern validation time.
- Grace writes produce normal mutation notify events (no new event kind).
- Activation still controls listing/read view; grace only bypasses the
  hidden gate for whitelisted write operations.
- Symlink/hardlink policy is not relaxed by grace.
- Session expires after `post_publish_grace_ms`; expired sessions are
  cleaned up lazily.

Files:
- `security/install.rs`: `PostPublishWritePattern`, `PostPublishGraceController`,
  `validate_post_publish_patterns`, session management.
- `security/config.rs`: `InstallSection` fields and validation.
- `fs/paths.rs`: `is_post_publish_grace_allowed`, `should_reject_hidden_write`.
- `fs/callbacks/meta.rs`: Grace bypass in lookup/getattr.
- `fs/callbacks/dir.rs`: Grace bypass in readdir/opendir.
- `fs/callbacks/read.rs`: Grace bypass in open.
- `fs/callbacks/write.rs`: Grace rejection in create.
- `fs/callbacks/mutate.rs`: Grace rejection in mkdir/unlink/rmdir; session
  start after staging rename.

I4 is complete when:
- Both `post_publish_grace_ms` and `post_publish_write_patterns` must be
  configured together (startup error otherwise).
- Staging rename triggers a grace session.
- Pending install completion triggers a grace session.
- Whitelisted writes succeed within the grace window.
- Non-whitelisted writes are rejected within the grace window.
- Writes are rejected after grace expires.
- `.skill-meta/**` is rejected even during grace.
- All existing staging, pending, notify, and activation tests pass.

## Acceptance Criteria

A2 is complete when:

- All A1 acceptance criteria still pass (json-only activation unaffected).
- Xattr-only activation (no `activation.json`) resolves the snapshot.
- Missing xattr falls back to `activation.json` transparently.
- Invalid xattr hides the skill even when `activation.json` is valid.
- Xattr/json target mismatch hides the skill.
- Unsupported-xattr environments fall back to `activation.json` (tests
  deterministically skip when the substrate lacks `user.*` support).
- FUSE read path still uses the startup-loaded `ActiveSkillResolver`;
  no per-read xattr calls.

A1 is complete when:

- Valid activation snapshot maps `/skills/<name>` to the snapshot tree.
- `target = null` hides the skill from `readdir` and `lookup`.
- Invalid target shapes hide the skill and produce a diagnostic error.
- Missing activation hides the skill only when activation mode is enabled.
- Existing `--decision-command` behavior is unchanged when activation mode is
  disabled.
- Fallback snapshot reads continue to respect fd pinning.

Minimum validation:

```text
cargo check -p skillfs-fuse -p skillfs
cargo test -p skillfs-fuse --lib security::activation
cargo test -p skillfs-fuse --lib activation_reload
cargo test -p skillfs-fuse --lib security::notify
cargo test -p skillfs-fuse --lib security::protocol_events
cargo test -p skillfs-fuse --test ledger_active_mapping_tests
cargo test -p skillfs-fuse --test ledger_demo_refresh_tests
cargo test -p skillfs-fuse --test notify_client_tests --test notify_fuse_tests
cargo test -p skillfs-fuse --tests
cargo test -p skillfs-core
```

## Known Risks

- Reading activation on every FUSE read would be simple but too expensive and
  can break fd consistency. Prefer explicit reload into `ActiveSkillResolver`.
- Xattr and JSON disagreement must not pick one arbitrarily. Hide and record a
  diagnostic event.
- Notify success does not imply scan completion. Do not expose a new version
  until activation state changes safely.
- The daemon can miss events while down. Reconcile must be based on disk state,
  not solely on event log completeness.
- Without A5, activation mode is a startup-plus-triggered-reload cache. It can
  temporarily diverge from daemon-written activation when updates happen after
  a poll timeout or outside the current mount's notify path. A5 is the planned
  convergence mechanism.
- Without A6/B1, in-place activation mode still needs careful deployment:
  external daemons must not scan the over-mounted FUSE path when they need the
  live source. A private backing root is the mechanism for making that path
  split explicit and testable. A6/B1 is implemented: the
  `--ledger-backing-root` flag and `[ledger].backing_root` config enable a
  private source alias for daemon-facing operations.
- The trusted daemon can now write activation through the dedicated control
  socket meta write API. Mount-path trusted writer remains a compatibility
  fallback until the trusted-writer tightening work makes that fallback
  explicitly secondary.
