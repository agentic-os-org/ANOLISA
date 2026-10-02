# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to ANOLISA Blaze will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-22

### Added

- Generic `StorageProvider` trait with pluggable backend architecture.
- `FileStorageProvider`: default file-based storage backend for development and standard deployments.
- `[storage]` config section: `provider`, `pool_size`, `prefork`, `flush_interval` fields with backward-compatible defaults.
- `GET /v1/health` now includes `storage_pool` status (ready/capacity/pending).
- `BackendSpawner` trait extended with `restore`, `pause`, `resume`, `create_snapshot` methods (default unsupported, enabling future snapshot workflows).

## [0.2.1] - 2026-07-21

### Changed

- **Rebrand**: Component renamed from Anvil to Blaze. Binary: `blazed`, config path: `/etc/anolisa/blaze/`, state: `/var/lib/blaze/`.
- Firecracker vCPU configuration now validated against upper bound (1–32).

### Added

- Component registered in project manifests (root README, AGENTS.md, PR template).
- VM resource configuration fallback chain documented in README.

## [0.2.0] - 2026-06-30

### Added

- FirecrackerSpawner: Firecracker microVM backend, daemon auto-detects and selects strongest isolation at startup.
- TCP remote API: configurable `[listen].http_addr` enables TCP listener (port 14159) for platform calls.
- Prioritized backend selection: `build_spawner()` auto-selects by firecracker → linux-sandbox → mock priority.
- Storage section: `[storage].images_dir` unifies vmlinux/rootfs lookup path.
- Packaging skeleton: `dist/anvil.service` (systemd unit) + `anvil.spec` (RPM) + `tmpfiles-anvil.conf`.
- `[backends]` config section for direct backend binary path mapping.

## [0.1.3] - 2026-06-24

### Changed

- Sandbox processes now run with full namespace isolation (PID, network, filesystem).

## [0.1.2] - 2026-06-22

### Added

- Sandbox processes are now managed by the daemon: auto-spawn on create, auto-kill on destroy.
- Daemon gracefully degrades when backend binary is unavailable (useful for dev environments).

## [0.1.1] - 2026-06-20

### Added

- Policy validation rejects unsafe configurations before sandbox starts.
- Safe coordination with `osbase sandbox uninstall` (prevents removing in-use backends).

## [0.1.0] - 2026-06-18

Initial scaffold of ANOLISA Anvil per-host sandbox daemon.

### Added

- Create, list, inspect, checkpoint (state-only), reset, and destroy sandboxes via HTTP API.
- Policy-driven backend selection: assign workload class → get the right sandbox type automatically.
- Warm pool: pre-created sandboxes ready for instant allocation, configurable min/target/max.
- Template sharing: multiple sandboxes share one base memory image, reducing per-instance cost.
- Prometheus metrics endpoint for monitoring.

