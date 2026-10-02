# SkillFS

[中文版](README_zh.md)

SkillFS is a local FUSE filesystem for agent skills. It parses `SKILL.md`,
organizes skills with views, and exposes compiled `SKILL.md` content through a
mounted filesystem while ordinary skill files remain backed by the source tree.

[![Rust](https://img.shields.io/badge/Rust-1.86+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

## Capabilities

- Parses standard `SKILL.md` files.
- Loads both flat skill directories and categorized directory layouts.
- Uses `skillfs-views.toml` to choose the default view and secondary views.
- Shows default-view skills directly in the mounted agent view.
- Always exposes the virtual `skill-discover` skill so agents can discover
  skills from secondary views and open their advertised paths.
- Compiles `SKILL.md` on read, including conditional blocks and command
  normalization.
- Passes ordinary files and subdirectories through to the physical source tree.
- Supports normal mounts and in-place mounts.
- Supports physical write passthrough while mounted; `SKILL.md` changes reparse
  and update the in-memory store.
- Provides a Linux POSIX compatibility baseline for ordinary passthrough paths:
  fd-backed I/O, create/mkdir mode handling, long-path fallback,
  open-after-unlink handles, restricted symlink/hardlink policy, FIFO creation,
  and conservative `user.*` xattr passthrough.
- Provides optional external security integration surfaces: decision-command
  activation, activation file/xattr consumption, notify socket events, protocol
  JSONL events, active-mapping reload, startup reconcile, trusted writer
  identity checks, trusted control socket writes, and managed mount recovery.

## Behavior Matrix

| Operation | Normal mount | In-place mount | Notes |
| --- | --- | --- | --- |
| `readdir` | Virtual view | Virtual view | Visibility comes from views plus the store. |
| Read `SKILL.md` | Configured transform output | Configured transform output | Directive/compiler stage by default; raw content when it is disabled with no other transform. |
| Read other files | Passthrough | Passthrough | Reads the physical source file. |
| Write `SKILL.md` | Passthrough + store reparse | Passthrough + store reparse | Directory name is the authoritative store key. |
| `create` ordinary file | Passthrough | Passthrough | Does not update the store. |
| `mkdir` skill directory | Immediately visible | Immediately visible | Inserts a degraded placeholder before async reparse. |
| `rename` skill directory | Visibility switches immediately | Visibility switches immediately | Old name is removed without a visibility gap. |
| `unlink` `SKILL.md` | Removes from store | Removes from store | Skill disappears from the virtual view. |
| `rmdir` skill directory | Removes from store | Removes from store | Also clears inode mappings. |
| `setattr(size)` | Truncate supported | Truncate supported | Other metadata operations are conservative passthrough where allowed. |
| `symlink` | Restricted passthrough | Restricted passthrough | Allows relative same-skill targets only. |
| `link` | Restricted passthrough | Restricted passthrough | Allows same-skill regular files only. |
| `mkfifo` | Passthrough | Passthrough | FIFO only; device/socket nodes are rejected. |
| `xattr user.*` | Passthrough | Passthrough | Ordinary passthrough paths only. |

## Scope

- Public CLI commands are `mount`, `stop`, `classify`, `validate`, and `list`.
- Skill visibility is controlled by `skillfs-views.toml`.
- FUSE write passthrough is supported while mounted, but only `SKILL.md`
  changes trigger store synchronization.
- The authoritative skill key is the directory name, not a stale frontmatter
  `name:` after rename.
- In-place mounting over-mounts the source directory. Controlled skill writes
  through SkillFS are supported, but tools that rename or replace the mounted
  directory itself, such as workspace checkpoint/init/rollback tools, must run
  before mounting or after unmounting.

## Architecture

```text
physical skills dir
  └─ skill-name/SKILL.md
            │
            ▼
    skillfs-core
      - parser
      - store
      - views
      - compiler
            │
            ▼
      skillfs-fuse
            │
            ▼
     mounted /skills view
```

## Write Path And Consistency

SkillFS is a hybrid filesystem: a virtual directory view plus physical
passthrough writes.

- `readdir` is still controlled by the virtual view.
- Reading `SKILL.md` returns compiled content by default; with the directive
  stage disabled and no other transform, it returns the selected target's raw
  content.
- Other files read and write directly against the underlying filesystem.
- Writing, creating, or writing after renaming `SKILL.md` reparses the file and
  updates `SharedSkillStore`.
- `mkdir` and skill-directory `rename` take an immediate-consistency path by
  synchronously updating the store, with async reparse later replacing the
  placeholder with the real entry.
- In-place mounts use `/proc/self/fd/{n}` to reach the underlying source and
  avoid recursively entering the FUSE over-mount.

## Quick Start

### Build

```bash
cargo build --release
```

### Common Commands

```bash
# Validate skills.
cargo run -p skillfs -- validate /path/to/skills

# List skills.
cargo run -p skillfs -- list /path/to/skills

# Generate or inspect skillfs-views.toml.
cargo run -p skillfs -- classify /path/to/skills

# Mount the FUSE filesystem.
cargo run -p skillfs -- mount /path/to/skills /path/to/mountpoint

# Opt-in managed mount: a detached supervisor keeps the mount alive.
cargo run -p skillfs -- mount /path/to/skills /path/to/mountpoint --managed

# Stop a managed mount and clear desired mounted state.
cargo run -p skillfs -- stop /path/to/mountpoint
```

### Mount Configuration

Use `skillfs mount --config /path/to/skillfs-mount.toml` with `mountpoint` and
an ordered `sources` array to aggregate directories. Earlier sources win whole
skill-name conflicts; multiple sources are read-only. Paths must be absolute.
See the [configuration reference](../../docs/user-guide/en/runtime/skillfs.md#mount-configuration)
for the TOML example, supported flags, and OpenClaw integration limits.

### Managed Mount Mode

Default `mount`, including `--foreground`, keeps the original foreground
behavior: the process blocks, and `SIGTERM` or `Ctrl+C` cleanly unmounts. If the
launcher process, such as a gateway, restarts and terminates its child
processes, the mount disappears with it.

`--managed` is opt-in and is intended for mounts that should survive gateway
restarts:

- The client writes managed state, starts a detached supervisor in its own
  session with `setsid`, waits for readiness, and then returns.
- The supervisor starts a foreground FUSE worker with the same source,
  mountpoint, config, security, audit, activation, trusted-writer, control
  socket, and logging options.
- If the worker exits unexpectedly while desired state remains `mounted`, the
  supervisor remounts after bounded backoff.
- Only `skillfs stop <MOUNTPOINT>` clears desired mounted state, terminates the
  supervisor/worker, and unmounts. `stop` is idempotent and can be run safely on
  an already-unmounted path.
- If the supervisor is killed with `kill -9`, an orphan worker may continue to
  serve the mount without monitoring. Run `skillfs stop <MOUNTPOINT>` to clean
  residual state, processes, and mounts before starting `mount --managed` again.

Managed state is stored in a user-isolated runtime directory: first
`$XDG_RUNTIME_DIR/skillfs/`, then `/run/user/<uid>/skillfs/`, and when neither
is available it falls back to `/tmp/skillfs-<uid>/`. The instance id is derived
from the normalized mountpoint, so `mount` and `stop` always resolve the same
mountpoint to the same instance.

### In-place Mount And Workspace Snapshots

When SkillFS is mounted in-place, tools that replace the mountpoint directory
itself must run before mounting or after unmounting. For example,
`ws-ckpt checkpoint -w <MOUNTPOINT>` may fail with `Device or resource busy` if
`<MOUNTPOINT>` is an active SkillFS mount.

Writes through SkillFS, including skill install/update/remove, remain supported
while mounted.

## `skillfs-views.toml`

Skill selection is controlled by `skillfs-views.toml`:

```toml
[[view]]
name = "major"
default = true
description = "Skills shown directly in /skills"
skills = ["github", "notion", "slack"]

[[view]]
name = "other"
default = false
description = "Skills exposed via skill-discover"
skills = ["apple-notes", "blogwatcher"]
```

After mounting:

- `/skills` shows skills from the default view.
- `skill-discover/SKILL.md` lists skills from secondary views and their
  readable `source_path` values.

## `SKILL.md` Format

```markdown
---
name: my-skill
description: Brief description
version: 1.0.0
tags: [tooling, example]
enabled: true
---

# My Skill

Detailed instructions.

## Parameters

- `input` (string, required): Input value
- `options` (object, optional): Extra options

## Returns

- `result` (string, required): Result value
```

## Conditional Compilation

When FUSE reads `SKILL.md`, SkillFS runs `compiler::compile` and supports:

- `<!-- @if os == darwin -->`
- `<!-- @if has_command("uv") -->`
- `<!-- @else -->`
- `<!-- @endif -->`

When there are no conditional blocks, SkillFS also applies a small set of
heuristic command normalizations, for example:

- `pip install` -> `uv pip install`
- `python -m venv` -> `uv venv`
- `npm install` -> `pnpm install` / `yarn install`

## Read-Time Transform Pipeline

`SKILL.md` reads pass through an ordered transform pipeline after the activation
target (live source, trusted snapshot, or hidden) is resolved:

```text
activation target -> read selected bytes -> [directive/compiler stage]
  -> [optional os_adapter stage] -> Agent-visible bytes
```

Both stages are optional and independent; the fixed order is
`directive -> os_adapter`:

- The **directive** stage is the conditional compiler above. It is **enabled by
  default**, and when present it always runs first, so existing mounts stay
  byte-for-byte identical to earlier releases. Disable it with
  `[transforms.directive] enabled = false` (see below).
- The **os_adapter** stage is opt-in, applies only to `SKILL.md`, and runs
  second. It rewrites distribution-specific literals (package managers,
  `-dev`/`-devel` package names, service unit names, filesystem paths) between
  Ubuntu/Debian and Alinux/Anolis style.

Any combination is valid: both stages (the default plus an enabled adapter),
directive-only (the default), adapter-only (directive disabled), or neither —
an empty pipeline serves the selected raw bytes unchanged. Initialization
diagnostics report the actual enabled stage list, including an empty list.

| Directive | OS adapter | Agent-visible `SKILL.md` |
| --- | --- | --- |
| enabled (default) | disabled (default) | Legacy compiler output |
| enabled | enabled | Compiler output, then OS adaptation |
| disabled | enabled | OS adaptation of raw selected bytes |
| disabled | disabled | Raw selected bytes |

Transforms never modify source files, snapshots, activation metadata, or the
rule artifact. Hidden skills stay `ENOENT` and never enter the pipeline; a
snapshot read is transformed from the snapshot and never falls back to the live
source. The same pipeline applies to flat and Hermes nested `SKILL.md`. Only
`SKILL.md` is adapted — other Markdown, shell, Python, and config files pass
through unchanged.

### Disabling the Directive Stage

The directive/compiler stage stays enabled unless explicitly turned off:

```toml
[transforms.directive]
enabled = false
```

When the `[transforms.directive]` section is absent, directive compilation
remains enabled, so existing configurations are unaffected. Disabling it only
affects the compiler stage; the OS adapter remains independently opt-in.

### OS Adapter Configuration

The OS adapter reuses the existing `--config <PATH>` TOML file (no new CLI
flags). It is disabled unless explicitly enabled. When enabled without a
`rules_path`, it uses the built-in catalog:

```toml
# /etc/skillfs/skillfs-security.toml
[transforms.directive]
enabled = true

[transforms.os_adapter]
enabled = true
target_os = "alinux" # auto | ubuntu | alinux
# rules_path = "/etc/skillfs/ubuntu-alinux.custom.yaml"
```

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --config /etc/skillfs/skillfs-security.toml
```

SkillFS **ships a built-in 311-rule Ubuntu/Alinux catalog** embedded in the
binary from the repository asset, so the adapter works in source builds, RPMs,
and containers without a separate file. Of those rules, 257 are
`auto_apply: always` and 54 are `auto_apply: never`; compilation produces 223
active substitutions toward Alinux and 192 toward Ubuntu. It remains opt-in.

- `target_os = "auto"` detects the host distribution from the exact
  `/etc/os-release` `ID` once at mount startup — `ubuntu`/`debian` map to Ubuntu
  and `alinux`/`anolis` map to Alinux. Detection is **fail-closed**: `ID_LIKE`
  is not consulted, so RHEL-family derivatives (Rocky, AlmaLinux, CentOS, …) do
  not silently resolve to Alinux, and unrecognized hosts reject the mount. Set
  `ubuntu` or `alinux` explicitly on other distributions.
- `rules_path` is an **optional external override**. Omit it to use the built-in
  catalog; set a non-empty path to load an external read-only artifact instead.
  A present-but-blank path is rejected, not treated as the default. SkillFS
  loads and validates the chosen artifact once at mount startup; the per-read
  hot path performs only in-memory substitution — no model, network, or
  subprocess call at any point.
- TOML does not enable individual rules. `rules_path` replaces, rather than
  merges with, the built-in catalog. To keep the defaults while enabling a
  protected rule or adding a local mapping, copy
  `crates/skillfs-core/assets/ubuntu-alinux.yaml` from the source checkout,
  edit the copy, and configure its absolute path. Change the selected rule from
  `auto_apply: never` to `always`, or append a complete custom rule. The
  artifact is loaded once at mount startup, so remount after editing it. There
  is currently no catalog overlay, hot reload, or export command.

In the built-in catalog, high-confidence rules are `auto_apply: always`;
medium- and low-confidence rules are `auto_apply: never`, so they protect
matched spans but are never substituted.
The rule artifact — built-in or external — is a top-level YAML sequence. Each
entry declares the literal strings for each OS side, a `direction`, and an
explicit `auto_apply` eligibility flag:

```yaml
- ubuntu: "apt-get install -y "
  alinux: "dnf install -y "
  direction: bidirectional          # bidirectional | ubuntu_to_alinux_only | alinux_to_ubuntu_only
  match: literal                    # literal | token — optional, defaults to literal
  auto_apply: always                # always | never — REQUIRED
```

- `auto_apply` is **required** on every rule, including external override
  artifacts. Only `auto_apply: always` rules are applied, and only in a
  direction the resolved target permits. An artifact that omits `auto_apply` is
  rejected at mount startup with an error naming the offending rule index.
- `confidence` and `notes` are accepted as human annotations but carry no
  behavior — eligibility is governed solely by `auto_apply`.
- `match` is optional and defaults to `literal`, preserving substring matching
  for existing artifacts. `match: token` requires ASCII-alphanumeric boundaries
  at alphanumeric source edges in both directions: `cron` matches at EOF or
  before whitespace/newlines/punctuation, but not inside `micron`, `crontab`,
  `cronutils`, or `cron2`.
- Substitution is a single non-cascading pass over the original bytes: at each
  position the longest matching pattern wins (most specific first), so
  overlapping patterns like `apache2` and `apache2-utils` never chain and file
  order does not affect the result.
- Ineligible patterns (`auto_apply: never`, identity, or direction-disallowed)
  still match and are emitted unchanged, protecting their whole span so a shorter
  eligible rule cannot rewrite inside them (e.g. a `never` `/etc/init.d/apache2`
  is not touched by the `apache2` rule). Protection is deduplicated by
  `(source, match)`: a substitution removes protection only for the same source
  and mode. Different modes coexist; substitution wins only when its own mode
  matches the input, otherwise matching protection still preserves the span.
- A many-to-one forward mapping (several Ubuntu spellings → one Alinux package)
  must resolve reverse ambiguity **explicitly**: mark exactly one pair
  `bidirectional` (the canonical reverse) and the alternates
  `ubuntu_to_alinux_only`. Two `bidirectional` rules that collide on the reverse
  target are rejected as ambiguous.

When `enabled = true`, a missing or unreadable external `rules_path`, a blank
`rules_path`, malformed YAML, an invalid `direction`/`auto_apply`/`match` value,
duplicate or ambiguous patterns, or an unrecognized `target_os = "auto"` host
all fail the mount before it starts with an actionable error rather than a
silently disabled adapter.

## Project Layout

```text
crates/
  skillfs-core/   parser, store, views, compiler, env, watcher
  skillfs-fuse/   FUSE filesystem and POSIX passthrough layer
  skillfs-cli/    mount / stop / classify / validate / list
container/        sidecar image: Dockerfile, entrypoint, preflight, mount probe
deploy/kubernetes/  Kubernetes sidecar manifests and example skill source
docs/specs/       implementation specifications
docs/security/    external decision and runtime activation docs
docs/testing/     POSIX acceptance and external harness docs
docs/skills/      bundled agent-facing SkillFS skill
scripts/          build.sh, test.sh, and optional POSIX harness
```

## Test Scripts

- [scripts/build.sh](scripts/build.sh)
  - Runs the workspace build.
- [scripts/test.sh](scripts/test.sh)
  - Creates a temporary skill source directory and `skillfs-views.toml`.
  - Verifies that the FUSE mount starts.
  - Verifies that `/skills` exposes default-view skills.
  - Verifies that `skill-discover` paths open secondary skills.
  - Verifies passthrough reads for physical files inside a skill directory.
  - Verifies clean unmount through `SIGTERM`.
- [scripts/posix/run_pjdfstest.sh](scripts/posix/run_pjdfstest.sh)
  - Optional external POSIX harness; normal `cargo test` does not depend on it.

## Kubernetes Sidecar

SkillFS can expose its FUSE view to a non-privileged workload from a privileged
sidecar. This requires Kubernetes 1.29+, `/dev/fuse`, and permission to run the
sidecar as privileged.

The sidecar supervisor detects failed FUSE reads and remounts inside the
container. Its recovery budget is configurable through `SKILLFS_SUPERVISOR_*`;
see the sidecar guide below for defaults and recovery limits.

For read-only installed skills, the guide also provides a Ledger profile that
seeds a private writable source, scans before mounting, and masks installed
copies from Cosh. Missing or invalid activation remains hidden.
The profile uses `skillfs mount --read-only` to reject FUSE writes at the kernel
boundary while Ledger keeps writing the separate private source.
Ledger RPC probes gate startup and readiness and restart an unresponsive daemon.

```bash
cd src/skillfs
IMAGE=registry.example.com/anolisa/skillfs-sidecar:$(git rev-parse --short=12 HEAD)
DOCKERFILE=container/Dockerfile
# Use container/Dockerfile.alinux4 for Alibaba Cloud Linux 4.
docker build -f "$DOCKERFILE" -t "$IMAGE" .
docker run --rm "$IMAGE" skillfs --version
docker push "$IMAGE"
kubectl apply -f deploy/kubernetes/00-namespace.yaml
kubectl apply -f deploy/kubernetes/10-example-configmap.yaml
sed "s|skillfs-sidecar:dev|$IMAGE|g" deploy/kubernetes/20-pod.yaml |
  kubectl apply -f -
```

See the
[Kubernetes sidecar user guide](../../docs/user-guide/en/runtime/skillfs-kubernetes-sidecar.md)
([中文](../../docs/user-guide/zh/runtime/skillfs-kubernetes-sidecar.md)) for
deployment, `skill-discover` verification, troubleshooting, and cleanup.

## Test Coverage

`crates/skillfs-fuse/tests/` covers:

- compiled `SKILL.md` reads, write passthrough, store reparse,
  mkdir/rename/unlink/rmdir visibility, and stale-frontmatter regressions for
  normal and in-place mounts;
- POSIX open/create, metadata, directory streams, long-path fallback,
  open-after-unlink, safe symlink/link/FIFO, and `user.*` xattrs;
- `.skill-meta`, lifecycle namespaces, security mode, audit runtime, source
  drift, install inbox, staging/direct install flows, trusted writer, trusted
  metadata view, activation consumer, control socket server behavior, notify,
  runtime reload, startup reconcile, and post-publish grace paths.

`crates/skillfs-cli/tests/` covers CLI parsing and startup gates, including
managed mount supervision, activation/notify option compatibility, backing-root
requirements, trusted writer executable validation, and control-socket trusted
peer configuration.

`skillfs-core` covers parser, store, compiler, and watcher behavior with unit
and integration tests.

## Highlights

- Virtual views are decoupled from the physical filesystem: directory
  visibility is view-controlled while file content still comes from the real
  source tree.
- `SKILL.md` reads and writes are intentionally split: agents read compiled
  content, while writes update the raw source file.
- Directory name is the unified authoritative skill key after rename, avoiding
  stale frontmatter reinjection under the old skill name.
- In-place mounts use a pre-opened source dir fd so SkillFS can write through
  without recursively entering its own FUSE mount.
- Active mapping can expose `/skills/<name>` as current source, trusted
  snapshot, or hidden, and open file handles keep their open-time target pinned.

## Security Integration

SkillFS does not perform scanning, signing, or risk decisions inside the
filesystem core. An external provider decides whether a skill is exposed as:

- `current`: serve the live source tree;
- `fallback`: serve a trusted `.skill-meta/versions/*.snapshot`;
- `hidden`: hide the skill from the agent-facing view.

Two integration paths are supported:

- Legacy decision-command mode:
  `--security --decision-command <COMMAND>` runs
  `<COMMAND> scan <skill_dir> --json` and then
  `<COMMAND> resolve <skill_dir> --json`.
- Activation-file mode:
  `--security --activation-mode file` consumes
  `.skill-meta/activation.json` or
  `user.agent_sec.skill_ledger.activation`, sends notify events to an external
  daemon when configured, and reloads active mappings when activation changes.

Related security surfaces:

- `.skill-meta/**` is hidden from untrusted lookup/list/read paths and ordinary
  mutation attempts are rejected. Trusted exact-path access can route to the
  live source for metadata operations.
- `--audit-log <PATH>` writes stable JSONL audit events.
- `--security-mode` requires `SOURCE` and `MOUNTPOINT` to resolve to the same
  directory so normal userspace access goes through FUSE policy and audit.
- `/.skillfs-inbox/<skill>/...` is an install/repair entry point for hidden or
  new skills; writes land in the source tree and completion can trigger the
  external security flow.
- `--notify-socket <PATH>` sends debounced skill mutation notifications to an
  external daemon. Notify v2 identifies the Skill with its canonical path and
  complete flat or Hermes `skillId`; live/backing paths are resolved separately.
  `--notify-auth-key-file <PATH>` enables mutual HMAC authentication for a
  container peer implementing the proposed contract. The inner notify v2
  payload is unchanged, while a session-bound tag protects both the request and
  acknowledgement. Authenticated notify also requires an owner-matched socket
  with no group/other permissions under an owner-matched directory that also
  grants no group/other permissions; `0700` is the recommended directory mode.
  The initial profile requires SkillFS and sec-core to use the same effective
  UID. Peer-side sec-core support is not implemented by this SkillFS change and
  remains tracked in #2439.
  In-place notify mounts, and any notify mount with `--ledger-backing-root`,
  require an authenticated control-peer mode so the resolver is available
  before the daemon accesses the source.
- `--activation-events-log <PATH>` writes activation protocol events as JSONL.
- `--activation-reload-mode poll` re-reads activation state after notify events
  and updates the resolver without a remount.
- Startup reconcile queues known skills through the notify worker and retries
  temporary delivery failures until the daemon acknowledges, so SkillFS can
  start before the daemon and still converge. Non-retryable failures are
  reported, while inconclusive authentication uses a shared bounded endpoint
  budget to prevent retry storms. See
  [Reconcile Delivery Durability](docs/security/runtime-activation-implementation-plan.md#reconcile-delivery-durability)
  for retry and observability details.
- `--ledger-backing-root <PATH>` provides a daemon-visible source view for
  in-place activation/notify mounts, because the public source path is a FUSE
  over-mount. Use `/run/user/$UID/skillfs-ledger/...` or
  `/run/skillfs-ledger/...` for daemon-facing roots. Do not use `/tmp` or
  `/var/tmp`: packaged `agent-sec-core.service` runs with `PrivateTmp=true`,
  so host tmp paths are invisible to the daemon and are rejected at startup.
- `--trusted-writer-exe <PATH>` is the recommended mount-path trusted writer
  gate. It verifies `/proc/<tgid>/exe`, `(dev, ino)`, and process start time to
  reduce PID-reuse and process-name spoofing risk.
- `--trusted-writer <NAME>` is a deprecated compatibility gate based on Linux
  TGID `comm`; process names can be spoofed and this should not be used for
  production trust.
- `--control-socket <PATH>` starts a trusted Unix socket control plane when
  exactly one peer mode is configured. `--trusted-peer-exe <PATH>` preserves
  host executable authentication; `--trusted-peer-key-file <PATH>` enables an
  explicit container HMAC profile, protects control requests and responses
  with session-bound tags, and requires an explicit socket path.
  Trusted peers can write activation JSON or xattr through methods such as
  `meta.writeActivation` and `meta.setActivationXattr`.
- The control plane is opt-in and authenticated. The endpoint is resolved by
  priority: CLI `--control-socket` > `[control_socket].path` in the config >
  the default per-user endpoint `/run/user/<uid>/skillfs/control.sock`. A
  executable peer without an explicit path uses the default endpoint; HMAC
  mode always requires an explicit path; an explicit path without a peer mode
  is a configuration error. The default never falls back to `/tmp` or
  `/var/tmp`, and a second instance never unlinks an active endpoint.
- `skill.resolveLiveSource` is a read-only query that maps a caller-supplied
  canonical Skill directory to its physical live/backing source. It returns
  `managed=true` (with the derived `skillId`, `relativeSkillDir`,
  `liveSkillDir`, and the live directory's `(device, inode)` identity),
  `managed=false` for a valid path outside the managed root, or a structured
  error. Skill ids are derived from the canonical relative path, so both flat
  (`my-skill`) and Hermes nested (`apple/apple-notes`) layouts resolve to full
  ids. No `register`, `mountId`, or `generation` is required.

## Documentation

- [Kubernetes sidecar deployment guide](../../docs/user-guide/en/runtime/skillfs-kubernetes-sidecar.md) -
  Build, deploy, verify, and clean up SkillFS in Kubernetes.
- [docs/specs/skillfs-spec.md](docs/specs/skillfs-spec.md) - Architecture,
  runtime consistency boundaries, and deployment scenarios.
- [docs/specs/core-spec.md](docs/specs/core-spec.md) - `skillfs-core`
  implementation.
- [docs/specs/fuse-spec.md](docs/specs/fuse-spec.md) - `skillfs-fuse`
  implementation.
- [docs/specs/posix-phase1-spec.md](docs/specs/posix-phase1-spec.md) - POSIX
  passthrough baseline.
- [docs/testing/posix-phase1-acceptance.md](docs/testing/posix-phase1-acceptance.md)
  - POSIX acceptance checklist.
- [docs/testing/posix-external-harness.md](docs/testing/posix-external-harness.md)
  - External POSIX harness usage.
- [docs/security/external-decision-protocol.md](docs/security/external-decision-protocol.md)
  - Decision-command JSON protocol.
- [docs/security/runtime-activation-implementation-plan.md](docs/security/runtime-activation-implementation-plan.md)
  - Activation, notify, reload, and backing-root integration.
- [docs/design/control-socket-resolver.md](docs/design/control-socket-resolver.md)
  - Control socket default endpoint and the read-only
    `skill.resolveLiveSource` resolver (SkillFS S1).
- [docs/design/notify-v2.md](docs/design/notify-v2.md) - Canonical Skill
  identity and live-root separation for daemon change notifications (SkillFS
  S2).
- [docs/skillfs-filesystem-capability-record.md](docs/skillfs-filesystem-capability-record.md)
  - Long-lived filesystem capability record.
- [POSIX_FS_TEST_MATRIX.csv](POSIX_FS_TEST_MATRIX.csv) - POSIX test matrix and
  current coverage.
- [POSIX_FS_REFERENCES.md](POSIX_FS_REFERENCES.md) - POSIX, FUSE, and project
  references.

## Validation

These commands are CI-equivalent checks. Run them before PR submission when
touching SkillFS code.

```bash
# 1. Formatting: must produce no diff.
cargo fmt --all --check

# 2. Clippy: must be warning-free under -D warnings.
cargo clippy --workspace --all-targets -- -D warnings

# 3. Unit and integration tests in the workspace.
cargo test --workspace

# 4. End-to-end FUSE mount test. Requires fuse3 and /dev/fuse; skips itself
#    on macOS or containers without /dev/fuse.
scripts/test.sh

# 5. Rustdoc. Required when changing public API or doc comments; useful for
#    catching broken intra-doc links early.
cargo doc --workspace --no-deps
```

Contributor conventions for comments, module layout, dependencies, error
handling, and commits are documented in [AGENTS.md](AGENTS.md).
