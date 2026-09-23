# cosh-ng Login Upgrade Notice Design

Date: 2026-09-23

Related documents: [runtime contracts](runtime-contracts.md), [ECS auth provisioning](ecs-auth-provisioning.md)

## Summary

When a user starts an interactive cosh-shell session, the shell can surface a
short, non-blocking notice if a newer cosh-ng version is available, together
with the exact command to upgrade. The check must cover the two install
topologies in the field — devices managed by the `anolisa` CLI and devices that
only carry the RPM package — and it must never slow down or block login, never
show an error to the user, and never require network access to succeed. This
document records the problem, the design decisions and their rationale, the
alternatives that were considered and rejected, and the behavioral boundaries.
It records the externally relevant design and implementation boundaries, not a
task breakdown.

## Background and Motivation

cosh-ng ships frequently, but there is no in-product signal that a device is
running a stale build. Users discover new versions out of band, so fixes and
capabilities land slowly on real machines. Login is the natural place to nudge:
it happens regularly, it is already a moment where the shell prints a banner,
and the user is present and able to act.

The difficulty is that "how do I learn the latest version, and how do I upgrade"
depends on how cosh-ng was installed:

- **anolisa-managed** — the device has the `anolisa` CLI, which already knows
  how the component was provisioned (raw binary or delegated RPM) and can resolve
  the latest published version and the correct upgrade path.
- **pure RPM** — the device has no `anolisa`; cosh-ng was installed as a system
  package and is upgraded through `dnf`/`yum`.

A login-time check also runs on machines that are slow, offline, or behind a
proxy. So the dominant design constraint is not "how to detect a version" but
"how to make detection completely invisible when it is slow or impossible".

## Goals

- Detect an upgradeable cosh-ng version at login and present a concise notice
  with the correct, method-specific upgrade command.
- Avoid network-dependent latency on the login path.
- Work for both the anolisa-managed and pure-RPM install topologies.
- **Fail silent**: any timeout, missing tool, or unexpected output results in no
  message and no error. RPM cache validation is the documented bounded local
  exception to the otherwise asynchronous probe.

## Non-Goals

- **Auto-upgrading.** This feature only informs; it never mutates the system.
- **Implementing package candidate resolution in the shell.** anolisa and the
  package manager remain authoritative for candidate discovery. The shell only
  compares an anolisa target with its own running SemVer.
- **Guaranteeing detection for unmanaged binaries.** A locally built or
  hand-copied binary, owned by neither anolisa nor an RPM package, is out of
  scope and stays silent. The shell does not query GitHub Releases or another
  upstream source for this `other` installation class.
- **Telemetry.** Recording whether the notice was shown or acted on is not part
  of this design.

## Design Principles

1. **Network-dependent detection is not on the login path.** Login reads the
   local cache and hands anolisa, package-manager, and network-dependent work to
   the background. The probe's latency, success, or failure never gates the
   prompt.
2. **The local result file is a cache of the last *successful* probe, with no
   TTL.** Every login re-probes. The file exists to do two things: let the very
   next login show a notice immediately, and let an offline login still show the
   last known state. It is never treated as authoritative beyond "the newest
   thing we last managed to confirm". RPM cache entries are additionally
   validated against the installed package and EVR before display.
3. **Detection has a single, ordered source of truth.** anolisa is preferred
   because it already unifies both install methods and owns the upgrade path;
   RPM ownership is the fallback for devices without anolisa.
4. **Everything fails silent.** Silence is always a correct outcome; a wrong or
   noisy message is not.

## Key Design Decisions

### D1 — Inform, do not upgrade

The notice states the current and latest version and the command to run; it does
not run it. Upgrading may require privilege (RPM), may replace the running
binary, and is a decision the user should make deliberately. Keeping the feature
read-only also keeps it safe to run unconditionally at every login.

### D2 — Background detection with bounded RPM cache validation

**Decision:** spawn anolisa and package-manager detection off the login path.
Cache reads are synchronous. For an RPM cache entry, the shell additionally runs
`rpm -qf` to verify that the package owner and installed EVR still equal the
values that produced the cached notice. This local query has a 500 ms bound.

**Rationale.** The upgrade panel is rendered once and cannot be safely withdrawn
from a terminal after it has been displayed. Deferring RPM validation would let a
recently upgraded machine show an obsolete package or EVR in that panel. The
bounded `rpm -qf` query reads only the local RPM database and does not contact a
repository; it is normally fast. In exceptional rpmdb or local-I/O conditions,
the shell may spend up to 500 ms before suppressing the notice. This is an
intentional trade-off for an accurate first panel.

**Rejected alternative — defer RPM cache validation to the background.** This
would avoid the bounded local query but could display a stale, non-retractable
notice during the current login. A background `dnf`/`yum` update check remains
required for candidate discovery and never gates the prompt.

### D3 — Prove installation ownership before probing

**Decision:** first associate the running executable with a supported install.
When `anolisa` is available on `PATH`, the shell runs `anolisa status cosh-ng
--json` and compares the canonical `current_exe()` path with every active
`integrity:<path>` entry. The integrity entry's status is not a gate: a matching
`sha256_mismatch` still proves ownership. Otherwise, the shell asks `rpm -qf
--qf '%{NAME}' <current_exe>` and keeps the returned package name rather than
assuming it is `cosh-ng`. Binaries that satisfy neither condition stay silent.

**Rationale.** Upgrade availability and the upgrade command describe an
installation, not merely a version string. Checking an unrelated managed copy
while running a local build can produce a valid but inapplicable notice. The
path comparison makes anolisa authoritative only for the executable it reports
as owned, while retaining support for historical RPM names such as
`copilot-shell`. Once anolisa ownership is established, its dry-run result is
the selected source; a failed or incomplete anolisa probe is silent rather than
falling through to RPM.

### D4 — "Upgrade available" is inferred from the upgrade plan, not the `updated` flag

This is the load-bearing correctness decision.

The anolisa dry-run result is a preview of what an upgrade *would* do. Its
`updated` field reports whether a version *actually changed*, which on a dry-run
is always negative — it is the same (`false`) whether the device is already on
the latest version or an upgrade is waiting. The signal that distinguishes the
two cases is the **presence of a non-empty upgrade plan** (equivalently, a
resolved target version that differs from the installed one). Availability is
therefore read from the plan / version delta, and the `updated` flag is ignored
for detection.

**Why this matters:** keying off `updated` would make the anolisa path never
report an upgrade, silently defeating the whole feature on exactly the devices
it most wants to serve. This dependency on anolisa's dry-run semantics is a
cross-component contract and is called out again under Boundaries.

For the RPM fallback, the equivalent question — "is a newer candidate offered?"
— is answered by the package manager's own check, whose result already encodes
version comparison. The shell does not compare RPM version-release strings
itself.

### D5 — Compare the anolisa target with the running binary

anolisa and the package manager remain responsible for candidate discovery. For
anolisa results, however, the shell compares the resolved `to_version` with the
version reported by the running binary—the same value shown by `/status`. This
prevents a locally built or otherwise newer binary from displaying an upgrade
to an older managed target. The notice uses the running version as `current` and
is shown only when the target is valid SemVer and strictly newer; an
unorderable target fails silent. The RPM fallback continues to trust the package
manager's update decision and does not attempt to order RPM EVR strings itself.

### D6 — No TTL; freshness is reconciled by re-probe plus stale suppression

Because every login re-probes and only overwrites the cache on a successful
probe, the cache can only be stale in one direction: it may still advertise an
upgrade the user has already applied. Three things keep this bounded:

- The re-probe on the current login refreshes the file as soon as it completes.
- A cache entry is bound to the executable path whose managed ownership was
  verified, so a local build cannot inherit another installation's notice.
- Before showing an anolisa cache entry, the shell compares the cached "latest"
  with the version the running binary reports about itself, suppresses targets
  that are not newer, and replaces the cached `current` value before rendering.
- Before showing an RPM cache entry, the shell verifies with a bounded local
  `rpm -qf` query that the RPM package and installed epoch-version-release (EVR)
  still equal the values recorded when the package manager reported an update.
  The query is limited to 500 ms and does not order RPM EVRs.

This prevents a just-upgraded machine from showing a stale RPM notice. If local
RPM validation times out or fails, the shell suppresses the cached notice while
the background probe continues fail-quietly.
The cache records `checked_at` for probe metadata, but never reads it as a TTL
or freshness gate. A TTL was rejected because it adds a second staleness concept
without solving the only staleness that can occur here.

### D7 — Offline and timeout behavior preserves the last good state

Each external command the probe runs is time-bounded. On timeout or failure the
probe treats the attempt as "no result": it does **not** overwrite the cache and
does **not** surface anything. Probe commands run in owned process groups that
are terminated when the shell session ends; on Linux, a parent-death signal also
backstops abnormal shell termination. An offline device therefore keeps showing
its last confirmed notice (if any) and never blocks or errors. The budgets are
sized so the read-only local checks are quick and only an explicit network
refresh is allowed a longer ceiling; exact values are an implementation concern.

## Behavior

At login:

- **Cache present** → the banner shows an inline notice immediately, with no wait
  on the background probe.
- **No cache** → the banner waits only a short, bounded moment for the probe. If a
  result lands in time, it is shown inline; otherwise the login proceeds with no
  notice, and if the probe later succeeds within the session it may surface a
  single deferred notice as soon as the shell idles at an empty prompt, or at the
  next prompt boundary when the user is mid-input. Failing that, the next
  login benefits from the now-populated cache.

The notice always names the current and latest version and the **method-specific
upgrade command** — the anolisa command when detection came from anolisa, the
package-manager command when it came from the RPM fallback. It also tells users
to log in again after upgrading so the running session is replaced. The notice
is bilingual like the rest of the banner and is shown at most once per login.

The feature is on by default, is disabled by an opt-out switch, and only runs
when the startup banner itself is enabled (a session with no banner does no
probing).

## Boundaries and Constraints

- **Unmanaged binaries.** A running binary owned by neither anolisa nor a package
  (e.g. manually copied) is not detectable; the feature stays silent by design.
  No upstream-release fallback exists for this class.
- **RPM ownership is explicit.** The RPM path is enabled only when `rpm -qf`
  reports an owner for the running executable. Its package name and installed
  epoch-version-release (EVR) form the current side of the notice; an absent or
  zero epoch is omitted, matching the candidate EVR `dnf` prints. The name is
  passed to `dnf` or, when unavailable, `yum`; `check-update <package>` exit
  status `100` means an update is available and the notice suggests `sudo
  <manager> update <package>`. The shell does not order RPM EVR strings. Cache
  display repeats the local ownership and EVR check with a 500 ms bound so the
  one-time panel remains accurate; package-manager discovery remains background
  work.
- **anolisa delegated updates depend on repository configuration.** anolisa's
  dry-run for a delegated (RPM-backed) component requires the ANOLISA repository
  to be declared in its repo configuration. When an anolisa-owned executable
  yields a failed or incomplete dry-run, detection is silent; it does not fall
  through to a separate RPM probe.
- **Cross-component contract on dry-run semantics (see D4).** This feature relies
  on anolisa's dry-run reporting an upgrade plan / target version rather than a
  flipped `updated` flag. A change to that behavior in anolisa would break
  detection and must be treated as a contract change.
- **One-login stale window (see D6).** A device upgraded but not yet restarted may
  show one stale notice; this is accepted and self-heals.
- **Observability.** Debug logs record source selection, executable ownership,
  plans, and version comparisons. Warnings record command, parsing, and output
  limit failures without retaining subcommand stdout or stderr.
- **Implementation boundary.** `upgrade::check` owns read-only probing, version
  comparison, cache access, and notice construction. Runtime code schedules and
  renders the result. Future mutation-capable work belongs beside it under the
  top-level `upgrade` module, rather than under diagnostics.

## Open Questions

- **Network-refresh cooldown.** The RPM path follows the package manager's
  metadata-expiration policy and may refresh metadata during its bounded
  background check. A short cooldown after a failed refresh could reduce
  repeated offline work at the cost of slightly staler candidate data. Deferred
  pending evidence that the repeated attempts are actually a problem.
