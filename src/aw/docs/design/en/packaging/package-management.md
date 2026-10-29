# Local package management

[中文版](../../zh/packaging/package-management.md)

`aw-cli` calls the `aw-package` library to
plan and commit local package changes. The implementation is in `database.rs`,
`open_bundle.rs`, `plan.rs` and `transaction.rs` and their matching directories.
The CLI remains the only software-management executable; `aw-build` produces
the existing Preview bundles.

## Files and authority

User records live at `$XDG_STATE_HOME/aw/packages`, falling back to
`$HOME/.local/state/aw/packages`. An explicit system scope uses
`/var/lib/aw/packages`; it manages raw AW files and does not invoke RPM or dpkg.
The process's effective UID never selects a scope implicitly. System writes
require root. System record and lock files are readable by ordinary users;
the `transactions/` root is enumerable while individual journal directories
remain private to root. User databases and all recovery journals stay private.

| Path below the database | Purpose |
| --- | --- |
| `installed.json` | Versioned snapshot, commit generation, prefix IDs, packages, files and exact dependencies |
| `lock` | Existing shared query/configuration lock or exclusive transaction lock |
| `transactions/<transaction_id>/` | Durable journal and database publication evidence for interrupted operations |
| `legacy/<installation_id>/<transaction_id>/` | Exact historical JSON bytes retained for each registration attempt |

The sole prefix-local `.aw-packages/aw-core.json` records a database path, scope
and installation ID. It contains no second copy of the package facts. Its
strict shape is rejected by the historical `Component` decoder before a legacy
writer changes its payload. Queries cross-check this locator against the
database and reject mixed locator/component records. Central listings retain
records when a payload prefix is missing; `doctor` reports the file damage.
An empty registration and locator survive complete removal to keep historical
writers closed and allow later reinstall at that prefix.

No-prefix queries use only the selected database. Explicit-prefix queries and
configuration generation follow the locator, allowing an ordinary user's
configuration to refer to a readable system installation without guessing root's
XDG directory. Old prefixes remain readable without a locator. Querying them
does not register them. Each migration attempt archives their raw bytes separately;
only a committed mutation establishes the locator's database authority. Archives
from rolled-back attempts remain readable without blocking a later migration of
changed old records. Opaque old version
and source labels remain recorded facts rather than being rewritten as SemVer
or fabricated Git commits.

## Local inputs and plans

`open_bundle::inspect` accepts exactly one bounded `package.json` or historical
`manifest.json`, then verifies every described payload under `payload/` using
no-follow regular-file reads, modes and digests. Independent descriptions use
SemVer, Linux architecture, exact requirements and optional source/protocol
metadata. Ordinary tools require neither a Provider declaration nor AW core.
Historical Preview bundles retain same-build dependency checks.
Individual files are bounded at 1 GiB. Bundle inspection retains its actual
verified byte count, and install planning combines that count with the actual
verified bytes already in the selected prefix. The resulting installation must
remain within 4 GiB, matching the complete-prefix verification budget used by
future plans and removals. Separately bounded inputs cannot bypass that limit
through repeated appends.

Admission also caps the resulting prefix at 32,767 payload files, reserving one
of the journal's 32,768 actions for its locator. Plans check the current operation's
exact payload/legacy/locator actions and newly created directories against the
same journal limits. A pre-existing oversized installation can remove selected
packages to reduce its size; an oversized whole-prefix removal is blocked during
planning rather than first failing during execution.

Plans also enforce the journal's 96 MiB encoded-byte ceiling using the actual
`Journal`/`Action` serializer and conservative identity and transaction-ID sizes.
The budget includes every possible payload parent, including existing empty
directories that might disappear later. Database snapshots remain exact UTF-8
strings; they are not written as JSON byte arrays. Installation admission reserves
whole-prefix removal space for two fully grown 16 MiB database snapshots and
their string escaping, so later additions in other prefixes cannot exhaust that
reserve. Preparation repeats the check under write locks before creating the
journal or changing the prefix; a valid empty database may already exist.

`plan::install` only adds packages. It checks exact dependencies in the selected
prefix, existing package IDs, unknown destination files, and ownership collisions
throughout the selected database. `plan::uninstall` accepts selected IDs or all
packages, checks the remaining database's reverse dependencies and verifies the
owned files before choosing removals. It never includes unknown files. Neither
function creates the prefix, database, lock or journal. Plans retain private
before/after evidence; caller-edited JSON cannot authorize execution. Replacement,
upgrades, remote fetching and cross-prefix dependency selection are separate work.

## Locks and commit

Queries retain the existing database shared lock. Configuration generation takes
the database shared lock before the prefix shared lock, then holds both through
file verification and private configuration publication. It reads historical
records only when no locator exists and rechecks that choice after taking the
prefix lock. Configuration never enables a newly installed Provider implicitly.

Execution takes database then prefix exclusive locks without waiting and checks
that the planned generation, metadata bytes, source payloads, installed files
and record directory still match. A missing database is initialized to a durable
empty generation zero while locked; a failed first write can leave this valid
empty database. Each successful operation advances the generation exactly once.
Prefix metadata and package files are staged locally, with durable journal intent
before publication. The historical-writer locator is published before changing
legacy payloads and cannot disappear during its atomic replacement.

The database and prefix filesystems must support Linux `O_TMPFILE`, hard links,
directory fsync and `renameat2` with `RENAME_NOREPLACE` and `RENAME_EXCHANGE`.
They may reside on different filesystems; each stages its own files locally.
Unsupported staging reports the operation and directory. There is no named-file
fallback: supporting one requires journaled temporary-file ownership and crash
recovery for partially written files.

Replacing `installed.json` durably is the commit point. Before it, recovery
restores previous files and restores the old-writer locator last; after it,
recovery completes cleanup rather than rolling back the committed database.
Replay checks recorded inode, mode and content evidence. Conflicting or unknown
bytes remain on disk with their backups and are reported for inspection. Queries
and normal writes refuse any unfinished transaction; only explicit `aw recover`
replays it. SIGINT/SIGTERM cancellation uses the same recovery path. SIGKILL
can leave a journal that the next recovery replays.

A visible rename does not establish durability when the directory fsync fails.
That error leaves the journal and backups intact and reports incomplete recovery.
Explicit recovery repeats the database publication durability barrier before
treating visible after-bytes as a committed database and cleaning up.

## Runtime file references

`aw-exec::PackageLeases` holds existing prefix shared locks for managed
executable paths and literal absolute argument paths. It reads path ancestors,
not the package database. The CLI protects its executable and known Agent
launch commands, the service protects its own executable, and each Host protects
its Provider commands until its owned processes have stopped. This placement
avoids a runtime-to-package-library dependency and keeps the Hook call path
unchanged. An exclusive writer fails while one of those references remains.

These locks coordinate this implementation. They cannot establish that arbitrary
external processes, shell-generated paths, or older AW runtimes are absent.
If the recorded AW executable's digest differs from the current command,
destructive plans and first-write legacy registration report `runtime_unknown`.
Rejecting the latter before writes keeps old installations manageable by their
existing tools; install a different AW version in a new prefix. The CLI also
holds package leases while registering native Agent Hooks through either
`aw agent connect` or legacy native `aw install --config`.
Complete instance discovery and lifecycle management remain outside this package
database.

## Verification

`tests/management.rs` covers independent tools, preview without writes, exact
dependencies, reverse references, ownership, stale evidence, legacy archival,
last-package removal/reinstall and configuration from registered packages.
`tests/database.rs` covers bounded inventory/doctor reads and locator authority.
Configuration unit tests verify both read locks remain held during publication;
transaction unit tests exercise interruption and recovery. Runtime lease tests
live with `aw-exec` and `aw-host`. Run the Linux checks in
[CONTRIBUTING](../../../../CONTRIBUTING.md); unit fixtures do not certify native Agent
adoption or general external-process discovery.
