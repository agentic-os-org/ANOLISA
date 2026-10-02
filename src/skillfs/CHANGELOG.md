# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to SkillFS are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-09-20

### Added

- `skillfs mount --config mount.toml` now combines ordered skill sources in
  one mount. Earlier sources win whole skill directories; multi-source mounts
  are read-only and require remounting after configuration changes or adding
  or removing skills
  ([#3200](https://github.com/agentic-os-org/ANOLISA/pull/3200)).
- An optional Kubernetes deployment seeds read-only skill packages into a
  private writable source for Ledger scanning and activation. The new
  `--read-only` option prevents writes through the agent-visible mount;
  missing or invalid activation keeps skills hidden
  ([#3182](https://github.com/agentic-os-org/ANOLISA/pull/3182)).

### Changed

- Both Debian and Alibaba Cloud Linux 4 sidecar images now supervise real
  FUSE reads and attempt remounts after repeated failures, with bounded
  retries. Workloads must reopen failed handles after recovery
  ([#2701](https://github.com/agentic-os-org/ANOLISA/pull/2701)).
- Read-only transformed `SKILL.md` handles now retain their captured content
  across source edits, while new opens read the updated content. A bounded
  cache reuses transformation results across attribute queries and opens.
  Separately, new captured handles that would exceed the mount-wide 64 MiB or
  1,024-handle budget fail to open with `ENOMEM`; closing captured handles frees
  capacity
  ([#3202](https://github.com/agentic-os-org/ANOLISA/pull/3202)).

## [0.4.2] - 2026-08-27

### Added

- An optional Alibaba Cloud Linux 4 sidecar image now provides a reproducible
  alternative to the existing Debian image, with bilingual guidance for
  building and operating both variants
  ([#2777](https://github.com/alibaba/anolisa/pull/2777),
  [#2787](https://github.com/alibaba/anolisa/pull/2787)).

### Fixed

- Startup reconciliation now retries transient notify-daemon endpoint failures
  with bounded backoff, allowing hidden skills to converge automatically when
  the daemon starts late or restarts
  ([#2790](https://github.com/alibaba/anolisa/pull/2790)).
- Flat normal-mode mounts now resolve categorized skills through their stored
  physical directories, so writes, truncation, renames, and synchronization no
  longer fail with `ENOENT`
  ([#2901](https://github.com/alibaba/anolisa/pull/2901)).

## [0.4.1] - 2026-08-21

### Added

- A Kubernetes sidecar deployment now lets a privileged SkillFS container
  expose its FUSE view to a non-privileged workload, including readable paths
  advertised by `skill-discover`
  ([#2057](https://github.com/alibaba/anolisa/pull/2057)).
- Optional mutual HMAC-SHA256 authentication now protects control and notify
  sockets across container namespaces through `--trusted-peer-key-file` and
  `--notify-auth-key-file`, while existing host authentication remains
  unchanged ([#2449](https://github.com/alibaba/anolisa/pull/2449)).

### Changed

- The reference Kubernetes deployment now marks the Pod unready after one
  failed FUSE read and restarts only the SkillFS sidecar after two liveness
  failures, allowing workloads to recover without restarting
  ([#2705](https://github.com/alibaba/anolisa/pull/2705)).

### Fixed

- Nested Hermes skills can now update activation state through the control
  socket with layout-relative identifiers such as `category/skill`
  ([#2407](https://github.com/alibaba/anolisa/pull/2407)).
- Activation metadata permissions, in-place backing aliases, and control
  listener startup now fail closed instead of exposing metadata, accepting an
  unsafe source, or leaving an unusable endpoint
  ([#2407](https://github.com/alibaba/anolisa/pull/2407)).
- The bundled `skillfs-mount` skill now names the shipped analysis scripts and
  accurately describes managed mounts and writable source behavior
  ([#1798](https://github.com/alibaba/anolisa/pull/1798)).

## [0.4.0] - 2026-07-24

### Added
- Configurable read-time transforms now keep directive compilation enabled by
  default and add an opt-in OS adapter with bundled Ubuntu/Alinux rules and
  external catalog overrides
  ([#1484](https://github.com/alibaba/anolisa/pull/1484)).
- An authenticated live-source resolver and notify v2 protocol now give Skill
  Ledger canonical flat or Hermes skill identities, event kinds, and changed
  paths without exposing backing-root details
  ([#1517](https://github.com/alibaba/anolisa/pull/1517)).

### Changed
- Agent-visible access checks now follow activated snapshot permissions while
  live-source permissions continue to govern writes
  ([#1517](https://github.com/alibaba/anolisa/pull/1517)).

### Fixed
- SLS telemetry writers now honor `/etc/anolisa/.telemetry_disabled`
  dynamically and fail closed when the gate cannot be inspected
  ([#1584](https://github.com/alibaba/anolisa/pull/1584)).
- Hermes symlink boundaries, resolver paths, socket ownership, and peer
  authentication now fail closed across discovery, reads, and mutations
  ([#1517](https://github.com/alibaba/anolisa/pull/1517)).
- Control-socket prerequisite diagnostics now consistently include the public
  `--control-socket` flag name
  ([#1739](https://github.com/alibaba/anolisa/pull/1739)).

## [0.3.4] - 2026-07-16

### Fixed
- SLS ops logging now preserves exactly one command record when CLI output
  pipes close early and panic unwinds.

## [0.3.3] - 2026-07-10

### Added
- Hermes workspace layout compatibility. SkillFS now recognizes Hermes hub
  markers, preserves management paths, and exposes nested
  `category/skill/SKILL.md` skills alongside top-level skills.
- Nested Hermes skills now support activation state, installer lifecycle
  writes, notifications, audit attribution, fallback snapshots, and hidden
  visibility.

### Fixed
- `skillfs validate --json` now includes source paths for warning and error
  entries so automation can locate invalid skills.
- FUSE teardown now bounds failed unmount cleanup and prevents leaked test
  mounts from affecting later sessions.

## [0.3.2] - 2026-07-03

### Fixed
- CLI SLS ops logging now records SkillFS mount and runtime operations.
- Runtime metrics now emit real-time deltas for SLS consumers.

## [0.3.1] - 2026-07-03

### Added
- Managed mount supervision can recover stale FUSE mounts and bound recovery
  retries during repeated starts.

### Changed
- English and Chinese README guidance now covers managed mounts, in-place
  operation, security boundaries, and troubleshooting.

### Fixed
- Post-publish grace reads fallback skill files from source paths after
  installers finish.
- `skillfs validate` now reports parse failures in the status summary.
- In-place authoring supports new skills and pending-install ownership changes.
- Managed stop and runtime-dir handling avoid stale ownership and unbounded
  recovery retries.
- Daemon-facing backing roots under PrivateTmp are rejected before mount
  startup.
- FUSE smoke cleanup handles leftover mounts and temporary paths more reliably.

## [0.3.0] - 2026-06-26

### Added
- Runtime security integration for agent skill directories. SkillFS can now
  consume activation decisions from `.skill-meta/activation.json` or the
  `user.agent_sec.skill_ledger.activation` xattr, then expose each skill as
  current, hidden, or a trusted fallback snapshot.
- File-change notification for external security daemons. With
  `--activation-mode file`, `--notify-socket`, `--activation-events-log`, and
  `--activation-reload-mode poll`, SkillFS reports skill mutations, reloads
  activation decisions, and keeps already-opened file handles pinned to their
  original target.
- Trusted control socket for activation writes. A daemon verified with
  `SO_PEERCRED`, executable identity, and start-time checks can update
  activation JSON or activation xattr through a bounded request API instead of
  writing `.skill-meta` through the agent-visible mount path.
- Installer compatibility for common skill installation flows. Staging
  directories, direct writes, quiet-timeout completion, and post-publish grace
  windows allow installers to finish writing a skill before SkillFS asks the
  security provider to scan and activate it.
- In-place mount support for security daemons. Ledger backing roots are bind
  mounted privately and validated at startup so scanners read the real source
  tree rather than the agent-facing FUSE view.
- Canonical skill identity based on the directory basename. Frontmatter
  `name:` remains display metadata and no longer changes the SkillFS store key
  or daemon-facing skill id.

### Changed
- `.skill-meta/**` is hidden from ordinary agents and remains accessible only
  through trusted metadata paths or the control socket.
- Skill mutation notify uses ordinary filesystem event kinds, including
  `create`, `write`, `rename`, `unlink`, `rmdir`, and truncate events, instead
  of a separate install-complete protocol event.
- POSIX passthrough behavior was expanded for symlink, hardlink, FIFO, path
  length fallback, open-after-unlink, xattr, and inode consistency cases.

### Fixed
- Prevented stale activation views by combining notify-triggered reload,
  polling, and activation watcher convergence.
- Hardened trusted-writer and trusted-peer checks against process reuse and
  executable replacement with start-time and file-identity validation.
- Avoided installer and daemon visibility bugs around hidden skills, fallback
  snapshots, staging paths, and backing-root propagation.

## [0.2.0] - 2026-05-09

### Added
- FUSE write passthrough for `write`, `create`, `mkdir`, `rename`, `unlink`,
  `rmdir`, and `setattr(size)` operations on skill directories.
- Background sync worker that reparses `SKILL.md` on write and `upsert`s the
  entry back into `SharedSkillStore`.
- Immediate visibility for newly created skill directories: `mkdir` inserts a
  `ParseStatus::Degraded` placeholder, then the sync worker overwrites it with
  the real entry once `SKILL.md` is written.
- in-place mount mode that accesses the underlying source via
  `/proc/self/fd/{n}` to avoid the over-mount self-loop.
- Integration suite `crates/skillfs-fuse/tests/write_guard_tests.rs` covering
  both normal and in-place write paths.

### Changed
- Directory name is now the authoritative store key. After `rename`, stale
  frontmatter `name:` no longer revives the old key.
- Read of `SKILL.md` still returns the compiled result; raw file is only used
  for writes and parsing.
- Architecture docs refactored into `docs/specs/skillfs-spec.md`,
  `docs/specs/core-spec.md`, `docs/specs/fuse-spec.md`.

### Removed
- Workspace-related code paths and the unused workspace config support from
  `skillfs-core` (commit 6d604c7).
- Legacy ad-hoc test scripts (kept only `scripts/build.sh` and
  `scripts/test.sh`).

### Fixed
- CLI tracing timestamps now use the local timezone instead of UTC.

## [0.1.2] - 2026-04-29

### Added
- Read-only mount write protection: `mknod`, `symlink`, `link`, and write
  callbacks all return `EROFS`.

### Fixed
- Parser summary truncation now respects multi-byte character boundaries.

## [0.1.1] - 2026-04-29

### Added
- `skillfs-mount` agent skill under `docs/skills/` to help users set up,
  mount, and unmount a SkillFS instance.

## [0.1.0] - 2026-04-25

### Added
- Initial release of the SkillFS workspace.
- `skillfs-core`: `SKILL.md` parser (with `Ok` / `Degraded` / `Error` status),
  in-memory `SkillStore` with flat and categorized directory layouts,
  `skillfs-views.toml` configuration, conditional `compiler::compile`, and
  environment probing (OS, commands, env vars).
- `skillfs-fuse`: read-only FUSE filesystem that exposes the configured
  default view at `/skills`, always-on virtual `skill-discover`, and
  compile-on-read for `SKILL.md`. Other files in a skill directory are
  passed through to the physical source.
- `skillfs` CLI: `mount`, `classify`, `validate`, `list` subcommands.
