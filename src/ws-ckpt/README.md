# ws-checkpoint

[中文版](README_zh.md)

Btrfs-based workspace snapshot system for AI Agents, providing sub-second checkpoint creation and rollback. ws-checkpoint is a runtime component of [ANOLISA](../../README.md), designed to give agents instant undo/redo capability at the filesystem level.

## Features

- **Sub-millisecond snapshots** — leverages btrfs COW for near-instant checkpoint and rollback
- **Daemon architecture** — privileged operations run in a daemon; CLI clients need no root
- **Unix Socket IPC** — bincode binary protocol for high-performance communication
- **systemd integration** — RPM one-click deploy, auto-start on boot
- **Snapshot listing** — table/json output formats
- **Diff between snapshots** — view file changes between any two checkpoints
- **Runtime status monitoring** — daemon and workspace health at a glance
- **Auto-cleanup** — daemon-side scheduled cleanup by count or age retention
- **TOML config hot-reload** — single config entry at `/etc/ws-ckpt/config.toml`, `ws-ckpt reload` applies instantly
- **Capacity alerting** — warns when any workspace exceeds 1000 snapshots or 90% filesystem usage

## Project Structure

```
ws-ckpt/
├── src/                       # Rust Cargo workspace
│   ├── Cargo.toml
│   ├── config.toml.sample     # Default config (shipped as /etc/ws-ckpt/config.toml, %config(noreplace), plus .sample reference)
│   ├── crates/
│   │   ├── common/            # Shared types, IPC protocol codec
│   │   ├── daemon/            # Daemon core logic
│   │   └── cli/               # CLI client
│   ├── systemd/               # systemd service files
│   └── skills/                # OS Skills
├── docs/                      # Documentation
├── ws-ckpt.spec.in            # RPM spec template
├── build-rpm.sh               # RPM build script
└── .gitignore
```

## Quick Start

### Requirements

- Linux (Alinux 4 recommended)
- btrfs filesystem
- Rust 1.70+

The workspace root must not be an active mount point. Initialization moves the
original directory aside as a backup, and `rename(2)` fails with `EBUSY` on a
directory that is itself a mount point — most often an in-place SkillFS mount.
Unmount it (`skillfs stop <PATH>`, or `fusermount3 -u <PATH>`) before `init` or
the first `checkpoint`.

### Build

```bash
cd src
cargo build --release
```

### Install (RPM)

```bash
# Build RPM
./build-rpm.sh

# Install
sudo rpm -ivh ~/rpmbuild/RPMS/x86_64/ws-ckpt-*.rpm

# Start service
sudo systemctl start ws-ckpt
```

### Basic Usage

```bash
# Initialize a workspace
ws-ckpt init --workspace ~/my-workspace

# Create a checkpoint
ws-ckpt checkpoint --workspace ~/my-workspace -s initial -m "initial version"

# Create another checkpoint after changes
ws-ckpt checkpoint --workspace ~/my-workspace -s feature -m "add feature"

# Preview rollback
ws-ckpt rollback --workspace ~/my-workspace -s initial --preview

# Rollback to a snapshot
ws-ckpt rollback --workspace ~/my-workspace -s initial

# Delete a snapshot
ws-ckpt delete --workspace ~/my-workspace -s feature
```

### Snapshot Management

```bash
# List all snapshots
ws-ckpt list --workspace ~/my-workspace

# JSON output
ws-ckpt list --workspace ~/my-workspace --format json

# Diff between two snapshots
ws-ckpt diff --workspace ~/my-workspace --from msg1-step1 --to msg1-step2

# Diff snapshot vs current working tree (omit --to)
ws-ckpt diff --workspace ~/my-workspace --from msg1-step1

# Cleanup old snapshots, keep latest 5
ws-ckpt cleanup --workspace ~/my-workspace --keep 5
```

### Recovering interrupted initialization

Recovery confirmation uses the daemon-resolved workspace and snapshot count.
If the recovery target or snapshot set changes before execution, confirm again.
Update the CLI and daemon together to use this confirmation protocol.

`ws-ckpt recover -w <workspace> --force` also handles an unregistered workspace:
it restores `.pre-init-bak` and retains migrated subvolumes for inspection.
Successful registered recovery archives leftover backups without blocking the
next `init`. If a
registered workspace's live subvolume was deleted externally, use
`ws-ckpt unregister -w <workspace> --force` to remove its stale registration;
this restores no data and preserves snapshots and backups. For both commands,
`--force` only skips interactive confirmation. See the
[recovery guide](../../docs/user-guide/en/runtime/ws-ckpt.md#recovering-interrupted-initialization-and-stale-registrations).

### Configuration

Configuration has two layers: **global** (`/etc/ws-ckpt/config.toml`, daemon-wide defaults) and **local** (`/var/lib/ws-ckpt/indexes/<ws_id>/policy.toml`, per-workspace override). The `ws-ckpt config` subcommand scope:

- No scope flag: prints read-only overview (global config + workspace override stats)
- `-g` / `--global`: view or modify the global config file
- `-w <workspace>` / `--workspace <workspace>`: view or modify a workspace's `policy.toml`

```bash
# View system status
ws-ckpt status --workspace ~/my-workspace

# View global config
ws-ckpt config -g

# Enable periodic auto-cleanup (global)
ws-ckpt config -g --enable-auto-cleanup

# Global retention policy (by count or age)
ws-ckpt config -g --auto-cleanup-keep 10
ws-ckpt config -g --auto-cleanup-keep 30d

# Per-workspace override (only auto_cleanup / auto_cleanup_keep)
ws-ckpt config -w ~/my-workspace                       # 3-column view: effective / local / global
ws-ckpt config -w ~/my-workspace --auto-cleanup-keep 5
ws-ckpt config -w ~/my-workspace --disable-auto-cleanup
ws-ckpt config -w ~/my-workspace --reset               # Remove local policy.toml, inherit global

# Hot-reload after manual config edits
ws-ckpt reload
```

`config -g` writes the global file, then verifies the daemon reloaded the same settings — it exits non-zero and lists any field that differs. In a Kubernetes sidecar deployment the CLI and daemon run in separate containers, so `/etc/ws-ckpt` must be on a volume shared by both, or global settings will not reach the daemon.

## Command Reference

| Command | Description |
|---------|-------------|
| `init` | Initialize a workspace |
| `checkpoint` | Create a snapshot checkpoint |
| `rollback` | Preview or rollback to a snapshot |
| `delete` | Delete a workspace or a single snapshot |
| `list` | List all snapshots in a workspace |
| `diff` | Show file changes between two snapshots |
| `cleanup` | Manually clean old snapshots |
| `status` | Show daemon and workspace status |
| `config` | View or modify daemon configuration |
| `reload` | Notify daemon to reload `config.toml` |
| `recover` | Restore a plain directory or an interrupted initialization backup |
| `unregister` | Remove a missing-subvolume registration while preserving snapshots and backups |
| `plugin` | Install/uninstall ws-ckpt Agent runtime plugins (openclaw/hermes) |

The OpenClaw adapter requires OpenClaw >= 2026.2.13. Installation stops rather than leaving a partial integration when the detected version or config capabilities cannot update the tool allowlist safely.

## License

Licensed under the Apache License, Version 2.0; see [LICENSE](../../LICENSE).

ws-ckpt interacts with the Linux kernel btrfs filesystem (GPL-2.0) solely through the
public system call interface, and invokes `btrfs-progs` (GPL-2.0) exclusively as
independent executable processes. No source code, object code, or header files from any
GPL-licensed component are incorporated, statically linked, or dynamically linked into
ws-ckpt. Such interaction constitutes an independent and separate work within the meaning
of GPL-2.0 Section 2 ("mere aggregation") and imposes no copyleft obligation on ws-ckpt.
