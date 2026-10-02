# SkillFS

SkillFS is a FUSE-based virtual filesystem for agent skills. It maps a physical
skill source tree into a stable runtime view, compiles `SKILL.md` on read, and
keeps ordinary files backed by the source tree.

SkillFS does not make business-level security decisions. External components
such as agent-sec-core or Skill Ledger scan skills and write activation state.
SkillFS consumes that state and exposes each skill as live, fallback snapshot,
or hidden.

## When to Use It

Use SkillFS when you need:

- a stable mount path for agents;
- separation between the source workspace and the agent-visible view;
- default-view filtering plus `skill-discover` for secondary skills;
- in-place policy and audit coverage for production access;
- Skill Ledger integration for fallback and hidden runtime views;
- `.skill-meta` protection from ordinary agent processes.

Do not in-place mount an existing hub workspace directly when that workspace
also contains registry metadata such as `.hub` directories or external
manifests. Keep the hub workspace and the clean SkillFS source root separate.

## Requirements

| Requirement | Details |
| --- | --- |
| OS | Linux for FUSE mounts |
| FUSE | FUSE3 (`libfuse3-dev`, `fuse3`, or equivalent) |
| Device | `/dev/fuse` must be available |
| Rust | 1.86+ for source builds |

macOS can run non-FUSE commands such as `validate`, `list`, and `classify`, but
it cannot mount SkillFS.

## Installation

```bash
# Recommended package install
sudo anolisa --install-mode system install skillfs

# Source build for developers
cd src/skillfs
cargo +1.86.0 build --release
```

## Source Layout

SkillFS expects a source directory with one skill per child directory:

```text
/path/to/skills/
  demo-weather/
    SKILL.md
    scripts/
      run.sh
  demo-search/
    SKILL.md
    config.json
```

The directory name is the canonical runtime skill id. The `name` field inside
`SKILL.md` is display metadata and does not override the directory key.

Do not treat `.skill-meta` as ordinary agent data. It stores SkillFS and ledger
metadata and is hidden from ordinary callers.

## Quick Start

```bash
# Validate skills in a source directory
skillfs validate /path/to/skills

# List all skills
skillfs list /path/to/skills

# Generate skillfs-views.toml
skillfs classify /path/to/skills

# Mount the virtual filesystem
skillfs mount /path/to/skills /mnt/skillfs --foreground
```

After a normal mount, agents read:

```text
/mnt/skillfs/skills/<skill-name>/SKILL.md
```

Unmount a foreground test mount with `Ctrl+C` or:

```bash
fusermount3 -u /mnt/skillfs
```

## Mount Layouts

### Normal Mount

Normal mount uses different source and mountpoint directories:

```bash
skillfs mount /path/to/skills /mnt/skillfs --foreground
```

Agents access skills under `<MOUNTPOINT>/skills`. Direct writes to the source
directory bypass SkillFS policy and audit, while writes through the mount pass
through to the source tree.

Use normal mount for local development, compatibility checks, and environments
where the source workspace is managed by another process.

### In-place Mount

In-place mount uses the same directory for source and mountpoint:

```bash
skillfs mount /path/to/skills /path/to/skills \
  --foreground \
  --security-mode \
  --audit-log /var/log/skillfs/audit.jsonl
```

SkillFS over-mounts the source directory, so normal userspace access goes
through FUSE policy and audit. In-place mounts do not add a `/skills` layer:

```text
/path/to/skills/<skill-name>/SKILL.md
```

Use in-place mount for production security integration. Tools that replace or
rename the mountpoint directory itself, such as workspace checkpoint or rollback
tools, must run before mounting or after unmounting.

### Managed Mount

`--managed` starts a detached supervisor that keeps the mount desired state as
mounted and remounts after unexpected worker exits:

```bash
skillfs mount /path/to/skills /mnt/skillfs --managed
skillfs stop /mnt/skillfs
```

`skillfs stop <MOUNTPOINT>` clears desired state, terminates the supervisor and
worker, and unmounts. It is idempotent and safe to run when the mount is already
stopped.

Managed mode also detects stale or dead FUSE endpoints after unexpected worker
termination, clears them, and remounts with bounded recovery retries. Default
foreground mounts are unchanged: they still exit and unmount on `SIGTERM` or
`Ctrl+C`.

### Mount Configuration

Aggregate explicit source directories with a TOML file:

```toml
mountpoint = "/mnt/skillfs"
sources = [
  "/path/to/workspace/skills",
  "/path/to/managed/skills",
  "/path/to/bundled/skills",
]
```

```bash
skillfs mount --config /path/to/skillfs-mount.toml
```

Sources are ordered from highest to lowest precedence. The first source containing
a skill directory name wins as a whole, including scripts and resources; files
from shadowed skills are not merged. Duplicate names are logged with both paths.
The view is exposed at `/mnt/skillfs/skills`, and discovery advertises mounted
paths. The existing directory-name identity is retained; this does not implement
OpenClaw's frontmatter-name precedence rules.

The file accepts only `mountpoint` and `sources`. Supply at least one source and
absolute paths (no `~` or environment expansion). Sources must exist and be
readable; canonical duplicate roots are removed, preserving order. Sources and
the mountpoint must not contain one another. Invalid skills or a combined skill
count above the existing limit fail startup.

Multiple distinct sources automatically enable read-only mounting. A single
source remains writable unless `--read-only` is supplied. This configuration
always uses the flat output layout, retaining the existing flat and categorized
source scanning depth. Only the first source's `skillfs-views.toml` is used;
read-only configuration mounts do not update that file. Skills absent from every
view are included in the effective default view in memory; explicitly assigned
secondary skills keep their grouping. No source config merge,
automatic source discovery, or hot reload is performed. Remount after changing
the config or adding/removing skills; reads are not an immutable content snapshot.

Mount configuration cannot be mixed with positional paths or security,
activation, installation, layout, or managed-mode options. The supported optional
flags are `--foreground`, `--allow-other`, `--read-only`, `--verbose`, and
`--log-file`. Existing `mount SOURCE MOUNTPOINT --config security.toml` usage is
unchanged for files without `sources` or `mountpoint`.

For OpenClaw, point the intended skill loader at the mounted `skills` directory
and verify actual loaded paths. Adding it as an extra directory alone does not
replace higher-priority original sources. Direct source access remains outside
this mount's coverage. Roll back by unmounting with `fusermount3 -u /mnt/skillfs`
and returning to the existing single-source command.

## CLI Utilities

### validate

```bash
skillfs validate /path/to/skills
skillfs validate /path/to/skills --format json
```

`validate` reports successful, degraded, and failed skill parses. Parse
failures are included in the status summary and produce a non-zero exit code;
degraded-only skills are reported but keep exit code 0.

In JSON output, error and warning entries include a `path` field so consumers
can locate the exact offending skill file.

### list and classify

```bash
skillfs list /path/to/skills
skillfs list /path/to/skills --enabled-only
skillfs classify /path/to/skills --primary-count 6
skillfs classify /path/to/skills --dry-run
```

`list` reports discovered skills and metadata. `classify` generates or previews
`skillfs-views.toml`; the first N skills go to the default view and the rest go
to a secondary view.

## Views and Discovery

`skillfs-views.toml` in the source directory controls visibility:

```toml
[[view]]
name = "major"
default = true
description = "Core skills shown directly in /skills"
skills = ["github", "notion", "slack"]

[[view]]
name = "other"
default = false
description = "Additional skills accessible through skill-discover"
skills = ["apple-notes", "blogwatcher"]
```

The default view appears directly in the mounted skill view. Secondary views
are listed by the virtual `skill-discover` skill, whose `SKILL.md` includes the
skill names and source paths.

Skills not assigned to any view are added to the default view on the next
mount.

## Read and Write Semantics

| Operation | Behavior |
| --- | --- |
| `readdir` | Controlled by views and runtime activation state |
| Read `SKILL.md` | Compiled content by default; the selected target's raw content when the directive stage is disabled with no other transform |
| Read ordinary files | Passes through to the physical source tree |
| Write `SKILL.md` | Writes through and reparses the store |
| Write ordinary files | Writes through without changing skill metadata |
| Rename skill directory | Uses the directory name as the authoritative key |
| Symlink or hardlink | Restricted to safe same-skill relative targets |
| `user.*` xattr | Conservative passthrough on ordinary paths |

In-place authoring supports newly created skill directories. A fresh directory
does not expose a phantom `SKILL.md` before the manifest exists; once
`SKILL.md` is written, SkillFS reparses it and exposes the compiled view.
Pending or direct-final installs can preserve ordinary top-level skill
directory metadata such as mode, timestamps, and ownership. `.skill-meta/**`
remains restricted to trusted metadata paths.

Without security integration, skills read from the live source tree. When
security activation is enabled, visibility is constrained by the active
mapping:

- current: read from the live source tree, for example through the legacy
  decision-command resolve path;
- fallback: read from a trusted snapshot under `.skill-meta`;
- hidden: hide the skill from ordinary callers.

In activation file mode, activation JSON expresses fallback and hidden states.
It does not write current/live state. If a skill has no activation JSON or
activation xattr in this mode, SkillFS treats it as hidden by fail-safe default.

### Permission Sources

Visibility decides **which** content is read; permissions decide **whether** it
can be read or written. Since 0.4.0 the two are resolved from different sources:

| Operation | Permission source |
| --- | --- |
| Agent-visible read | The activated target's own permissions — for fallback, the snapshot's |
| Write | The live source's permissions |

A skill can therefore be readable through its snapshot while its live source is
not writable. If you relied on reads following live-source permissions before
0.4.0, review the permission bits on your snapshots.

## Read-Time Transforms

After the activation target is resolved, `SKILL.md` bytes pass through an
ordered transform pipeline before an agent sees them:

1. The **directive** stage runs the conditional compiler (`@if` / `@else` /
   `@endif` plus heuristic command normalization). It is enabled by default;
   when present it always runs first, so output is unchanged from earlier
   releases. Disable it with `[transforms.directive] enabled = false`.
2. The optional **OS adapter** stage runs second and only on `SKILL.md`. It
   rewrites distribution-specific literals between Ubuntu/Debian and
   Alinux/Anolis conventions.

Both stages are optional: you can run both, directive-only (the default),
adapter-only (directive disabled), or neither — an empty pipeline serves the
selected raw bytes unchanged. Initialization diagnostics report the actual
enabled stage list.

| Directive | OS adapter | Agent-visible `SKILL.md` |
| --- | --- | --- |
| enabled (default) | disabled (default) | Legacy compiler output |
| enabled | enabled | Compiler output, then OS adaptation |
| disabled | enabled | OS adaptation of raw selected bytes |
| disabled | disabled | Raw selected bytes |

The pipeline only affects the bytes an agent reads. Source files, trusted
snapshots, activation metadata, and the rule artifact are never modified.
Hidden skills stay hidden and never enter the pipeline; a fallback read is
transformed from the trusted snapshot and never falls back to the live source.
The same pipeline and activation ordering applies to flat `<skill>/SKILL.md`
and Hermes `<category>/<skill>/SKILL.md` layouts. A snapshot read resolves,
reads, and transforms only the selected snapshot; if snapshot target parsing or
resolution fails, or its `SKILL.md` cannot be read, the operation returns an
error (`ENOENT` at the virtual read boundary) and never retries the live source.
`getattr` size, partial reads, and full reads always agree on the transformed
bytes. Only `SKILL.md` is adapted — other Markdown, shell, Python, and config
files pass through untouched.

### Disabling the Directive Stage

The directive/compiler stage stays enabled unless explicitly turned off:

```toml
[transforms.directive]
enabled = false
```

An absent `[transforms.directive]` section keeps directive compilation enabled,
so existing configurations are unaffected. Disabling it only affects the
compiler stage; the OS adapter remains independently opt-in.

### Enabling the OS Adapter

The OS adapter is disabled by default and configured through the existing
`--config <PATH>` TOML file (no extra CLI flags). When enabled without a
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

SkillFS ships a **built-in 311-rule Ubuntu/Alinux catalog** embedded in the
binary from the repository asset, so the adapter works in source builds, RPMs,
and containers without a separate file. It stays opt-in. The catalog contains
257 `auto_apply: always` rules and 54 `auto_apply: never` protection rules,
producing 223 active substitutions toward Alinux and 192 toward Ubuntu.
High-confidence rules are applied; medium- and low-confidence rules remain
protection-only.

- `target_os = "auto"` reads the exact `/etc/os-release` `ID` once at mount
  startup — `ubuntu`/`debian` map to Ubuntu, `alinux`/`anolis` map to Alinux.
  Detection is fail-closed: `ID_LIKE` is not consulted, so RHEL-family
  derivatives (Rocky, AlmaLinux, CentOS, …) are not silently treated as Alinux,
  and unrecognized hosts reject the mount. Set `ubuntu` or `alinux` explicitly
  on other distributions.
- `rules_path` is an optional external override. Omit it to use the built-in
  catalog; set a non-empty path to load an external read-only artifact instead.
  A present-but-blank path is rejected, not treated as the default. SkillFS
  loads and validates the chosen artifact once at startup; the per-read path
  performs only in-memory substitution and never parses YAML, reads
  `/etc/os-release`, spawns processes, or makes network/LLM calls.
- TOML controls which stages run, the target OS, and the rule artifact. The YAML
  artifact controls individual mappings and eligibility. There is no per-rule
  TOML switch.

### Enabling Protected Rules and Adding Custom Rules

The rule artifact — built-in or external — is a top-level YAML sequence. Each
rule declares the literal for each OS side, a `direction`, and a required
`auto_apply` flag:

```yaml
- ubuntu: "apt-get install -y "
  alinux: "dnf install -y "
  direction: bidirectional          # bidirectional | ubuntu_to_alinux_only | alinux_to_ubuntu_only
  match: literal                    # literal | token — optional, defaults to literal
  auto_apply: always                # always | never — REQUIRED
```

`rules_path` is a **complete replacement**, not an overlay. To retain all
built-in mappings and customize only selected entries, copy the repository asset
from a source checkout:

```bash
cp src/skillfs/crates/skillfs-core/assets/ubuntu-alinux.yaml \
  /etc/skillfs/ubuntu-alinux.custom.yaml
```

Then set `rules_path = "/etc/skillfs/ubuntu-alinux.custom.yaml"` in the TOML
configuration. An absolute path avoids dependence on the mount process working
directory.

To opt a protected medium- or low-confidence rule into local policy, change its
`auto_apply` value in the copied artifact. For example:

```yaml
- ubuntu: "ufw"
  alinux: "firewalld"
  direction: ubuntu_to_alinux_only
  auto_apply: always
  confidence: low
  notes: "enabled by local policy"
```

Append complete entries to define local mappings:

```yaml
- ubuntu: "acme-agent-dev"
  alinux: "acme-agent-devel"
  direction: bidirectional
  auto_apply: always
  confidence: high
  notes: "local package mapping"
```

`ubuntu`, `alinux`, `direction`, and `auto_apply` are required.
`match` is optional; `confidence` and `notes` are optional inert annotations.
The external file must also retain any built-in rules you still want: SkillFS
does not merge it with the embedded catalog. Rules are loaded once when the
mount starts; remount after editing the file. There is currently no catalog
overlay, hot reload, per-rule identifier, or export command.

- `auto_apply` is required on every rule, including external override artifacts;
  only `auto_apply: always` rules are applied, and only in a direction the
  resolved target allows. An artifact that omits `auto_apply` is rejected with an
  error naming the rule index.
- `confidence` and `notes` are accepted as annotations with no behavior —
  eligibility is governed solely by `auto_apply`.
- `match` defaults to `literal`, preserving substring matching for existing
  artifacts. `match: token` requires ASCII-alphanumeric boundaries at
  alphanumeric source edges in both directions: `cron` matches at EOF or before
  whitespace/newlines/punctuation, but not inside `micron`, `crontab`,
  `cronutils`, or `cron2`.
- Substitution is a single non-cascading pass; at each position the longest
  matching pattern wins, so overlapping patterns never chain and file order does
  not affect the result.
- Ineligible patterns (`auto_apply: never`, identity, or direction-disallowed)
  still match and are emitted unchanged, protecting their whole span so a shorter
  eligible rule cannot rewrite inside them. Protection is deduplicated by
  `(source, match)`: a substitution removes protection only for the same source
  and mode. Different modes coexist; substitution wins only when its own mode
  matches the input, otherwise matching protection still preserves the span.
- A many-to-one forward mapping must resolve reverse ambiguity explicitly: mark
  one pair `bidirectional` (canonical reverse) and the alternates
  `ubuntu_to_alinux_only`. Colliding `bidirectional` reverses are rejected.

When enabled, a missing/unreadable external `rules_path`, a blank `rules_path`,
malformed YAML, a missing or invalid `direction`/`auto_apply` value, an invalid
`match` value, duplicate or ambiguous patterns, or an unrecognized
`target_os = "auto"` host reject the mount before it starts with an actionable
error.

## Security Integration

### Activation File Mode

Use activation file mode when an external daemon receives SkillFS mutation
events, scans the source tree, and writes activation metadata:

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --foreground \
  --security \
  --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --activation-events-log /var/log/skillfs/activation-events.jsonl \
  --activation-reload-mode poll
```

Flow:

```text
Agent or installer writes through SkillFS
  -> SkillFS sends a notify event
  -> Skill Ledger scans and writes activation state
  -> SkillFS reloads activation state
  -> the skill becomes live, fallback, or hidden
```

`--activation-reload-mode poll` requires `--notify-socket` or
`--activation-events-log`, because SkillFS needs a trigger source for polling.

`--notify-socket` points at a socket the **external daemon** listens on, not one
SkillFS creates. In a joint deployment with Skill Ledger this is the
agent-sec-core daemon endpoint, which defaults to
`$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock` and can be overridden with
`AGENT_SEC_DAEMON_SOCKET`. Note that a failed notify delivery is only a warning
and never stops the FUSE service, so pointing at the wrong path shows up as
skills staying hidden rather than as an obvious error.

For in-place activation and notify mounts, set `--ledger-backing-root` to a
daemon-visible backing source path and enable the authenticated resolver.
Notify v2 carries canonical identity only, so startup rejects an in-place
notify configuration that omits authenticated control-peer configuration. The same resolver
requirement applies to an out-of-place notify mount whenever it explicitly
configures `--ledger-backing-root`:

```bash
skillfs mount /path/to/skills /path/to/skills \
  --security-mode \
  --security \
  --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --trusted-peer-exe /usr/bin/python3.11 \
  --ledger-backing-root /run/user/$UID/skillfs-ledger/source
```

Avoid `/tmp` and `/var/tmp` for daemon integration paths when the daemon runs
with `PrivateTmp=true`; those paths are invisible to the daemon and rejected by
startup validation.

### Control Socket

The trusted control socket is the preferred production path for activation
writes and for the read-only resolver query:

```bash
skillfs mount /path/to/skills /mnt/skillfs \
  --security \
  --activation-mode file \
  --control-socket /run/skillfs/control.sock \
  --trusted-peer-exe /usr/bin/python3.11
```

The socket requires `--security --activation-mode file`, is mutually exclusive
with `--decision-command`, and requires exactly one peer authentication mode.
The host mode uses Linux peer credentials and executable identity checks.

The packaged AgentSecCore daemon starts the Skill Ledger worker with
`sys.executable`, which resolves to `/usr/bin/python3.11`; the worker is not a
`/usr/bin/skill-ledger` executable. For a custom virtual environment, run the
following with the exact interpreter that starts the daemon and configure the
real path it prints:

```bash
/path/to/ledger/python -c 'import os, sys; print(os.path.realpath(sys.executable))'
```

This M1 executable gate trusts that Python interpreter, not a particular
module. Keep SkillFS and the Ledger worker in the same UID/security domain and
account for the fact that another process under that UID using the same
interpreter also satisfies the executable identity check.

#### Endpoint and priority

The control plane is opt-in and authenticated. The endpoint is resolved by
priority:

1. CLI `--control-socket <PATH>`
2. `[control_socket].path` in the config file
3. the default per-user endpoint `/run/user/<uid>/skillfs/control.sock`

An executable peer with no explicit path uses the default endpoint; HMAC mode
requires an explicit path. An explicit path with no peer mode is a
configuration error; neither leaves the control plane off. The default endpoint
never falls back to `/tmp` or `/var/tmp` — if
`/run/user/<uid>` is unavailable, startup fails with an actionable error and
you must pass `--control-socket` explicitly. A second instance never unlinks an
active endpoint; only a confirmed-stale socket that SkillFS owns is reclaimed.

No `register`, `mountId`, or `generation` handshake is required — the endpoint
is stable per UID and the resolver is queried directly.

#### Container HMAC profile

The SkillFS side can use the HMAC profile when a trusted peer runs in a
separate PID or mount namespace. Both processes must read the same secret from
private files mounted only into the trusted containers:

```bash
skillfs mount /var/lib/skillfs/source /var/lib/skillfs/shared/mount \
  --foreground --allow-other \
  --security --activation-mode file \
  --notify-socket /run/anolisa/peer/notify.sock \
  --notify-auth-key-file /run/anolisa/auth/skillfs.key \
  --control-socket /run/anolisa/skillfs/control.sock \
  --trusted-peer-key-file /run/anolisa/auth/skillfs.key
```

The secret file must be an absolute, nonblocking, no-follow regular file owned
by the effective user, grant no group or other permissions, and contain
32–4096 raw bytes. FIFO and other non-regular candidates fail startup without
blocking. HMAC mode does not fall back to executable or plain authentication.
After mutual authentication, session-bound tags protect both raw business
requests and responses before either side interprets them.
For authenticated notify, the daemon socket must be owned by SkillFS's
effective UID, grant no group or other permissions, and live directly under an
owner-matched directory that also grants no group or other permissions; `0700`
is the recommended directory mode. The compatible agent-sec-core listener
creates that endpoint with mode `0600`; SkillFS checks these properties before
every connection. This initial profile therefore requires SkillFS and
agent-sec-core to run with the same effective UID. Mount the runtime socket
volume read-write only into the trusted containers. A negative-test workload
may receive it read-only, but never with permission to replace socket entries.
SkillFS and a resolver peer must see the physical source at the same absolute
path because the resolver continues to return `transport: shared_path`. Do not
mount the source or Secret into the workload container.

This repository change implements only the SkillFS server/client surfaces. It
does not configure or implement agent-sec-core. The peer-side behavior is the
proposed contract in
[Container Peer Authentication](../../../../src/skillfs/docs/design/container-peer-authentication.md)
and remains subject to sec-core maintainer confirmation. Until that follow-up
lands, use the independent probe and
[unilateral validation plan](../../../../src/skillfs/docs/testing/container-peer-authentication-unilateral-validation.md)
instead of treating sec-core integration as available.

Supported JSONL request examples:

```json
{"schemaVersion":"1","method":"ping"}
{"schemaVersion":"1","method":"status"}
{"schemaVersion":"1","method":"meta.writeActivation","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
{"schemaVersion":"1","method":"meta.setActivationXattr","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
{"schemaVersion":"1","method":"skill.resolveLiveSource","canonicalSkillDir":"/path/to/skills/apple/apple-notes"}
```

#### `skill.resolveLiveSource`

A read-only query that maps a canonical Skill directory to its physical
live/backing source. The only business parameter is `canonicalSkillDir`. It has
three distinct outcomes:

- **`managed=true`** — the path is inside the managed canonical root and
  resolves to a valid live Skill directory. The response includes the derived
  `skillId`, `relativeSkillDir`, the physical `liveSkillDir`, the live
  directory's `identity` (`device`, `inode`), and `transport` (`shared_path`).
  The query is read-only: it triggers no scan, manifest build, policy decision,
  or activation write.
- **`managed=false`** — the request is well-formed and `canonicalSkillDir` is a
  valid absolute path outside the managed root (`reason: not_managed`). This is
  a normal success; the caller may manage that directory directly.
- **structured error** — a non-absolute or non-normalized path (including
  repeated or trailing `/`), an illegal `..` segment, a symlink/path escape, a
  management/reserved directory, a missing Skill directory, an invalid layout /
  missing `SKILL.md`, an unreadable live source, or peer-authentication failure.
  These are never disguised as `managed=false`.

The skill id is derived from the canonical relative path, so both flat
(`my-skill`) and Hermes nested (`apple/apple-notes`) layouts resolve to full
ids. S1 implements a single source runtime; the endpoint is shared across
future multiple canonical roots.

> Note: `skill.resolveLiveSource` (SkillFS S1) is a read-only resolver. notify
> v2 and deletion-state semantics are not part of S1.

#### Notify v2

`skill_ledger.skillfs_notify_change` uses schema version 2. Its business
payload contains only `canonicalSkillDir`, the complete `skillId`, `eventKind`,
and relative `paths`. Flat ids stay intact (`weather`), and Hermes ids retain
both components (`category/weather`). SkillFS sorts and deduplicates paths; an
empty array requests a whole-Skill rescan, including when the path limit is
exceeded.

The canonical directory is derived from the absolute, lexically normalized
source identity without following a source-root symlink. The physical
live/backing root remains private to activation and the S1 resolver, so backing
paths never appear in notifications. The daemon must accept v2 directly and
return `schemaVersion=2` with `accepted=true`; there is no v1 fallback or
negotiation.

### Trusted Mount-path Writer

`--trusted-writer-exe <PATH>` is a compatibility gate for trusted writers that
write through the mount path. Prefer the control socket for new production
integrations.

`--trusted-writer <NAME>` is deprecated and only matches the Linux process
`comm` name. Use executable identity when compatibility allows it.

### Decision-command Mode

`--security --decision-command <COMMAND>` is the legacy compatibility path.
SkillFS invokes the external command for scan and resolve decisions.

Decision-command mode is mutually exclusive with activation file mode,
`--notify-socket`, `--activation-events-log`, `--ledger-backing-root`, and
`--control-socket`.

## Install Protocols

SkillFS supports installer-friendly lifecycle paths:

- staging roots can be hidden from ordinary listing while exact staging paths
  remain writable;
- direct-to-final installs can remain hidden until activation appears;
- `/.skillfs-inbox/<skill>/...` is an install or repair entry point for hidden
  or new skills; writes land in the source tree and can trigger the external
  security flow;
- quiet-timeout notification can aggregate install mutations after a configured
  quiet window;
- post-publish grace can allow bounded installer metadata writes after publish;
- post-publish grace paths for fallback skills are routed to the live source so
  installers can finish metadata updates after publish.

These behaviors are configured through the SkillFS TOML config and require a
notify source such as `--notify-socket` or `--activation-events-log`.

## Observability

### Audit and Activation Logs

`--audit-log <PATH>` writes filesystem audit events as JSONL.
`--activation-events-log <PATH>` writes activation protocol events as JSONL for
daemon-driven activation flows.

When the OS adapter is enabled, a successful read-only Open of a virtual flat
or Hermes `SKILL.md` includes content-free adapter context in `detail`:
`transform=os_adapter target_os=<target> rule_digest=<sha256>`. It records only
the enabled stage, resolved target OS, and rule-artifact digest — never source
content, transformed content, a diff, or rule literals. Successful per-syscall
Read events remain suppressed to avoid high-volume audit flooding.

### SLS Ops and Runtime Metrics

SkillFS writes best-effort SLS records to:

```text
/var/log/anolisa/sls/ops/skillfs.jsonl
```

The file is owned and pre-created by the deployment/SLS component. SkillFS only
appends when the file exists; it never creates the file or parent directory, and
write failures do not change CLI or FUSE behavior.

The following CLI commands append ops records: `mount`, `list`, `validate`, and
`classify`. While a mount is alive, runtime metric records use
`record_type = "runtime_metric"` and include mount lifecycle, view pruning,
skill hits, and security policy outcomes. The legacy mount-session summary
shares the same file for compatibility.

## Common Options

| Option | Purpose |
| --- | --- |
| `--foreground` | Run in the foreground |
| `--managed` | Start a detached supervised mount |
| `--security-mode` | Require source and mountpoint to be the same path |
| `--skill-layout <MODE>` | `auto` (default, detect Hermes from source-root markers), `flat`, or `hermes`; `hermes` is incompatible with `--decision-command` |
| `--security` | Enable security integration |
| `--activation-mode file` | Consume activation JSON/xattr state |
| `--activation-reload-mode poll` | Poll activation after notify triggers |
| `--notify-socket <PATH>` | Send mutation events to an external daemon |
| `--notify-auth-key-file <PATH>` | Mutually authenticate notify connections with an owner-only shared key file |
| `--activation-events-log <PATH>` | Write activation protocol events as JSONL |
| `--audit-log <PATH>` | Write filesystem audit events as JSONL |
| `--audit-queue-capacity <N>` | Queue size for the audit writer thread; `0` uses the built-in default, and it only applies with `--audit-log` |
| `--events-log <PATH>` | Write legacy security decision events as JSONL; only applies with `--security --decision-command` |
| `--control-socket <PATH>` | Override the control socket endpoint (default for executable mode: `/run/user/<uid>/skillfs/control.sock`) |
| `--trusted-peer-exe <PATH>` | Pin the trusted control socket peer (enables the control plane on the default endpoint if no path is given) |
| `--trusted-peer-key-file <PATH>` | Enable mutually authenticated container peer mode; requires an explicit control socket and is mutually exclusive with `--trusted-peer-exe` |
| `--trusted-peer-uid <UID>` | Additionally constrain the control socket peer's UID (from `SO_PEERCRED`) |
| `--trusted-peer-gid <GID>` | Additionally constrain the control socket peer's GID (from `SO_PEERCRED`) |
| `--trusted-writer-exe <PATH>` | Pin a trusted mount-path writer |
| `--ledger-backing-root <PATH>` | Provide a daemon-visible source view |
| `--decision-command <CMD>` | Use legacy external decision mode |
| `--pid-file <PATH>` | Write a process pid file |
| `--allow-other` | Allow other users to access the FUSE mount |
| `--read-only` | Reject mutations through the FUSE mount with `EROFS`; the separate physical source remains writable by its owner |
| `--config <PATH>` | Load SkillFS TOML configuration |
| `-v`, `--verbose` | Enable debug logging |
| `--log-file <PATH>` | Write logs to a file |

## Troubleshooting

**A newly installed skill is not visible.**
With security activation enabled, new skills can remain hidden until the ledger
writes activation state. Check notify delivery and activation reload events.

**Fallback reads an older version.**
Fallback intentionally reads a trusted snapshot under `.skill-meta`, not the
live source tree.

**`.skill-meta` is not listed.**
This is expected for ordinary callers. Trusted peers can access metadata through
the configured trusted path.

**Notify socket failures appear in logs.**
Notify failures are warnings and do not stop FUSE service, but the external
daemon may miss mutation events until the socket is fixed.

**In-place activation fails at startup.**
Check that `--ledger-backing-root` is set and visible to the daemon. Avoid
`/tmp` and `/var/tmp` with services that use `PrivateTmp=true`.

**A managed mount survived the launcher restart.**
That is expected. Stop it with `skillfs stop <MOUNTPOINT>`.

## More References

- [SkillFS README](../../../../src/skillfs/README.md)
- [External decision protocol](../../../../src/skillfs/docs/security/external-decision-protocol.md)
- [Runtime activation plan](../../../../src/skillfs/docs/security/runtime-activation-implementation-plan.md)
- [FUSE crate layout](../../../../src/skillfs/docs/architecture/fuse-crate-layout.md)
