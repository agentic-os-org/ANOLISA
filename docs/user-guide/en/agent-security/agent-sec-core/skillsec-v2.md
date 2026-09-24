# SkillSec V2

[中文版](../../../zh/agent-security/agent-sec-core/skillsec-v2.md)

SkillSec scans Skill content, signs its results, retains recoverable versions and publishes the
selected version to SkillFS. Built-in scanning runs locally without model calls. The Rust CLI keeps
the `agent-sec-cli skill-ledger` entry point and sends operations to one root daemon.

This page describes the V2 core. The [Skill Ledger guide](skill-ledger.md) describes V1 and its
Agent integrations. V2 Agent Hook migration is separate: existing Hook initialization checks,
defaults and enablement are not updated by this core migration.

## Installation boundary

The normal released-component route is `sudo anolisa --install-mode system install sec-core`, followed by
the Alinux RPM alternative described in [installation](QUICKSTART.md#installation). These routes
select published artifacts; they do not select this unreleased V2 migration branch. Do not assume
that installing a release supplies SkillSec V2.

For a V2 RPM, build this branch with the repository recipe on Linux. The full recipe also builds
the unchanged sandbox and plugin packages and needs Node.js 22.14 or later, Rust 1.93 or later,
RPM tools, systemd RPM macros, a C compiler, pkg-config and OpenSSL development headers:

```sh
./scripts/rpm-build.sh agent-sec-core-v2
sudo yum install ./scripts/rpmbuild/RPMS/x86_64/agent-sec-cli-*.rpm
```

Use the matching architecture directory on aarch64. Install the core CLI package for this phase;
the metapackage also pulls Agent integrations that have not been migrated here. V1 and V2 CLI
packages use the same package name and executable paths, so installing V2 replaces V1. Stop V1
writers and retain a matching backup first, as described under deployment rollback below.

For a core-only source installation, use Linux and Rust 1.93 or later. From the repository root:

```sh
make -C src/agent-sec-core build-cli-v2
sudo make -C src/agent-sec-core install-core-v2
```

This produces `src/agent-sec-core/target/v2/bin/agent-sec-cli` and `agent-sec-daemon`.
The core binaries require no Python Ledger runtime. `install-core-v2` installs both binaries into
`/usr/bin`, the system unit into `/usr/lib/systemd/system`, and an initial 0600 configuration into
`/etc/agent-sec/skillsec.json`. Reinstallation preserves an existing configuration; RPM uses
`%config(noreplace)`. Installation does not initialize a signing key or enable Agent Hooks.

RPM follows the distribution's systemd preset and may enable this service at boot. Review its
configuration, then explicitly start the root system service:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now agent-sec-core.service
sudo systemctl status agent-sec-core.service
agent-sec-cli skill-ledger status
```

Use system `systemctl`, without `--user`. The unit creates runtime/state/log directories, keeps
the key and logs private, and grants only `CAP_DAC_OVERRIDE`, `CAP_CHOWN` and `CAP_FOWNER` to the
daemon. It retains `NoNewPrivileges`, `SystemCallFilter=@system-service`, native syscall
architecture, `MemoryDenyWriteExecute` and kernel protections, while keeping HOME, `/tmp`, system Skill
directories and shared mounts accessible for scanning, metadata publication and rollback. It does
not grant `CAP_SYS_ADMIN` to the daemon. Custom SkillFS mounts must be visible to the service. In-place SkillFS backing setup requires kernel support for `open_tree` and `move_mount`,
and access to `/proc/self/fd`. Unsupported or denied mount operations fail closed. The daemon
does not need an additional mount capability.

Use an isolated deployment for evaluation: V1 and V2 must never write the same Skill metadata
concurrently. Implementation and actual Linux delivery acceptance are tracked separately in the
[migration document](../../../../../src/agent-sec-core/docs/design/SKILL_SEC_PHASE_ONE.md).

`V2_CARGO_TARGET_DIR` selects a shared Cargo build cache; `V2_BIN_BUILD_DIR` selects the binary
staging directory. Pass the same staging override to build and install. To validate an existing
Linux build's staged installation and preservation of configuration:

```sh
bash src/agent-sec-core/tests/packaging/test-skillsec-install.sh
```

This staging check does not start systemd or establish live installation acceptance.

For a foreground development daemon, first create a root-owned runtime directory with mode 0755
and an empty root-owned state directory with mode 0700. Supply the absolute socket through
`agent-sec-daemon serve --socket /run/agent-sec-core/daemon.sock` and optionally supply
`--skillsec-config /etc/agent-sec/skillsec.json`. The process must run as root. Its runtime
directory must already exist; it creates the private state directory if absent.

## System configuration and trust

The daemon reads root-owned `/etc/agent-sec/skillsec.json`; `--skillsec-config` selects another
absolute file. Parent directories must be trusted, the file must not be group/world writable,
and symlinks or multiply linked configuration files are rejected. A missing default file uses
the following defaults; a malformed or unsafe file fails startup.

```json
{
  "stateDir": "/var/lib/agent-sec/skillsec",
  "managedSkillDirs": []
}
```

`managedSkillDirs` contains exact Skill roots, not parent-directory wildcards. Explicit scan or
certify operations register their root. `init`, `scan --all` and `check --all` also discover the
caller's default Skill locations and combine them with the daemon registry. A single explicit root
never registers its siblings. `status` describes the shared registry, independent of caller HOME.
Default discovery skips hidden child directories, including Ledger snapshots; legitimate Skill
locations beneath host directories such as `.hermes/skills` remain supported.
It includes direct Skill children of `$XDG_DATA_HOME/anolisa/skills`, using the CLI caller's
environment. If `XDG_DATA_HOME` is unset, empty, relative, or contains `.`/`..` path segments, it
uses `$HOME/.local/share/anolisa/skills`, matching ANOLISA's user installation layout. If `HOME`
is unset or empty, it uses the CLI user's system account home directory. A valid
override whose directory is absent is skipped without falling back. Explicit Skill paths and
`init --no-baseline` do not invoke discovery.

The state directory is root-owned 0700; `signing-key.pk8` is 0600. All local users may operate all
managed Skills in phase one. Ownership is not an authorization ACL. Only kernel UID 0 may rotate
the key. The key signs daemon-computed records, not arbitrary caller-provided bytes.

V1 history, encrypted keys and keyrings are not imported. New manifests bind the canonical
absolute Skill identity. Moving a Skill to another identity requires establishing trust there.
Different Skills with the same directory name retain separate identities.

## Core workflow

Run these commands as the ordinary user whose Skill is being managed. `init --no-baseline`
initializes only the shared key. `scan` also initializes a missing key; neither operation silently
replaces a damaged key.

```sh
agent-sec-cli skill-ledger init --no-baseline
agent-sec-cli skill-ledger list-scanners
agent-sec-cli skill-ledger analyze /path/to/skill
agent-sec-cli skill-ledger scan /path/to/skill
agent-sec-cli skill-ledger check /path/to/skill
agent-sec-cli skill-ledger show /path/to/skill
agent-sec-cli skill-ledger audit /path/to/skill --verify-snapshots
agent-sec-cli skill-ledger status --verbose
```

`analyze` is read-only and does not initialize keys or write a Ledger. The default built-ins are
`code-scanner` and `static-scanner`. `scan --scanners code-scanner,static-scanner` selects them
explicitly. Changed content creates a version; unchanged content fills missing scan results.
`scan --force` replaces scan results for unchanged content without inventing a content version.

The six integrity states are:

| State | Meaning |
| --- | --- |
| `none` | Content has no authenticated scan verdict yet |
| `pass` | Authenticated content has a passing scan result |
| `warn` | Authenticated content has warning findings |
| `deny` | Authenticated content has denying findings |
| `drifted` | Current content differs from the authenticated record |
| `tampered` | Ledger metadata, signature or version binding fails authentication |

Execution errors and `unmanaged` diagnostics are not additional integrity states. A successful
scan means the scan completed, not that its security verdict is `pass`.

Before the first key initialization, checking a Skill with no Ledger returns `none` and exit 0;
auditing its empty history also succeeds. Neither query creates keys or metadata. Existing Ledger
artifacts with missing or invalid keys do not qualify as an empty history.

## External findings, decisions and recovery

Import an external JSON findings report through the CLI. Custom `skill`, `cli` or `api` scanner
entries are import-only; the root daemon never executes their configured commands.

```sh
agent-sec-cli skill-ledger certify /path/to/skill --findings /path/to/findings.json --scanner skill-vetter
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason 'Reviewed this version'
agent-sec-cli skill-ledger decide /path/to/skill --action always_allow
agent-sec-cli skill-ledger decide /path/to/skill --action block
agent-sec-cli skill-ledger decide /path/to/skill --action rollback --version v000001
agent-sec-cli skill-ledger decide /path/to/skill --clear
agent-sec-cli skill-ledger export /path/to/skill --version v000001 --output /path/to/empty-export
agent-sec-cli skill-ledger activate /path/to/skill
```

`allow` approves the selected version; `always_allow` persists the broader manual approval.
`block` hides exposure. `rollback` restores the selected authenticated snapshot and records the
decision; `--clear` removes the manual decision. Rollback restores files to the Skill directory
owner so that its user can edit them afterward; privileged file mode bits are stripped.

Export writes `snapshot/`, `manifest.json` and `findings.json` to an empty caller-owned directory.
The CLI creates missing export directories as the caller with mode 0700 and leaves existing
directory permissions unchanged. It refuses a nonempty
destination. `--delete-findings` deletes the imported local report only after successful
certification and confirmation that the report has not changed.

`show` exposes `active`, `pending` or `hidden` selection. With `pass_warn_only`, an eligible
authenticated `pass` or `warn` version may be exposed automatically; unsafe latest content may
retain an eligible previous version or a pending view. Manual decisions affect that selection.
`activation.activationPending=true` means Ledger work committed but publication did not complete.
Inspect `show` and retry `activate` after repairing the reported cause. Daemon restart reconciles
supported interrupted publication and rollback states. No durable notification queue is promised.

Startup recovery covers registered Skills. If the first scan reports a registration failure after
writing its version, resolve that failure and retry `scan` for the same Skill. Unchanged content
reuses the verified version and completes registration before activation; a failed first request
is not guaranteed to be discovered automatically after restart.

## SkillFS binding

SkillFS uses the same public socket as ordinary CLI calls. Its notify handshake and messages retain
their existing HMAC contract; ordinary V2 JSON remains strict. The public socket must be root-owned
0666 under a root-owned 0755 directory. SkillFS also checks ancestor safety and the actual kernel
peer UID. The permission change does not permit plaintext fallback or bypass HMAC.

Configure explicit canonical/backing mappings in the daemon:

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

Use a separate random HMAC secret, 32–4096 bytes, never the signing key. The daemon's copy must be
root-owned 0600. Provision the same bytes in a separate 0600 file owned by the SkillFS user; do not
make the daemon's private file world-readable. For this UID-1000 example, set SkillFS notify
`socket_path` to `/run/agent-sec-core/daemon.sock` and its `auth_key_file` to that private user copy.
Its control socket remains UID-1000 owned, parent 0700/socket 0600; set control
`trusted_peer_uid=0` and `trusted_peer_key_file` to the same private copy. Use HMAC authentication;
leave `trusted_peer_exe` unset because executable authentication is a separate, mutually exclusive
mode. Configure SkillFS activation
and its backing root following the [SkillFS runtime reference](../../../../../src/skillfs/docs/security/runtime-activation-implementation-plan.md),
using the V2 daemon endpoint and peer identity above instead of that document's older examples.

Both processes must see the configured absolute paths and the same backing filesystem objects.
A container deployment therefore needs matching shared-volume paths. Canonical and live roots
may be identical for one ordinary non-in-place mount; otherwise they must be disjoint and
must not overlap other configured mappings. Resolver failure does not fall back to scanning the
FUSE view. A mismatched directory inode is rejected instead of certifying replacement content.

Notifications coalesce per Skill, then trigger scan followed by activation. A notify acknowledgment
confirms queue admission only. `status` includes `skillfs` queue/running/processed/failed counters
when configured. Check those counters, public audit and actual FUSE reads to distinguish admission,
completed processing and visible publication. Startup reschedules registered mount Skills.

## Output, audit and limits

The CLI prints business JSON to stdout. `check` returns 1 for `deny`, `tampered` or execution error;
completed scan/certify and complete analyze return 0 even for risk findings. Incomplete analyze
returns 1; invalid analyze input returns 2. Transport failures go to stderr and return 1. Always
inspect verdict and activation fields as well as the process exit code.

Use `--socket` or `AGENT_SEC_DAEMON_SOCKET` for an explicit endpoint. SkillSec defaults to 60
seconds; `--timeout-ms` is capped at 120 seconds server-side. A timeout does not establish whether
a write committed. Read status/history before retrying a mutation. Oversized responses return
`ResponseTooLarge` and `operationMayHaveCommitted=true`, rather than truncated data.

Public audit uses the shared Action Runtime and the private `/var/log/agent-sec` event store.
Its projection contains operation, counts, verdict, version and error class; it omits source code,
raw findings, local paths, manual reasons and keys. Full business data remains in the CLI response.
Built-in scans are bounded to 2,000 files, 50 MiB and depth 32; findings import is limited to 2 MiB.
Coverage failures remain visible instead of certifying partial content.

## Key rotation and deployment rollback

```sh
sudo agent-sec-cli skill-ledger rotate-keys
agent-sec-cli skill-ledger scan --all
```

Rotation withdraws registered exposure before replacing the key. Earlier signatures then lose
trust, including earlier V2 signatures. Rescan and activate to rebuild trust; no previous-key
verification fallback exists. If withdrawal fails, the old key remains and ordinary Ledger writes
are fenced until root retries or startup recovery completes.

Before changing an existing deployment, stop its daemon and SkillFS writers, retain its matching
binary/configuration, and back up private key/state plus each Skill's content and `.skill-meta`.
Use copies for migration evaluation. To return to V1, stop V2 and restore the matching V1 files,
keys, configuration and metadata before starting V1/SkillFS. Do not point V1 at V2 manifests or
mix restored old metadata with modified source content. Keeping a binary alone is not a state
rollback, and starting both daemons is not an upgrade strategy.
