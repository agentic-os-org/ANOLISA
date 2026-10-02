# SkillFS Filesystem Capability Record

This document is the maintained record for SkillFS filesystem behavior,
security integration surfaces, test coverage, and known boundaries.
It replaces the earlier split POSIX progress, POSIX development plan, and
roadmap notes.

The intent is to describe what SkillFS can do today, why the main design
boundaries exist, and how the implementation evolved. It intentionally avoids
machine-local paths, branch names, host names, and internal incident history.

## Design Position

SkillFS is a FUSE-based filesystem layer over a physical skill source
directory. It provides:

- a stable `/skills/<name>` runtime view;
- compiled `SKILL.md` reads;
- Linux-like passthrough behavior for ordinary files;
- explicit security extension points for policy, audit, notification, and
  activation;
- active mapping from a skill name to current source, trusted snapshot, or
  hidden state.

SkillFS does not own security judgement. External providers own scanning,
signature validation, policy evaluation, findings, and version decisions.
SkillFS owns filesystem observation, safe path validation, event delivery, and
view exposure.

## Skill Identity And View Semantics

SkillFS treats the skill directory name as the canonical runtime identity.
`SKILL.md name:` is metadata and never creates a second `/skills/<declared>`
alias. External decision results whose `skillName` does not match
`basename(skill_dir)` are rejected.

Core view semantics:

| Surface | Current behavior |
| --- | --- |
| `/skills` | Virtual directory exposing active skills. |
| `/skills/<name>/SKILL.md` | Read returns compiled content. |
| `skill-discover` | Always visible virtual read-only skill. |
| ordinary files under a skill | Mostly physical passthrough. |
| hidden active mapping | Omitted from readdir; lookup returns `ENOENT`. |
| fallback active mapping | New reads are served from a trusted snapshot. |
| open file descriptors | Target is pinned at open time; later mapping changes affect new opens only. |

## POSIX / FUSE Capability Matrix

SkillFS now covers the main POSIX surfaces needed for regular tool usage and
for the security-provider integration path.

| Area | Status | Notes |
| --- | --- | --- |
| open/create flags | Supported | Covers common Linux flags, including truncation and append behavior. |
| fd-backed read/write | Supported | Handles offset I/O and open-after-unlink behavior. |
| flush/fsync/fsyncdir | Supported | Passthrough paths sync physical descriptors where applicable. |
| getattr/metadata projection | Supported | Projects Unix mode, uid, gid, nlink, size, and times. |
| chmod/chown/utimens/truncate | Supported | Virtual paths keep read-only semantics. |
| access/statfs | Supported | Includes virtual-path boundaries. |
| opendir/readdir | Supported | Directory handles and stable snapshots are covered. |
| mkdir/rmdir/unlink/rename | Supported | Includes store sync for skill-level changes and rename flags where supported. |
| PATH_MAX fallback | Supported | Uses parent-fd `*at` fallbacks for long physical paths. |
| readlink / symlink identity | Supported | Physical symlink identity and raw target are preserved. |
| symlink creation | Supported with policy | Allows relative same-skill targets; rejects absolute, cross-skill, outside-source, `.skill-meta`, lifecycle, and virtual targets. |
| hardlink creation | Supported with policy | Allows same-skill regular-file links; rejects cross-skill, virtual, sensitive, directory, symlink, FIFO, and special-file sources. |
| FIFO creation | Supported | `mknod` accepts FIFO only. |
| device/socket mknod | Rejected | Block/char/socket/special nodes remain intentionally unsupported. |
| xattr | Supported for `user.*` | No-follow passthrough on ordinary paths; unsupported namespaces rejected. |
| fallocate | Not implemented | Deferred. |
| lseek `SEEK_DATA` / `SEEK_HOLE` | Not implemented | Deferred. |
| copy_file_range | Not implemented | Deferred. |
| full per-caller uid/gid enforcement | Not implemented | Requires broader FUSE permission and identity design. |

## Security Integration Capability Matrix

SkillFS security work is organized as filesystem mechanisms, not business
policy. The external provider is expected to decide pass/warn/deny/fallback
and write the resulting activation state.

| Area | Status | SkillFS responsibility |
| --- | --- | --- |
| Policy/event skeleton | Supported | `SecurityPolicy`, `SkillEvent`, event sink abstractions. |
| `.skill-meta/**` protection | Supported | Ordinary callers cannot mutate security metadata. |
| JSONL audit stream | Supported | Optional best-effort audit sink. |
| Runtime audit wiring | Supported | CLI can opt into audit JSONL and queue capacity. |
| Security mount mode | Supported | Can require in-place mount for stronger coverage. |
| Drift observation | Supported | Observes selected source-side changes for visibility. |
| Watcher shutdown | Supported | Explicit shutdown handles for long-lived embedders. |
| Lifecycle namespace reservation | Supported | Reserved roots are hidden/denied in ordinary views. |
| Management-view contract | API only | Helpers exist; FUSE management view is not enabled by default. |
| External decision protocol | Supported | Generic decision command can run `scan` then `resolve`. |
| Active resolver read mapping | Supported | Maps skill to current, snapshot, or hidden. |
| Dynamic decision refresh | Supported | Debounced FUSE writes can trigger external scan/resolve. |
| Trusted writer identity | Supported | Production: `--trusted-writer-exe` pins `/proc/<tgid>/exe` readlink + `(dev,ino)` file identity. Compatibility: `--trusted-writer` matches `comm` (spoofable, deprecated). Exe identity is the sole authorization basis when both are configured. |
| Install inbox namespace | Supported | `/.skillfs-inbox/<skill>` provides a write entrance for hidden candidates. |
| Installer staging compatibility | Supported | Configurable bypass for installer staging roots such as `.openclaw-install-stage-*`; rename to a valid skill emits a rename mutation notification (non-blocking). Optional quiet-timeout heuristic emits aggregated mutation notifications for direct final-skill writes when no mutations occur within a configured window. |
| Canonical skill identity | Supported | Directory name is authoritative; mismatched provider result is rejected. |
| Activation file consumer | Supported | Reads `.skill-meta/activation.json`. |
| Activation xattr consumer | Supported | Prefers `user.agent_sec.skill_ledger.activation`, falls back to JSON. |
| Notify change client | Supported | Sends debounced change notifications over Unix socket. |
| Protocol event log | Supported | Writes activation protocol JSONL events. |
| Runtime activation reload | Supported | Polls activation after notify and refreshes the active resolver. |
| Startup reconcile | Supported | Queues known skills through the notify worker, retries transient delivery until ACK, and bounds inconclusive authentication retries per endpoint. Exposes delivery counters and pending queue depth; see [Reconcile Delivery Durability](security/runtime-activation-implementation-plan.md#reconcile-delivery-durability). |
| Ledger backing root | Supported | Private source-side work path for external daemons, especially under in-place mounts. Bind mounts are marked `MS_PRIVATE\|MS_REC` to isolate propagation and prevent the FUSE over-mount from leaking into the backing root. |
| Control socket activation write | Supported | Trusted peers can write `activation.json` and activation xattr through the control socket instead of the FUSE mount path. Validates skill name, activation payload, writes atomically, and triggers reload. |
| Direct final-skill pending install | Supported | Newly created, not-yet-activated skill directories are tracked as pending installs: hidden from listing, writable via exact path, intermediate mutations suppressed. After a quiet window, completeness check (parseable `SKILL.md`) gates the aggregated mutation notification. Activation still determines visibility. |
| Post-publish grace window | Supported (default off) | Time-limited grace window after staging rename or pending install completion. Allows installer metadata writes to whitelisted paths (e.g. `.openclaw/**`) even when the active resolver marks the skill as hidden. `.skill-meta/**` always rejected. Configured via `post_publish_grace_ms` + `post_publish_write_patterns` in `[install]`. |

## Symlink Policy

Symlink creation is intentionally conservative.

Allowed by default:

- relative target;
- resolves inside the same skill;
- does not target `.skill-meta/**`;
- does not target lifecycle reserved roots.

Rejected by default:

- absolute target;
- cross-skill target;
- target outside the source tree;
- `.skill-meta/**`;
- lifecycle reserved roots;
- `skill-discover`;
- virtual paths.

The absolute-target rejection is important for non-in-place mounts. An
absolute symlink target can point at the physical source path; when userspace
follows it, the access may bypass the FUSE mount, audit stream, active mapping,
and `.skill-meta` policy. If absolute same-skill symlinks are ever enabled,
they should be restricted to a documented security-mode layout with tests that
prove the path remains inside the intended enforcement boundary.

## xattr Policy

SkillFS exposes only `user.*` xattrs on ordinary passthrough paths. The
implementation uses no-follow `l*xattr` syscalls so symlink identity is not
silently followed.

Security-relevant activation xattr consumption is separate from the FUSE xattr
callbacks. SkillFS reads `user.agent_sec.skill_ledger.activation` directly
from the physical skill directory as part of activation loading. If both xattr
and `activation.json` exist and disagree, the skill fails safe to hidden.

## Runtime Activation Flow

The production-oriented activation path is:

```text
FUSE mutation
  -> debounce per skill
  -> protocol event log
  -> notify external daemon

External daemon
  -> check / scan / policy / reconcile
  -> write activation xattr and/or .skill-meta/activation.json

SkillFS
  -> reload activation
  -> update ActiveSkillResolver
  -> expose current, fallback snapshot, or hidden
```

SkillFS does not parse scan status, policy, findings, or ledger internals.
It validates the activation target and updates the filesystem view.

In in-place security mounts, the agent-visible path is intentionally the FUSE
view. That path is not a safe source of truth for the external daemon because
hidden skills may be invisible and fallback skills may resolve to a snapshot.
The ledger backing root creates a private source-side work path for daemon
scan, activation, reload, and N3 protocol event-log v1 `skillDir` values.
Socket notify v2 keeps `canonicalSkillDir` under the canonical source root;
the daemon resolves that identity to the live backing path through the control
socket when it needs source access. The backing root complements the
trusted-writer gate: trusted-writer controls selected `.skill-meta/**`
mutations through the FUSE entry point, while the backing root is protected by
OS ownership, private parent permissions, identity checks, and mount setup.

## Test Coverage

The repository has three layers of tests:

- Rust unit and integration tests for core, FUSE, security modules, activation,
  notify, event logging, inbox, lifecycle, xattr, link/FIFO, path limits, and
  fd pin behavior.
- `scripts/test.sh` for end-to-end mount smoke coverage.
- Optional external POSIX harness based on pjdfstest, with manifests for
  intentionally unsupported surfaces and tests blocked by unsupported helper
  dependencies.

The external harness is operator-driven and is not part of normal `cargo test`.

## Installer Compatibility

SkillFS-native installs can use `/.skillfs-inbox/<skill>` and write the
`.install-complete` sentinel to mark a multi-file candidate as complete. Some
external installers instead create top-level staging directories inside the
managed skills root and later publish the final skill directory. Those
staging directories should not be interpreted as skills while files are still
being written.

The installer staging compatibility layer allows a small, configured set of
root-level staging patterns, such as `.openclaw-install-stage-*`, to bypass
normal skill parsing and notify until the install is complete. Staging roots
are hidden from the `/skills` parent listing and agent discovery view but
remain fully accessible for exact-path access: lookup, stat, opendir, readdir,
read, write, create, mkdir, rename, unlink, rmdir, and setattr all follow
normal physical passthrough behavior. When an active resolver is attached,
staging roots bypass the resolver hidden gate so installers can traverse and
populate the staging directory through the FUSE mount. SKILL.md inside a
staging root is served as a raw physical file (no compiler pass). Intermediate
writes inside a staging root are silently suppressed from normal
notify. A rename from staging to a valid skill name triggers exactly one
rename mutation notification (non-blocking). The FUSE reply is not delayed
by socket I/O or activation reload. Invalid rename targets (sensitive
namespaces, bad skill names) are deterministically rejected.

When `quiet_timeout_ms` is configured in the `[install]` section, an optional
quiet-timeout heuristic emits an aggregated mutation notification (using the
last observed mutation kind, e.g. "write") after a configured period of no
mutations inside a final skill directory. This supports installers that write
files directly into a legitimate skill directory without using the staging
rename boundary or the `.install-complete` sentinel. Multiple writes within
the quiet window are collapsed into one notification; subsequent writes after
a fired notification start a new window. The quiet timeout does not apply to
staging roots — staging roots complete only through the rename boundary. The
quiet timeout is an installer compatibility notification, not a security
approval; activation still determines visibility.

Note: "install-complete" is not a protocol-level event kind. It is an
internal/historical flush concept only. The formal notify protocol uses
ordinary filesystem mutation events (rename, write, create, etc.). In
in-place/security mode, the daemon-facing backing root must be configured
and accessible. Socket notify v2 identifies the skill with
`canonicalSkillDir` under the canonical source root and the full `skillId`;
the daemon resolves that identity before accessing live source. The separate
N3 protocol event log remains schema v1 and writes `skillDir` under the live
backing root. Activation bootstrap, reload, and watching also use that live
root rather than the agent-visible FUSE view.

Missing activation remains hidden by default; the notification triggers
security processing and does not approve exposure. Configuration is via the
`[install]` section of the security config file, with `staging_patterns`,
`unactivated_visibility`, and optional `quiet_timeout_ms` fields.

Direct final-skill pending install is implemented.  When a
`PendingInstallController` is attached with an `ActiveSkillResolver`, newly
created skill directories that have no activation entry are treated as pending
install transactions.  Pending skills are hidden from `/skills` listing and
agent discovery but remain writable via exact-path access.  `SKILL.md` inside a
pending skill is served as a raw physical file (no compiler pass).  Intermediate
mutations are aggregated locally and do not trigger normal notify, refresh, or
quiet-timeout.  After a quiet window, the controller checks whether the
directory has a complete skill shape (directory exists, `SKILL.md` exists and is
parseable).  If complete, one aggregated ordinary mutation notification is
emitted.  If incomplete, the entry stays pending and waits for the next
mutation.  Already-activated skills continue to use the normal mutation notify
path.  Activation still determines visibility; the pending notification triggers
daemon processing, not exposure.

## Implementation History

The implementation evolved in package-sized steps:

1. **POSIX baseline**: open/read/write, metadata, directory handles, rename,
   sync, access, statfs, and acceptance testing.
2. **Symlink identity**: physical symlink identity and `readlink` support.
3. **Security seams**: policy/event skeleton, `.skill-meta` protection, audit
   stream, and CLI runtime wiring.
4. **Security mount mode**: explicit in-place gate for stronger enforcement.
5. **Drift observation**: source-change observation and watcher lifecycle.
6. **Lifecycle namespace reservation**: hidden reserved roots and management
   helper contract.
7. **External POSIX harness**: pjdfstest runner, manifests, smoke/full
   profiles, and baseline-driven fixes.
8. **Compatibility hardening**: PATH_MAX fallbacks, open-after-unlink fd
   survival, safe links, FIFO creation, and user xattrs.
9. **External decision integration**: generic decision command, active resolver,
   dynamic refresh, event stream, and install inbox.
10. **Production activation path**: activation file/xattr consumption,
    notify client, protocol event log, runtime reload, and startup reconcile.
11. **Source-side daemon path**: ledger backing root for in-place and
    security-mode deployments where the daemon must scan live source rather
    than the agent-visible FUSE view.
12. **Control socket activation write**: trusted peers can write activation
    state through the control socket instead of the FUSE mount path, enabling
    future tightening of `.skill-meta/**` for ordinary FUSE clients.
13. **Direct final-skill pending install**: newly created skill directories
    without activation are tracked as pending installs with quiet-timeout
    completeness check.
14. **Post-publish grace window**: time-limited installer metadata write
    window after staging rename or pending install completion.

## Current Boundaries

The following are intentionally outside the current filesystem core:

- full security policy computation;
- risk scoring and findings parsing;
- daemon-side reconcile logic;
- production-grade trusted writer identity;
- capability / command-set product modeling;
- device node creation;
- `fallocate`, `lseek SEEK_DATA/SEEK_HOLE`, `copy_file_range`;
- full per-caller uid/gid/sticky-bit fidelity.

These should be designed as separate packages with explicit security and test
criteria.
