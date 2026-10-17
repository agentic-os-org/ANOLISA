# Bounded approval worker in release packages

Linux release builds use the existing standalone renderer for a sole eligible
standard Bash tool approval. Host retains approval, audit, execution and input
ownership. Hook, turn-extension, multiple and unsupported cards remain inline.
Debug builds retain explicit test activation; release activation ignores the
`COSH_UI_*` test policy and worker-path environment.

The worker is discovered beside the resolved running Shell executable. It must
be a regular executable, not a symlink, owned by the same installation owner,
without set-id or group/other write bits. Its directory ancestry must be owned by
root or that owner and not group/other writable. Missing or unusable candidates
retain inline presentation. RPM installs Shell, Core and worker in the same
libexec directory; existing protocol version admission still applies.

The policy is fixed: 1 MiB frame payload, 4096 draw operations, 16 KiB text,
24 surface rows, 40 columns, five-second handshake/render/backpressure bounds,
100 ms heartbeat interval, three-second heartbeat timeout, two-second shutdown,
failure limit three and one pending revision. These are functional bounds,
not latency or SLO claims. The existing relay maintenance tick can be one second;
the three-second bound matches the validated PTY policy and lets that tick pump
heartbeats before declaring a healthy worker hung. Production does not parse debug
injection settings.

The surface stays 40 columns even in a wider terminal. Existing Gate requires
at least that physical width and enough rows, and invalidates the claim after
any resize. Narrow or untrusted geometry and exhausted worker recovery retain
conservative inline fallback. Existing ownership and fresh-input barriers apply.
The worker never receives a terminal descriptor, provider credentials or approval
callbacks: it has an empty environment, detached session, cwd `/`, null standard
descriptors and one IPC socket. Host is the only physical terminal writer.

Native Bash owns PS1, history and shell execution. Physical recovery must restore
the original prompt text, color, directory expansion and visible editable caret
before new input. An Enter-triggered redraw is insufficient evidence.
A committed handoff whose physical display failed retains its display lease
through native prompt recovery. A second physical write failure keeps input held
and retries presentation without repeating the effect or HostExecutedResult.
Only a successful prompt flush retires that lease. Because recovery may consume
held native prompt bytes before the provider finishes, the existing prompt owner
must also repaint after the provider's final output. Parser, storage and PTY
transport failures remain strict errors; this is local display recovery within
the existing bounded handoff, not a general terminal takeback interface.

Tests reuse the existing real Core/PTY harness. Its parent-only
`COSH_ACCEPT_RELEASE_PROFILE` chooses actual release artifacts and is removed
from the child environment. Original debug tests run without it. The canonical
fixture has explicit `--production` mode to remove all UI test environment.
Malformed-worker protocol negatives are labeled separately from actual worker
ELF/process identity. Final dual-architecture installed-RPM/provider acceptance
must bind to the newly frozen source and packages; old debug or package evidence
does not establish that release result.
