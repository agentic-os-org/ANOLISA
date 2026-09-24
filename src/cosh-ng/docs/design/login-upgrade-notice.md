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
It intentionally does not prescribe module layout, code structure, or a task
breakdown; those belong to the implementation PR.

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
- Add **zero perceptible latency** to the login path.
- Work for both the anolisa-managed and pure-RPM install topologies.
- **Fail silent**: any timeout, missing tool, or unexpected output results in no
  message and no error — never a blocked or degraded login.

## Non-Goals

- **Auto-upgrading.** This feature only informs; it never mutates the system.
- **Implementing package candidate resolution in the shell.** anolisa and the
  package manager remain authoritative for candidate discovery. The shell only
  compares an anolisa target with its own running SemVer.
- **Guaranteeing detection for unmanaged binaries.** A locally built or
  hand-copied binary, owned by neither anolisa nor the `cosh-ng` RPM, is out of
  scope and stays silent.
- **Telemetry.** Recording whether the notice was shown or acted on is not part
  of this design.

## Design Principles

1. **The login path is synchronous and fast; detection is not on it.** Login
   only reads a local result file (an instant file read) and hands the actual
   probe to the background. The probe's latency, success, or failure never gates
   the prompt.
2. **The local result file is a cache of the last *successful* probe, with no
   TTL.** Every login re-probes. The file exists to do two things: let the very
   next login show a notice instantly, and let an offline login still show the
   last known state. It is never treated as authoritative beyond "the newest
   thing we last managed to confirm".
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

### D2 — Detect in the background, cache locally, never block login

**Decision:** spawn the probe off the login path and read only the cached result
synchronously.

**Rejected alternative — synchronous check with a timeout.** Even a short
synchronous budget is latency the user pays on a fast, online machine, and on a
slow or offline machine the login stalls for the full budget on *every* login.
Backgrounding removes login latency entirely and makes offline behavior a
non-event (fall back to cache, or stay silent).

### D3 — Prove installation ownership before probing

**Decision:** first associate the running executable with a supported install.
A raw anolisa install must run from its user- or system-layout path with the
matching CLI present. An RPM install must be owned by the `cosh-ng` package in
the RPM database. Binaries that satisfy neither condition stay silent, even if
an unrelated `anolisa` executable is available on `PATH`.

**Rationale.** Upgrade availability and the upgrade command describe an
installation, not merely a version string. Checking an unrelated managed copy
while running a local build can produce a valid but inapplicable notice. Once
ownership is established, anolisa remains authoritative for its raw installs.
For RPM-backed installs, anolisa is preferred when available; if its delegated
dry-run has a plan but omits the target version, the package manager supplies
the candidate while the notice retains the anolisa upgrade command. A pure RPM
installation uses the package-manager command.

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
- Before showing a cached notice, the shell compares the cached "latest" with
  the version the running binary reports about itself, suppresses targets that
  are not newer, and replaces the cached `current` value with the running
  version before rendering.

This makes a just-upgraded machine self-heal: at worst one login shows a stale
notice, and it disappears once the probe (or the suppression check) catches up.
A TTL was rejected because it adds a second staleness concept without solving
the only staleness that can occur here.

### D7 — Offline and timeout behavior preserves the last good state

Each external command the probe runs is time-bounded. On timeout or failure the
probe treats the attempt as "no result": it does **not** overwrite the cache and
does **not** surface anything. An offline device therefore keeps showing its last
confirmed notice (if any) and never blocks or errors. The budgets are sized so
the read-only local checks are quick and only an explicit network refresh is
allowed a longer ceiling; exact values are an implementation concern.

## Behavior

At login:

- **Cache present** → the banner shows an inline notice immediately, with no wait
  on the background probe.
- **No cache** → the banner waits only a short, bounded moment for the probe. If a
  result lands in time, it is shown inline; otherwise the login proceeds with no
  notice, and if the probe later succeeds within the session it may surface a
  single deferred notice at the next prompt boundary. Failing that, the next
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
- **RPM ownership is explicit.** The RPM path is enabled only when the database
  reports that the running executable belongs to `cosh-ng`. Other packages and
  unowned local builds are outside this feature's scope.
- **anolisa delegated updates depend on repository configuration.** anolisa's
  dry-run for a delegated (RPM-backed) component requires the ANOLISA repository
  to be declared in its repo configuration. When it is not, the anolisa path
  fails and detection falls through to the RPM path — which is the intended
  degradation, not an error.
- **Cross-component contract on dry-run semantics (see D4).** This feature relies
  on anolisa's dry-run reporting an upgrade plan / target version rather than a
  flipped `updated` flag. A change to that behavior in anolisa would break
  detection and must be treated as a contract change.
- **One-login stale window (see D6).** A device upgraded but not yet restarted may
  show one stale notice; this is accepted and self-heals.

## Open Questions

- **Network-refresh cooldown.** The RPM path follows the package manager's
  metadata-expiration policy and may refresh metadata during its bounded
  background check. A short cooldown after a failed refresh could reduce
  repeated offline work at the cost of slightly staler candidate data. Deferred
  pending evidence that the repeated attempts are actually a problem.
