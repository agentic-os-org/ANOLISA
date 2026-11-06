# Observation Source and AW correlation

The default executable refuses observation before target access because forced-death
isolation remains unproven. Only explicit `unsafe-lab` builds enable collection
for disposable synthetic targets. This admission gate is the current fail-closed
mechanism; it does not contain a running lab tracer crash or terminate targets
as an availability policy. No EXITKILL, global security change or target-side
SIGTRAP handler is installed.

The experimental Source outputs `aw.transport-observation-poc/v0` JSONL. It has
no AW dependency, socket, registration credentials or policy controls. Its
ProcessRef includes PID/start ticks/UID/PID namespace/boot identity/executable
identity. The permit adds an executable SHA-256, while the running tracer retains
a pidfd and rechecks process identity at event handling and idle boundaries.

The Source stops existing threads, resolves exact dynamic OpenSSL symbols, and
arms per-thread x86 execution breakpoints. TRACECLONE provides a stop before a
new thread runs; Linux may expose inherited DR7 with empty DR0/DR1, which is
validated and initialized. Stops already available are drained into a bounded
queue before resuming threads to avoid low-TID starvation. Pending stops remain
owned by the Engine so cancellation and error cleanup can detach them. Exec and
fork cause an explicit unsupported result and bounded detach. Foreign signals
are forwarded after checking signal provenance. SEIZE group stops use LISTEN
until SIGCONT, and detach preserves the group stop. Four-wave thread churn and
busy attachment have dedicated synthetic tests; arbitrary dlclose/reload and
unconstrained real application behavior remain unverified. Cleanup responsibility
is registered before the first debug-register write, inherited by tracked
children and retained until detach or verified loss of ownership. An exiting
thread returning ESRCH does not abort cleanup of other threads. Each cleanup
attempt has a three-second deadline; unresolved ownership reports failure.
An interrupt stop can hide a queued hardware SIGTRAP. Cleanup disables breakpoints,
uses PTRACE_PEEKSIGINFO to inspect the bounded per-thread queue, then resumes under
ptrace to consume that signal's delivery stop before detaching. The delivered
signal is suppressed only with hardware provenance and a matching owned symbol
address; register-read failure retains cleanup responsibility. This recovery
requires a live tracer and cannot execute after its SIGKILL.

TLS writes produce counts, lengths and SHA-256 only. Events use Linux
CLOCK_MONOTONIC nanoseconds and boot identity, with explicit `unknown` coverage.
The event means entry to a TLS write API; it cannot prove successful transmission,
parse a complete request, identify a tool call, or authorize an action. Only the
controlled receiver validates byte adoption in these tests. A buffer shared with
concurrently mutating application threads has no atomic-snapshot guarantee.

## Fixture correlation boundary

`fixtures/aw_owner.py` calls the real `aw instance` CLI as the direct parent.
It registers the existing Qoder SDK profile with **empty** capabilities/routes,
acknowledges Ready for that empty route set, opens and acknowledges a real Scope,
and records CLI replies plus the service's parent/scope status in a private file.
It then execs the synthetic TLS client, preserving AW PID/start identity.

`scripts/correlation.py` runs outside the Source. It imports that test-owned file,
checks owner PID/start identity, service generation/config identity, exact handles,
scope-parent status and liveness, then pins the client's final executable. It
correlates events while the process remains live and the capture's 60-second
window remains valid. An unregistered process stays observed/unassociated.
Multiple candidate instances for one PID remain ambiguous with null IDs; no
timestamp heuristic chooses a task. This importer trusts its same-UID fixture
capture and lacks a live release/revocation feed. It is not a production AW
attestation format. Descendant worker mapping is future work; the demonstrated
worker uses the owner's preserved PID.

## Minimum future service integration — proposed, unimplemented

1. Define a read-only, generation-bound identity export for an authorized
   Observation Source. Include instance/scope validity intervals, process/boot
   identity, ambiguity and revocation. The Source receives no full AW socket.
2. Keep a separately supervised Source Worker and a bounded private metadata
   store. Resolve tracer-death safety before enabling it for user processes.
   Source lifecycle, privilege and resource ownership stay outside the AW daemon.
3. Add an independently distributed `aw-provider-observation` query/projection
   bridge. A normal stdio invocation reads bounded snapshots from that store;
   aw-host continues to manage only per-invocation Provider execution.
4. Review provenance, observation intervals, drops and semantic coverage before
   mapping to formal AW evidence. Context Freeze (#9425) may consume reviewed
   facts later; semantic Provider evaluation and Directive (#9427) authorization
   remain distinct work. Transport events never become trusted native callbacks.

Native Hook/SDK remains the preferred integration. Optional AgentSight resources
may later supply process discovery, reviewed fingerprint assets or trajectory
data. Neither AgentSight nor eBPF is a runtime dependency of this POC. Formal
protocols, schema registration, packaging and installers require a separate review.
