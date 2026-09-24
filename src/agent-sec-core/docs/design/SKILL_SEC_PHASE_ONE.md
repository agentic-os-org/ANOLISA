# SkillSec phase-one migration

[中文版](SKILL_SEC_PHASE_ONE_zh.md)

SkillSec separates Skill scanning, content authentication, version storage and activation inside
one `asc-capability-skill-sec` crate. `SkillSecService` coordinates these modules in the
system daemon. This document records the migration contract, implementation batches and Linux
acceptance boundaries.

## Delivery and acceptance

The core migration and Agent Hook integration are separate PRs. This PR covers the Rust core,
public Action Runtime audit lifecycle, daemon, CLI, SkillFS and Linux deployment. Agent Hook
implementations, capability views, Hook defaults and real Agent acceptance belong to the next PR.
Consumer request/response fixtures establish an interface contract, not successful Agent integration.
Phase-two policy integration is outside this PR.

| Batch | Responsibility | Implementation | Acceptance required before the next batch |
| --- | --- | --- | --- |
| 1 | Types, canonical identity, system keys, Integrity | Preserved; Linux revalidation pending | Signature/tamper/replay checks, key permissions, source/snapshot path rules |
| 2 | Scanner and analyze | Preserved; Linux revalidation pending | V1 result comparison, selection/aliases, incomplete coverage, errors, no Ledger writes |
| 3 | Ledger and Service | Preserved; Linux revalidation pending | Versions, fill-in/force, snapshots, export, serialization, changes during scan |
| 4 | Activation | Preserved; Linux revalidation pending | Decisions, active/pending/hidden, rollback, publish failure and startup reconcile |
| 5 | daemon, CLI and audit | Alignment implemented; Linux validation pending | Real CLI requests, outputs/exit codes, peer identity, audit, deadlines, admin rotation, consumer fixtures |
| 6 | SkillFS | Alignment implemented; Linux validation pending | One socket, authenticated notify/resolver, no downgrade, real FUSE effects, ordinary IPC regression |
| 7 | Deployment | Packaging restored; installed acceptance deferred | Source/RPM installation, root systemd service, local non-root callers, complete core workflow |

Each batch is one independently compiling logical commit with its tests and documentation.
Failures introduced by a batch are fixed in that commit. Linux tests are required; macOS formatting
or manifest inspection does not establish build or runtime acceptance.

## Shared execution and OTel alignment

The fixed main baseline is `4507cfb74ec39f6f264e8753a3eaf7715c7a83ee`. Library crates use
`v2/crates/asc-*/`. The current reconstruction retains the seven business batches, but its Linux
build, per-commit compilation and installed/FUSE acceptance must be repeated. Historical results
below describe earlier revisions only. The test machine is unavailable; machine-dependent
acceptance is deferred, and this PR remains Draft. PII integration precedes the final SkillSec rebase.

```text
CLI / RPC -> Handler -> ActionService -> ActionRuntime -> SkillSecExecutor -> SkillSecService
SkillFS notify -> authenticated queue / worker -> ActionService
Startup recovery ---------------------------> ActionService
```

`asc-action-types` owns the shared command, canonical identity and decision types. Invocation input
contains only a command and a server-injected caller UID. Physical roots, handles and keys remain
inside the capability. Handler decodes and projects responses; Executor selects and resolves roots
through a narrow `SkillEnvironment` port before Service performs the existing domain transitions.
An unavailable authenticated mapping never falls back to the visible FUSE tree. The existing
unavailable-root representation preserves per-Skill errors in aggregate operations.

The daemon composition root owns one Service, the executors, projectors, runtimes and shared
Finalizer. SkillFS lives in `asc-daemon::skillfs`, using the existing dispatcher/session ports.
Startup prepares resolver and queue resources, registers ActionService, runs recovery, starts the
worker, then opens the socket. The Executor retains resolver/health resources, not the worker bridge.
Configuration, private-state or bridge initialization and worker-start failures prevent admission.
Unfinished rotation recovery retains its fence and permits status/admin retry; individual reconcile
failures are diagnosed while other items continue.

Ordinary requests use main's top-level OTel carrier, compatibility labels and request scope.
UID/GID/PID come from kernel credentials; Agent baggage is correlation metadata, never authority.
Each dequeued notification or startup recovery item starts a fresh daemon Context. Scan, activation
and Busy retries remain separate invocations. Runtime finalizes an unexpected execution failure once
before returning `InvokeError`; entries do not emit a second terminal record. Analyze remains
Ledger-read-only while producing invocation audit. Audit, telemetry and diagnostics share the public
Finalizer, with the existing telemetry field allowlist and independent output-failure handling.

SkillFS keeps its HMAC/notify wire contract. Authentication is selected before ordinary dispatch and
retains its bounded five-second exchange; the SkillSec execution override remains 60 seconds by
default, capped at 120 seconds. Other methods retain their configured limits. Shutdown stops UDS
admission and drains requests, joins SkillFS (65 seconds) and PAP (30 seconds) in parallel, then closes
the outer runtime, durable sinks and OTel. Waiting notifications are recovered from registered Skills
on restart; this is not a durable queue or an exactly-once guarantee.

Backing mounts are cloned while detached, made private through `/proc/self/fd`, then published with
`move_mount`. This protects copies received by an already running daemon from later in-place FUSE
over-mounts. The daemon gains no mount capability. Unsupported/denied operations fail closed.
The privileged regression and real FUSE verification remain required for the reconstructed source.

## Preserved business capabilities

| Capability | V1 source oracle | Owner/batch |
| --- | --- | --- |
| Initialize, status, scanner inventory | `core/status.py`, `config.py`, `cli.py` | Service/CLI, 3 and 5 |
| Built-in scan and read-only analyze | `scanner/skill_code_scanner.py`, `scanner/builtins/cisco_static/`, `analyze.py` | Scanner, 2 |
| External findings certification | `scanner/parsers.py`, `core/certifier.py` | Scanner/Ledger, 2 and 3 |
| Signatures and file hashes | `signing/`, `models/manifest.py`, `core/file_hasher.py` | Integrity, 1 |
| Version history, fill-in, force, snapshots | `core/certifier.py`, `core/version_chain.py` | Ledger/Service, 3 |
| Check and audit, including snapshot verification | `core/checker.py`, `core/auditor.py` | Integrity/Ledger, 3 |
| Show, export, rollback, decisions and clear | `core/decision.py`, `core/exposure.py` | Ledger/Activation, 3 and 4 |
| Resolver, activation publication, background change processing | `core/live_root.py`, `core/resolver.py`, `activation_policy.py` and daemon SkillFS integration | Activation/daemon, 4 and 6 |

Paths in this table are relative to `agent-sec-cli/src/agent_sec_cli/skill_ledger/`.
V1 source supplies the behavioral oracle, never a Rust runtime fallback.
The six integrity states remain `none`, `pass`, `warn`, `deny`, `drifted` and `tampered`; execution
errors and activation state are separate. The `skill-ledger` CLI business entry and required
consumer fields, results and exit codes remain migration acceptance requirements.

## Approved compatibility changes

1. **Trust and storage:** the daemon owns one system signing key, independent of caller HOME and
   passphrases. New keys use private PKCS#8 storage, not the V1 encrypted-seed/keyring layout.
   V1 records and keys are not imported. A missing initial key can be created atomically; corrupt
   or unsafe existing keys produce an error and are never silently replaced.
2. **Manifest:** the supported signed-record format is `version: 2` and includes
   `canonicalSkillDir`. The canonical absolute source identity, rather than a leaf name or resolved
   backing-directory name, is covered by the signature. Canonical JSON recursively sorts keys,
   uses compact UTF-8, excludes `manifestHash` and `signature`, and is hashed with SHA-256. Ed25519
   signs the UTF-8 `sha256:<hex>` hash string. This is a new record contract, not a claim of V1
   byte compatibility. SkillFS protocol versions are independent and remain unchanged.
3. **Rotation:** only an administrator may rotate the system key. No old public-key fallback is
   retained, including for earlier V2 records. Rotation must withdraw old activation before trust
   is rebuilt by scanning, signing and activating again. Rotation is not implemented by the
   batch-one key initialization API.
4. **Authorization:** every local caller may operate every managed Skill. Managed-directory
   configuration bounds the managed set; it is not an ownership ACL. User isolation is an explicit
   TODO. Arbitrary bytes cannot be submitted to a generic signing endpoint.
5. **Runtime:** one root daemon is the sole writer; the Rust CLI calls the daemon and does not
   invoke Python Ledger or execute a local fallback. Agent Hooks remain unchanged in this PR.

## Integrity boundary in batch one

`SkillIdentity` validates an expanded absolute lexical path. It rejects ambiguous separators,
dot/parent traversal, NUL and non-UTF-8 paths. Physical I/O resolution remains separate so a SkillFS
live directory and its canonical source can share the same identity and later the same write lock.
Hashing requires an explicitly resolved physical root and does not implicitly follow symlink
components. Source enumeration skips `.git`, `.skill-meta`, symlinks and special files; snapshot
verification rejects those entries. Descriptor-relative opens prevent entries being replaced with
symlinks between enumeration and reading. File metadata is checked across hashing. This alone does
not prove a consistent scan: batch three must scan and snapshot the same staged content, then check
the live tree again before committing.

The signing directory must already exist, be service-owned and inaccessible to other users.
Keys must be private regular files with one link; loading rejects symlinks, hard links, invalid
owners, oversized input and invalid PKCS#8. Initialization uses an exclusive temporary file, file
sync, no-replace rename and directory sync, so racing first-time initialization does not replace
the winner's key. Signature verification checks record invariants, canonical identity, hash,
algorithm, current fingerprint and Ed25519 signature before trusting file hashes or decisions.

The batch-one integration suite is
`v2/crates/asc-capability-skill-sec/tests/integrity.rs`.
Its `fixtures/integrity.json` uses a public synthetic seed (bytes 0 through 31), Python standard
JSON/SHA-256 canonicalization and OpenSSL Ed25519 signing. The Rust test must verify that external
signature and reproduce the same hash/signature, including recursive metadata ordering and Unicode.
Other cases cover changed signed fields, cross-Skill and cross-key replay, unsafe keys, concurrent
initialization, ambiguous identities and source/snapshot entry handling.

## Planned orchestration and recovery

Service owns per-Skill serialization from reading old state through publication. Scanner supplies
findings; Integrity authenticates content and records; Ledger stores versions/snapshots and exports;
Activation selects and publishes the exposure. Transport owns authenticated caller identity and
protocol parsing; it does not duplicate domain transitions. Public Runtime/Finalizer/Sink receives
an explicit safe audit projection separate from the full business result.

Persistence uses single-file atomic replacement, rollback backups and startup reconciliation.
Committed Ledger state, selected activation and observed SkillFS effect remain distinct results.
No multi-file transaction engine, durable job queue or exactly-once promise is introduced.

SkillFS will use the public V2 socket. Its endpoint permission checks will accept the system socket
layout while preserving trusted ownership, actual peer identity and HMAC secret checks. First-frame
dispatch will distinguish ordinary V2 requests from the existing HMAC exchange. Only the legacy
notify contract is adapted; other V1 RPC methods are not enabled by that adapter.

For `init`, `scan` and `certify`, Service owns argument validation and the subsequent key operation.
Executor delegates these commands through the public runtime; invalid scanner names, and invalid
findings for certification, fail before key creation or rotation. Public Service and standalone
Scanner entries validate their inputs. Internal per-Skill scans consume explicit selections without
repeating name validation, including when a baseline processes multiple Skills.

Existing-key initialization takes a shared generation lock. Only a missing key requires releasing
that lock, acquiring the exclusive lock and checking again before creation. Rotation retains its
exclusive boundary; per-Skill mutations retain their own lock. Key permissions, rotation state and
content identity are still checked at use time.

## Rollback boundary

Batch one has no daemon registration, installation change or live migration side effect. Reverting
the crate and workspace registration removes it. For the completed migration, retain V1 state
separately when replacing deployment; V1 cannot consume V2 manifests or system keys. A deployment
rollback must restore its matching state and configuration, never mix both writers on one Ledger.

## Scanner boundary in batch two

`ScannerRegistry` runs `code-scanner` and `static-scanner` in process, in that order. Code Scan
reuses the V2 capability for Python, shell and recognized suffixless shebangs. Static Scan embeds
the existing ten rules and checks metadata, links, hidden/credential-like files, binary assets and
undeclared networking. A symlink contributes a finding; its target bytes are never scanned.
Registered custom scanners remain import-only, as in V1; `cli`/`api` metadata never executes a
caller-supplied command in the root daemon. Findings-array import retains unknown evidence fields
and returns visible normalization warnings. Retired scanner names are rejected on new input.

`analyze` is independent of keys and managed registration and never writes a Ledger. Complete
`pass`/`warn`/`deny` analyses return exit 0. Incomplete coverage returns `error` and exit 1; invalid
root/manifest inputs return exit 2. A consumed deadline is an execution timeout. Physical source,
HOME and configured temporary/XDG roots are redacted from nested analysis evidence. The later CLI
adapter must expand user paths before sending its absolute request.

The V1 analyze limits (2,000 regular files, 50 MiB total, directory depth 32) also bound built-in
Ledger scans; exceeding them fails the scan rather than certifying a partial inventory. Per-file
limits remain 1 MiB for Code Scan and configurable `maxFileBytes` (default 1,000,000) for Static
Scan. Metadata decoding uses the existing YAML tokenizer, preserves V1 unquoted booleans,
duplicate-key replacement and ordinary anchors/merges, and rejects recursive or excessive metadata
(depth 32, 10,000 expanded nodes, 8 MiB scalar content). These resource limits protect the shared
daemon. Scanner diagnostic wording may change with the implementation language; rule identifiers,
risk levels, evidence and coverage outcomes remain the tested business contract.

Inventory stops on the first limit and retains at most 20,000 enumerated names across the tree,
including directories, links and special files. Excluded directories count as one name and are not
traversed. Limit metadata reports the observed prefix with `truncated: true`, not a full-tree total.
Analyze checks for a regular `SKILL.md` before traversal on the same directory descriptor; signing,
content comparisons and rollback reject incomplete inventories. Quoted or explicitly string-tagged
YAML `<<` keys remain ordinary keys, including aliases to those keys; only merge keys combine maps.

`tests/reference_scanners.py` freezes V1 results with source revision and SHA-256 file hashes into
`tests/fixtures/scanners.json`. Its 50 cases compare both built-ins and analyze, including false
positive suppression, Unicode, metadata, symlinks, excluded directories and incomplete coverage.
Only elapsed time, engine version and language/platform diagnostic wording are normalized; risk
results and evidence are compared. Rust tests also cover scanner selection, disabled/import-only
entries, parser fallback, aliases, invalid input, resource limits, deadlines and absence of state
writes. Fixture generation is a developer tool; the deployed Rust binary never invokes Python.


## Ledger and Service boundary in batch three

`SkillSecService` owns one lock per canonical Skill identity, shared by direct and resolved
paths. Unused lock entries are discarded. A generation read lock protects operations against
system key replacement; rotation will take the write side in batch five. Registration stores
exact roots in private daemon state and never discovers siblings from a user-selected parent.
Business roots cannot be inside `.skill-meta`, including authenticated physical mappings; internal
snapshot verification does not use this business-root entry point.

Scan captures at most 2,000 regular files / 50 MiB / 10,000 directories / depth 32, excluding `.git` and `.skill-meta`.
It scans a private staging tree, retains original symlink classifications for static findings,
and rechecks live bytes, ordinary executable bits, directories, links and root identity after
the temporary snapshot is written and verified, immediately before publication. Snapshots retain empty directories and strip setuid/setgid bits. A new snapshot is
published before its signed version record and `latest.json`; each file uses an exclusive temporary
file, fsync and atomic rename. An interrupted multi-file publication is detectable. Reconciliation
and rollback orchestration are delivered in batch four, not claimed by this batch.

Unchanged content reuses only a fully authenticated latest/version/snapshot tuple. Fill-in adds
missing scanners; force replaces scanner results on the same version. Drift or tampering creates
a new version linked to the newest fully verified predecessor. Both JSON and snapshot names reserve
version slots, while an unauthenticated high number cannot force a numbering jump. `check` compares
live hashes against the newest authenticated record without requiring snapshots. `audit` verifies
parent signatures and optionally snapshots; unauthenticated records cannot supply public metadata.
Without any Ledger artifacts, `check` returns `none` and an empty `audit` succeeds even before key
initialization. These read-only queries do not create a key or Ledger; existing artifacts still
require authentication.

Registration follows the signed commit and precedes activation. If the first registration fails,
the request fails without publishing activation, although its version may already be committed.
Startup recovery only enumerates registered roots. After correcting the reported failure, retrying
`scan` on unchanged content reuses the authenticated version and completes registration. No durable
discovery queue is promised for an unacknowledged first request.

Export reads an authenticated snapshot and writes `snapshot/`, `manifest.json` and `findings.json`.
The caller must first create an empty, caller-owned output directory outside Skill/state roots;
peer UID, directory type and write permissions are checked by the service. No destination parent
is created as root, and no symlink or existing destination file is followed or truncated. Newly
created export files and directories are assigned to the authenticated caller so they can edit and
remove the export. Snapshot and ledger storage remain daemon-owned. The
`active` selector and rollback decision flow are added with Activation in batch four.

`tests/reference_ledger.py` records ten V1 workflows with source hashes. The Rust tests compare
business statuses, version IDs, scanner merging, file counts, drift lists and audit verdicts.
Keys, manifest format and signatures intentionally differ between V1 and V2. Additional tests
cover parallel certification, alias serialization, deadline expiry, staged-content mutation,
missing/forged artifacts, safe export and exact registration. No daemon or Hook interface is
registered by this batch.


## Activation boundary in batch four

Service now supplies `decide`, `clear_decision`, `show`, `activate` and `rollback`. Scan and certify
publish activation before releasing the same Skill lock. `allow`, `always_allow`, `block` and
`rollback` retain V1 selection rules; only `always_allow` inherits into a new content version.
`active` exports the selected authenticated snapshot. `show` is read-only and keeps latest/active,
source consistency, findings and bounded review messages separate.

Publication writes the minimal schema-1 `activation.json` and directory xattr consumed by SkillFS.
It exposes a verified snapshot, a safe pending-review stub, or a null target for an explicit block.
`contractWritten`, `activationXattr.written` and `activationPending` distinguish committed business
state from incomplete publication. An xattr failure never undoes a signed decision; activation or
startup reconcile retries it. This is publication evidence, not proof of an observed FUSE effect.

Rollback scans a captured trusted snapshot, backs up the current tree, then records a private
per-Skill recovery intent before replacing source content. The backup retains nested metadata directories and link text without
following targets; special files and excessive trees fail before replacement. Signed snapshots
still exclude links and privileged executable bits. A prepared intent does not undo later edits.
After replacement begins, failure before the matching signed version restores the backup; after
the version is committed, reconcile repairs latest without undoing that commit. Recovery verifies
backup hashes/link text and refuses damaged backups. Backups are retained for explicit inspection.
Startup reconciliation also repairs authenticated version/latest splits only with a valid snapshot,
removes abandoned internal temporary entries, and republishes selection. There is no automatic
history retention policy or generic transaction engine.

`tests/reference_activation.py` freezes twelve source-pinned V1 workflows. The Rust suite compares
selection, manual decisions, fallback, drift, rollback, export and show explanations. Linux-specific
tests cover actual xattr bytes, file/xattr split failure, rollback commit failure, interrupted source
replacement (including missing SKILL.md), damaged backups and committed-intent recovery. The daemon
startup loop and real SkillFS consumer are integrated in subsequent batches.

## Daemon, CLI and audit boundary in batch five

`action.skill_sec` accepts a closed `command` enum. This is the approved V2 replacement for the
previous candidate `action.skill_ledger`; it does not enable generic action dispatch or legacy V1
RPC envelopes. Results contain `success`, `exitCode`, `error`, `errorType` and the business `data`.
The Rust `skill-ledger` CLI prints the business object and uses the explicit exit code. It never
reads the daemon key, executes Python, or scans locally. Client-side path expansion, findings-file
reading/deletion and export-directory creation run with the caller's own permissions.

Supported commands are `init`, `check`, `analyze`, `scan`, `certify`, `status`, `audit`,
`list-scanners`, `decide`, `show`, `export`, `rotate-keys`, and publication retry `activate`.
`init --no-baseline` creates only keys; `init --force-keys` and `rotate-keys` require kernel UID 0.
The retired `init-keys` and per-user `--passphrase` are not part of the system-key contract.
`scan`, `certify` and `decide` create the initial key when absent, but never replace a corrupt current key.
`check` returns exit 1 for deny/tampered/error; scan/certify and complete analyze return exit 0 for
completed risk results. An incomplete analyze returns 1 and invalid analyze input returns 2.
A committed scan/decision can return exit 0 with `activation.activationPending=true`; callers must
inspect that field before claiming publication or a live FUSE effect.

The process uses `/run/agent-sec-core/daemon.sock`, overridden by `--socket` or a nonempty
`AGENT_SEC_DAEMON_SOCKET`. A service-owned 0700/0750/0755 runtime directory and private flock file
protect singleton/stale-socket handling; only a verified owned socket returning connection refused
is removable. The runnable process uses mode 0666 while embedded service defaults remain 0600.
These focused lifecycle changes align with the open system-service proposal #3217 at `5d2ff1f`;
they do not imply that proposal has merged or that its service identity is adopted.

Root-owned `/etc/agent-sec/skillsec.json` (or `--skillsec-config`) configures `stateDir`, exact
`managedSkillDirs`, scanner overrides and parsers. The default state is
`/var/lib/agent-sec/skillsec`, owned by root with mode 0700; the current key remains 0600.
No user config, history or keyring is imported. For `init` baseline and `check/scan --all`, the CLI
contributes exact roots discovered in the current user's default Skill locations and the two system
Skill directories; the daemon combines these with its registered roots. Empty `check/scan --all` returns an execution failure without creating keys. Caller discovery is
bounded to 1024 roots; persisted registration remains available for status and key rotation.
No sibling registration is
inferred from a single explicit path. Status reports the system registry, not the invoking HOME.
The shared CLI discovery includes direct Skill children of `$XDG_DATA_HOME/anolisa/skills`.
It follows the installer's syntax rules: unset/empty/relative overrides or raw `.`/`..` segments
fall back to `$HOME/.local/share/anolisa/skills`; a valid but absent directory is simply skipped.
An unset or empty `HOME` resolves through the CLI user's system account home directory.
Only the CLI reads this caller environment. `init --no-baseline` and explicit-path requests bypass
discovery, and hidden/snapshot directory filtering remains in effect.

Rotation takes the service generation write lock, records a private intent, and withdraws every
registered exposure before replacing the key. A pending rollback must first reconcile. A failed
withdrawal retains the old key and fences ordinary Ledger operations until administrator retry or
startup recovery succeeds. A changed fingerprint during recovery proves replacement already
committed and permits intent cleanup without resolving mappings or rotating again. The intent stores
only the previous fingerprint and canonical Skill identities. Startup, `rotate-keys`, and
`init --force-keys` resolve current physical mappings and inodes again before withdrawal; the service
requires exactly the recorded Skill set. Resolver failure retains the intent and old key for retry.
Startup recovery uses the public Action Runtime; failed
Skill recovery is visible without disabling unrelated daemon methods.

The public Finalizer/Sink receives controlled command, counts, verdict/status, version and execution
failure class. `result.verdict` retains the V1 command-specific verdict and worst-result batch
projection; non-judgment operations do not manufacture a security verdict. It excludes raw findings, code, imported evidence, manual reasons, paths and key
bytes. Full business results remain available to the client. Two concurrent SkillSec requests
bound content-capture memory; busy returns a visible failure. SkillSec defaults to 60 seconds,
with `timeoutMs`/CLI `--timeout-ms` bounded to 120 seconds on the server. Other methods retain their
configured dispatch deadline. Requests are never retried automatically. A response above 3 MiB
returns `ResponseTooLarge` with `operationMayHaveCommitted=true`; no data is silently truncated and
no committed mutation is undone. Findings import is bounded to 2 MiB.

`v2/fixtures/skillsec/consumer.json` records normal, risk, uninitialized, timeout, execution-error
and incomplete-activation examples. CLI rendering tests consume these examples; runtime and real
CLI tests independently exercise execution, caller identity, rotation and safe audit. They do not
establish Agent Hook integration or SkillFS effects. Historical batch-five acceptance (before the current alignment) passed strict
workspace Clippy, all workspace tests and rustdoc. The cross-UID CLI workflow runs with normal
root DAC permissions; other permission-sensitive cases retain reduced DAC. A separate daemon
binary and CLI completed 25 operations, including restart, rotation, ordinary-user export, PAP,
Code Scan and safe public audit.

A rollback performed by the root daemon restores regular files and directories to the source
Skill directory owner, so its ordinary user can continue editing. Snapshot privilege bits remain
stripped. The cross-UID CLI workflow verifies rollback followed by a real user write.

## Batch six: SkillFS boundary

`asc-daemon::skillfs` owns the compatibility adapter. The generic socket service only adds
an optional connection-local session port and retains buffered bytes between frames. The normal
V2 envelope remains closed to unknown fields. Authenticated sessions accept only
`skill_ledger.skillfs_notify_change`; they cannot dispatch PAP or arbitrary V1 methods.

The existing four-frame HMAC handshake, string `authVersion="1"`, separate client/server domains,
raw payload plus `auth.frame` tag, and notify schema version 2 are retained. The daemon bounds a
session to four incoming frames, 64 KiB per frame and five seconds after the initial frame. Invalid
proofs close the connection without plaintext fallback. A verified business response is signed,
including rejected notifications. `accepted`, `queued` and `coalesced` describe in-memory work;
metadata-only `.skill-meta` notifications are acknowledged with `ignored=true`.
Coalesced work retains `queued=true`. The V1 `skill` summary, metadata-only `reason`,
daemon-generated `request_id`, `stdout/stderr/exit_code` and structured error fields are retained.
An unexpected worker exit closes admission and reports unhealthy status until daemon restart.

The authenticated SkillFS client accepts its existing private endpoint or a root-owned parent
mode `0755` plus socket mode `0666`. It verifies ancestor ownership, symlinks, writable parents,
endpoint type/owner and the connected kernel UID before the HMAC handshake. The HMAC key remains
an owner-only `0600` regular file. Public socket access does not grant administrative key rotation
or change PAP authorization. All local users may operate managed Skills under the phase-one
access contract.

The root-owned `--skillsec-config` file accepts the optional binding below. Paths must be
absolute and normalized. One mount's canonical/live roots may be identical for an ordinary
non-in-place mount; otherwise they must be disjoint. Prefixes belonging to different mounts cannot
overlap. The daemon and each SkillFS
process receive private copies of the same raw HMAC secret. This secret is separate from
`stateDir/signing-key.pk8`; neither signing-key rotation nor user HOME selects the HMAC key.

```json
{
  "stateDir": "/var/lib/agent-sec/skillsec",
  "managedSkillDirs": ["/home/alice/.openclaw/skills/demo"],
  "skillfs": {
    "authKeyFile": "/etc/agent-sec/skillfs-hmac.key",
    "mounts": [{
      "controlSocket": "/run/user/1000/skillfs/control.sock",
      "canonicalRoot": "/home/alice/.openclaw/skills",
      "liveRoot": "/home/alice/.openclaw/live-skills",
      "peerUid": 1000
    }]
  }
}
```

The control endpoint stays private: configured peer UID, owner-only parent/socket, kernel peer
verification and the existing control HMAC domains. Each read-only `skill.resolveLiveSource`
response must match both configured prefixes, the exact relative Skill ID and `shared_path`.
Source/live aliases map to one canonical lock identity. The returned device/inode is checked when
opening the backing directory and again inside the service boundary. Failed resolution inside a
configured mount never falls back to reading the FUSE view. Aggregate commands retain per-Skill
mapping failures; `status` still returns key readiness. Rotation persists physical identity pins
in its private recovery intent and refuses unresolved mappings before starting replacement.

One worker coalesces notifications per canonical Skill with a 500 ms debounce and a two-second
maximum delay under continuous edits. It invokes public Action Runtime scan, then activation even
if scan fails or is a no-op. Only `Busy`, which proves execution was not admitted, is retried
within the execution deadline; unknown post-commit failures are not replayed. Each operation has a
30-second budget. Failures remain visible in the audit, stderr and `status`'s `skillfs` counters.
Shutdown stops queue admission and waits for the current pair of bounded operations.

After synchronous rollback/latest/activation recovery, daemon startup schedules all explicitly
registered mount Skills for a fresh scan and activation. This does not require SkillFS to restart.
The startup list comes from the bounded private registry; live notifications allow up to 256
pending distinct Skills, and queue overflow returns a signed rejection. This is an explicit Rust
resource bound beyond the unbounded Python pending map; it is not durable acceptance. Mount
startup notifications cover newly discovered Skills. There is no persistent event queue or replay
of original notification order.

Batch-six tests cover frozen SkillFS HMAC vectors, coalesced wire frames, wrong keys and payload
MACs, plaintext rejection, source/live identity, false resolver mappings, replaced backing
inodes, daemon startup rescan, activation after scan errors and normal V2 calls. The synthetic
resolver tests establish the IPC contract. Historical acceptance (before the current alignment) additionally passed V2 workspace gates,
SkillFS workspace tests with targeted retries under the existing tests' environment assumptions,
and the repository's real FUSE smoke. The root in-place mount and UID-1001 ordinary mount each
completed 25 real daemon/SkillFS operations, including authenticated notify/resolver, publication,
invalid identity/key/plaintext rejection and daemon-only restart recovery. SkillFS Clippy used the
repository's pinned Rust 1.86; V2 used Rust 1.93.1. These are core/FUSE results, not Agent Hook or
installed-systemd acceptance.

## Batch seven: deployment boundary

V2 `install-core-v2` installs the Rust binaries, root system unit and initial private configuration.
The V2 RPM shares the binary and system-unit installation targets and uses systemd system-service
scriptlets. V1 retains its original user unit. Neither source installation nor the unit enables
Agent Hooks or imports V1 state. Existing operator settings survive source reinstallation and RPM
upgrade (`%config(noreplace)`). The signing key is created by a business operation, not installation.

The system unit owns `/run/agent-sec-core` (0755), `/var/lib/agent-sec/skillsec` (0700) and
`/var/log/agent-sec` (0700), with umask 0077. It runs as root with only DAC override, CHOWN and
FOWNER capabilities, `NoNewPrivileges`, `SystemCallFilter=@system-service`, native syscall
architecture, `MemoryDenyWriteExecute` and kernel protections. HOME, `/tmp`, system Skill roots
and shared mounts remain accessible because these are supported content locations. The daemon
does not receive SYS_ADMIN. A systemd test container's separate namespace-management capability
is a test-runtime requirement, not part of the product service's capability set.

The V2 CLI RPM no longer depends on Python, GPG or loongshield; unchanged Hook packages retain
their own dependencies. The full repository RPM recipe still builds those plugin packages and
the sandbox. Its OpenClaw build dependency requires Node.js 22.14 or later; V2 CI uses Node 22.
`V2_CARGO_TARGET_DIR` allows a task-owned cache while preserving the real release-build path.

`tests/packaging/test-skillsec-install.sh` checks real built binaries in a temporary DESTDIR,
configuration preservation, absence of automatic activation/key creation and V1 unit isolation.
The installed Python V2 E2E fixtures isolate daemon state/audit and exercise ordinary-UID PAP
denial against a root process. These checks remain distinct from actual systemd lifecycle and
real FUSE evidence. The
[core guide](../../../../docs/user-guide/en/agent-security/agent-sec-core/skillsec-v2.md) gives
source/RPM commands, shared-volume requirements and state-matched upgrade/rollback instructions.

Historical delivery acceptance (before the current alignment) passed on Alibaba Cloud Linux 4, x86_64. After alignment with the upstream system daemon, the
source-installed suite passed 914 tests, with 38 skips including the unavailable system manager.
The RPM suite passed 914 tests with 37 skips; its corrected systemd lifecycle case passed separately,
for 915 unique installed cases. Both exclude two real-model cases. The other skips concern
source-only rule inventory/metadata and telemetry;
SkillSec, PAP and daemon lifecycle cases ran. Python Ledger was unavailable to these suites.
The repository recipe produced the RPMs, and DNF installed the core and Skill resources with
normal dependency checks. This is repository-built artifact evidence, not a GitHub CI result.

The unchanged product unit completed 45 operations under actual PID 1 systemd 255. Its effective
and bounding capabilities were exactly CHOWN, DAC_OVERRIDE and FOWNER, with `NoNewPrivileges`.
UID 1001 managed private HOME, `/tmp`, system and shared-volume Skills; rollback remained editable,
rotation stayed root-only, restart retained trust, and public audit omitted sensitive details.
Twelve package lifecycle checks passed: reinstallation retained configuration, removal saved
modified configuration and retained the private key, and restoration preserved trust and package
verification. This verifies V2 package recovery, not a V1 downgrade or Agent Hook integration.

The system-manager fixture also verifies the shipped 75-second forced-stop deadline and startup
rate limit. It uses the selected executable in place because `/run` may be noexec, injects a
non-terminating stop signal only in the isolated test unit, and checks actual admission rejection
instead of a distribution-specific `Result` string. Earlier failed fixture attempts remain recorded.
