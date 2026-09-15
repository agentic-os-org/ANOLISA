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
| 2 | Scanner and analyze | Planned | V1 result comparison, selection/aliases, incomplete coverage, errors, no Ledger writes |
| 3 | Ledger and Service | Planned | Versions, fill-in/force, snapshots, export, serialization, changes during scan |
| 4 | Activation | Planned | Decisions, active/pending/hidden, rollback, publish failure and startup reconcile |
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
