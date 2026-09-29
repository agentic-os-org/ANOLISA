# Changelog

[中文版](CHANGELOG_zh.md)

## 0.5.0

### Added
- Unified workspace path identity, conflicting alias registrations are rejected at init
- Workspaces whose init or unregister was interrupted are recovered on daemon restart
- Added guarded rollback V2 protocol and evidence wire variants used by cosh-ng governed tasks, cosh-ng requires ws-ckpt daemon 0.5.0 or newer

### Changed
- OpenClaw tool allowlists are now managed through the OpenClaw config CLI, requiring OpenClaw 2026.2.13 or newer (#3221)
- `/etc/ws-ckpt/config.toml` is now RPM `%config(noreplace)`: upgrades keep admin edits and land new defaults as `.rpmnew`; erase keeps a modified config as `.rpmsave`, restore it manually after reinstall (#3070)

### Fixed
- Fixed empty workspaces being skipped instead of snapshotted
- Fixed checkpoint/rollback silently succeeding on a detached workspace directory, now failing fast with a recovery hint (#3059)
- Fixed `recover --all` exiting 0 when individual workspaces failed (#3069)
- Fixed `rpm -e` wiping the last remaining snapshot copies when recover failed (#3069)
- Fixed checkpoint failures under disk-space pressure by reclaiming deleted subvolumes (#3053)
- Fixed a race when two clients initialize the same workspace concurrently
- Fixed adapter discovery for raw installation layouts
- Fixed OpenClaw plugin install to probe the installed CLI before accepting capabilities (#3116)

## 0.4.5

### Added
- Added bilingual (English/Chinese) sidecar deployment guide (#2965)

### Fixed
- Fixed intermittent bootstrap failure when all loop devices are in use (#2965)
- Scoped sidecar preStop loop cleanup to ws-ckpt-owned devices only (#2965)

## 0.4.4

### Added
- Added guarded checkpoint protocol with identity-fenced snapshot creation, checkpoint state stays verifiable across crashes

### Fixed
- Fixed interrupted rollback state lost on daemon restart, ambiguous candidates are now preserved instead of deleted
- Fixed `config --global` to verify the daemon actually loaded the written config file (#2813)
- Fixed sidecar deployment guide missing the `/etc/ws-ckpt` shared volume required for global config (#2813)
- Fixed error hints to cover containerized deployments and the daemon's mount namespace (#2807, #2814)

## 0.4.3

### Added
- Added k8s sidecar containerized deployment support (#2034)

### Fixed
- Fixed daemon memory leak that caused unbounded growth under sustained checkpoint workloads, eventually leading to OOM (#2554)
- Fixed loop-device backend checkpoint performance under concurrent IO, reducing latency by up to 5x (#2523)
- Fixed bootstrap failure leaving orphan images and loop devices behind, and now reports clear startup errors instead of exiting silently (#1956)
- Fixed init failing with a cryptic EBUSY error when workspace path is already a mount point; now rejected with a clear message (#1798)
- Added RPM component identity declaration for anolisa-cli adapter discovery (#2568)

## 0.4.2

### Added
- Added an telemetry gate to ops log writes (#1509)

### Fixed
- Fixed auto-recover orphan `.pre-init-bak` from interrupted init (#1601)

## 0.4.1

### Added
- Added skip auto-checkpoint after rollback (#1263)

### Fixed
- Fixed workspace sync after config update (#1263)
- Fixed absolute path handling for ws-ckpt in crontab entries (#1263)
- Changed rollback -n offset, pass numAncestors as-is (#1263)

## 0.4.0

### Changed
- **BREAKING** checkpoint `-i`/`--id` flag replaced by `-s`/`--snapshot` as primary; `-i` remains as hidden alias but may be removed in a future release (#1064)

### Added
- Added plugin install/uninstall subcommand (#1005)
- Added component.toml for anolisa-cli adapter discovery (#1005)
- Added rollback preview support with --preview parameter (#1103)
- Added elapsed time display after each CLI operation (#1075)
- Added auto-generated snapshot ID when --snapshot is omitted (#1064)
- Added SLS ops log output for dashboard metrics (#1059)
- Added optional -t flag for diff to compare snapshot against current workspace (#848)
- Added rollback-by-ancestor-count and snapshot DAG tracking (#877)
- Added cron-based scheduled checkpoint snapshots (#819)

### Fixed
- Fixed --snapshot/-s as primary flag and aligned plugin flag handling (#1103, #1064)
- Fixed SKILL.md to sync with actual CLI/plugin implementation (#847)
- Fixed init and recover to guard against replaced workspace symlink (#860)
- Fixed init rsync by dropping --copy-unsafe-links (#873)

## 0.3.3

### Added
- Added per-workspace policy override with hermes/openclaw plugin support (#721)
- Added `/proc` cwd occupant guard for init and rollback (#684)
- Added Hermes adapter runner script (#617)

### Fixed
- Fixed write lock contention and cwd guard deadlock in rollback (#721, #684)
- Fixed input validation for non-UTF-8 paths and path-traversal snapshot IDs (#695, #678)
- Fixed seccomp arch selection, workspace registry concurrency, and RPM packaging (#695, #684)

## 0.3.2

- Fixed openclaw uninstall to remove tool whitelist from config
- Fixed parent path refusal to apply as workspace-level rules for skill and openclaw plugin

## 0.3.1

- Fixed plugin workspace config registration and auto-loading
- Reject workspace paths that are hermes cwd itself or parent
- Fixed plugin tool to prefer explicit workspace parameter over config
- Fixed skill delete requiring --force flag
- Fixed daemon workspace path validation and fswatch fd leak
- Removed unused btrfs_ops.rs module

## 0.3.0

- Added openclaw plugin scaffolding for ws-ckpt
- Added hermes plugin scaffolding for ws-ckpt
- Made ws-ckpt skill agent-agnostic and prompted for workspace at invocation
- Followed `make install` contract for build-all integration
- Fixed bugs in list and diff sub-commands
- Made daemon stateful

## 0.2.0

- Added auto_cleanup feature and switch
- Unified config modification entry through the TOML file
- Added global CLI warning when any workspace>1000 snapshots or filesystem usage>90%
- Fixed backend detection and daemon state recovery logic
- Fixed image size configuration not taking effect after daemon restart
- Removed obsolete fs_warn_threshold_percent parameter
- Fixed config.toml to ship as a sample file

## 0.1.0

- Daemon with Unix Socket IPC and Bincode binary protocol.
- `init` / `checkpoint` / `rollback` / `delete` / `list` / `diff` / `cleanup` / `status` / `config` commands.
- Background scheduler: auto-cleanup, health check, orphan recovery.
- Multi-backend: btrfs-base / btrfs-loop / overlayfs with auto-detection.
- TOML config persistence with runtime hot-reload.
- systemd service with RPM packaging for Alinux 4.
