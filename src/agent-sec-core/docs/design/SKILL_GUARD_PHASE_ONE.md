# SkillGuard phase-one migration

[中文版](SKILL_GUARD_PHASE_ONE_zh.md)

SkillGuard separates Skill scanning, content authentication, version storage and activation inside
one `asc-capability-skill-guard` crate. `SkillGuardService` will coordinate these modules in the
system daemon. This document tracks the migration contract and implementation batches; a planned
row is not evidence that the capability is already available.

## Delivery and acceptance

The core migration and Agent Hook integration are separate PRs. This PR covers the Rust core,
public Action Runtime audit lifecycle, daemon, CLI, SkillFS and Linux deployment. Agent Hook
implementations, capability views, Hook defaults and real Agent acceptance belong to the next PR.
Consumer request/response fixtures establish an interface contract, not successful Agent integration.
Phase-two policy integration is outside this PR.

| Batch | Responsibility | Implementation | Acceptance required before the next batch |
| --- | --- | --- | --- |
| 1 | Types, canonical identity, system keys, Integrity | Implemented; Linux gates passed | Signature/tamper/replay checks, key permissions, source/snapshot path rules |
| 2 | Scanner and analyze | Implemented; Linux gates passed | V1 result comparison, selection/aliases, incomplete coverage, errors, no Ledger writes |
| 3 | Ledger and Service | Implemented; Linux gates passed | Versions, fill-in/force, snapshots, export, serialization, changes during scan |
| 4 | Activation | Implemented; Linux gates passed | Decisions, active/pending/hidden, rollback, publish failure and startup reconcile |
| 5 | daemon, CLI and audit | Planned | Real CLI requests, outputs/exit codes, peer identity, audit, deadlines, admin rotation, consumer fixtures |
| 6 | SkillFS | Planned | One socket, authenticated notify/resolver, no downgrade, real FUSE effects, ordinary IPC regression |
| 7 | Deployment | Planned | Source/RPM installation, root systemd service, local non-root callers, complete core workflow |

Each batch is one independently compiling logical commit with its tests and documentation.
Failures introduced by a batch are fixed in that commit. Linux tests are required; macOS formatting
or manifest inspection does not establish build or runtime acceptance.

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
`v2/crates/action/capabilities/asc-capability-skill-guard/tests/integrity.rs`.
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

`SkillGuardService` owns one lock per canonical Skill identity, shared by direct and resolved
paths. Unused lock entries are discarded. A generation read lock protects operations against
system key replacement; rotation will take the write side in batch five. Registration stores
exact roots in private daemon state and never discovers siblings from a user-selected parent.
Business roots cannot be inside `.skill-meta`; internal snapshot verification does not use this
business-root entry point.

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
