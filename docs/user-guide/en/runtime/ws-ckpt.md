# Workspace Checkpoints (ws-ckpt)

ws-ckpt provides millisecond-level workspace checkpoint and rollback for AI Agents. It leverages filesystem COW (Copy-on-Write) to create instant snapshots of the working directory, enabling safe experimentation and fast recovery.

---

## Overview

When AI Agents modify code, configurations, or data files, mistakes can be costly. ws-ckpt allows Agents (and users) to:

- Create instant snapshots before risky operations
- Roll back to any previous checkpoint in milliseconds
- Compare differences between checkpoints
- Auto-checkpoint via plugin integration

---

## Prerequisites

- Linux (x86_64 or aarch64)
- btrfs filesystem on the workspace volume (for native COW snapshots), or any filesystem (ws-ckpt will create a btrfs loop image automatically)
- Agent runtime: OpenClaw (>= 2026.2.13) or Hermes (for plugin mode)

---

## Installation

### Option 1: anolisa CLI (recommended)

```bash
sudo anolisa --install-mode system install ws-ckpt
```

### Option 2: YUM (Alinux, requires ANOLISA YUM repo)

```bash
sudo yum install ws-ckpt
```

### Option 3: Source build (developers)

```bash
cd src/ws-ckpt && make build
```

---

## Plugin Installation

Install the ws-ckpt plugin for your Agent runtime:

```bash
# For OpenClaw
ws-ckpt plugin install --runtime openclaw

# For Hermes
ws-ckpt plugin install --runtime hermes

# Uninstall
ws-ckpt plugin uninstall --runtime openclaw
```

`plugin install` first runs a detect script to verify prerequisites (exit 2 = missing prerequisite, abort; exit 1 = not installed but installable, continue), then runs the install script. Scripts live under `/usr/share/anolisa/adapters/ws-ckpt/<runtime>/`.

The OpenClaw plugin requires OpenClaw >= 2026.2.13, the first release whose config write path avoids persisting runtime defaults and restores unchanged `${VAR}` references before writing to disk. OpenClaw >= 2026.9.1 uses conditional config writes. Versions from 2026.2.13 to <2026.9.1 use plain JSON writes only when the root config does not contain `$include`; otherwise installation stops before installing the plugin and requires an OpenClaw upgrade. An unparseable version, missing write capability, or failed allowlist update also aborts installation rather than leaving a partial integration. Uninstall remains best-effort: it removes local plugin files but warns and skips plugin unregister and allowlist cleanup when no safe config write is available.

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `ws-ckpt init -w <workspace>` | Initialize a workspace for checkpointing |
| `ws-ckpt checkpoint -w <workspace> -s <snapshot-id> -m <message> [--metadata <json>]` | Create a new checkpoint |
| `ws-ckpt rollback -w <workspace> -s <snapshot> [--preview]` | Restore workspace to a checkpoint |
| `ws-ckpt rollback -w <workspace> -n <num-ancestors>` | Rollback N ancestors |
| `ws-ckpt list [-w <workspace>] [--format table\|json]` | List all checkpoints |
| `ws-ckpt diff -w <workspace> -f <from> [-t <to>]` | Show differences between checkpoints |
| `ws-ckpt delete [-w <workspace>] -s <snapshot> [--force]` | Delete a specific checkpoint |
| `ws-ckpt status [-w <workspace>] [--format table\|json]` | Show current workspace status |
| `ws-ckpt cleanup -w <workspace> [--keep 20]` | Remove old checkpoints |
| `ws-ckpt config [-g \| -w <workspace>] [--enable-auto-cleanup] [--auto-cleanup-keep <N\|Nd>]` | View/edit configuration |
| `ws-ckpt plugin install --runtime openclaw\|hermes` | Install runtime plugin |
| `ws-ckpt plugin uninstall --runtime openclaw\|hermes` | Uninstall runtime plugin |
| `ws-ckpt recover [-w <workspace> \| --all] [--force]` | Recover from interrupted operations |
| `ws-ckpt reload` | Reload daemon configuration |
| `ws-ckpt daemon [--mount-path ...] [--socket ...] [--log-level ...]` | Start the daemon process |

### Examples

```bash
# Initialize a workspace
ws-ckpt init -w /home/user/projects/my-project

# Create a checkpoint
ws-ckpt checkpoint -w /home/user/projects/my-project -s snap-001 -m "before refactor"

# List checkpoints
ws-ckpt list -w /home/user/projects/my-project

# Diff between two snapshots
ws-ckpt diff -w /home/user/projects/my-project -f snap-001 -t snap-002

# Rollback to a specific checkpoint
ws-ckpt rollback -w /home/user/projects/my-project -s snap-001

# Preview rollback without applying
ws-ckpt rollback -w /home/user/projects/my-project -s snap-001 --preview

# Cleanup old checkpoints, keep last 20
ws-ckpt cleanup -w /home/user/projects/my-project --keep 20

# Enable auto-cleanup for workspace
ws-ckpt config -w /home/user/projects/my-project --enable-auto-cleanup --auto-cleanup-keep 7d
```

### diff Output Markers

| Marker | Meaning | Color |
|--------|---------|-------|
| `+` | File/directory added | Green |
| `-` | File/directory deleted | Red |
| `M` | Content modified | Yellow |
| `R` | Renamed | Cyan |

> diff ships a smart resolver that maps btrfs low-level transient inode references (such as `o261-118-0`) to real file paths and dedupes multiple operations on the same file. Rollback previews (`rollback --preview`) use the same marker semantics.

---

## Configuration

### Daemon Configuration

The daemon configuration file is located at `/etc/ws-ckpt/config.toml`. This is a system-level configuration for the ws-ckpt daemon process.

There is no user-side global config file. Auto-checkpoint and cleanup behavior are controlled per-plugin:

### OpenClaw Plugin Configuration

```json
// ~/.openclaw/ws-ckpt.json
{
  "autoCheckpoint": true,
  "workspace": "/home/user/projects/my-project"
}
```

### Hermes Plugin Configuration

```bash
hermes config set plugins.ws-ckpt.workspace /home/user/projects/my-project
```

### CLI-Based Configuration

Configuration has two layers: **global** (`/etc/ws-ckpt/config.toml`, daemon-wide defaults) and **local** (per-workspace `policy.toml` overrides). Running `ws-ckpt config` without a scope prints a read-only overview; `-g` views/edits the global config; `-w` can only override `auto_cleanup` and `auto_cleanup_keep` — the remaining fields (interval / image / health check) are daemon-wide and can only be set via `-g`; `-w <workspace> --reset` removes the workspace override and falls back to the global config.

```bash
# Enable auto-cleanup, keep checkpoints for 7 days
ws-ckpt config -w /home/user/projects/my-project --enable-auto-cleanup --auto-cleanup-keep 7d

# Global config
ws-ckpt config -g --enable-auto-cleanup --auto-cleanup-keep 20
```

The global config file is read by the daemon, so `config -g` does more than write it: after saving `/etc/ws-ckpt/config.toml` it asks the daemon to reload, then compares what the daemon actually loaded against what was written. On any mismatch the command lists each differing field and exits non-zero instead of reporting success.

This matters most in a Kubernetes sidecar deployment, where the CLI (app container) and the daemon run in separate containers with separate filesystems. Mount `/etc/ws-ckpt` on a volume shared by both containers (an `emptyDir` is enough); otherwise every `config -g` setting silently stays at the daemon's built-in default. The bundled `k8s-sidecar-example.yaml` already wires this shared volume up.

---

## Important Notes

> **WARNING**: The workspace path configured for ws-ckpt must NOT be:
> - The root path (`/`)
> - Inside the daemon's mount_path
> - An active mount point (see below)
> - The Agent startup directory or any parent directory (validated at plugin level)
>
> These constraints are enforced by the daemon. Attempts to use invalid paths will be rejected.

### The workspace root cannot be a mount point

Initializing a workspace moves the original directory aside as a backup, and
`rename(2)` fails with `EBUSY` on a directory that is itself a mount point. Any
filesystem type is affected, not just FUSE.

The common case is an in-place SkillFS mount, where the source and the mountpoint
are the same directory. Unmount it first:

```bash
skillfs stop /path/to/workspace      # in-place SkillFS mount
fusermount3 -u /path/to/workspace    # any other FUSE mount
```

This applies to `init` and to the first `checkpoint` on an unmanaged path, which
auto-initializes. Once a workspace is initialized, later `checkpoint`, `rollback`,
`list`, and `diff` operations are unaffected.

Only the workspace root itself is rejected. A mount nested *inside* the workspace
does not block `init`, but the outcome is rarely what you want: the mount stays
attached to the backup directory that `init` moves aside, while the new workspace
receives a plain copy of the mount's contents — subsequent writes land in the
copy, not on the mounted filesystem, and the two silently diverge. Unmount nested
mounts before initializing, or keep mount points outside the workspace tree.

### Rolling back an OpenClaw workspace can trigger a safety block

OpenClaw records workspace setup state outside the workspace itself. Restoring
an older snapshot can therefore make the workspace contents disagree with
recent OpenClaw state, causing OpenClaw to stop instead of reseeding files:

```
WorkspaceVanishedError: OpenClaw workspace appears to have disappeared ...
Refusing to reseed BOOTSTRAP.md over a recently attested workspace.
```

After a successful agent conversation, consider immediately creating and
recording a baseline checkpoint:

```bash
ws-ckpt checkpoint -w /path/to/workspace
```

Prefer that checkpoint, or a later checkpoint already verified with the agent,
over snapshots from before the first successful conversation. OpenClaw's check
combines workspace contents with version-specific setup state. The presence of
any one file, including BOOTSTRAP.md, is not by itself proof that a snapshot
will be accepted. After recovery, run the OpenClaw agent that uses the restored
workspace and confirm that `WorkspaceVanishedError` no longer occurs. Treat
later provider, credential, or runtime errors separately.

The recovery steps below are limited to the releases reproduced here. For other
OpenClaw versions, use the recovery guidance shipped with that release rather
than extrapolating from an adjacent version.

- OpenClaw 2026.7.1 (file-backed attestation) — remove
  this workspace's attestation files. First obtain the exact effective home and
  state directory used by the agent process from its invocation, service, or
  deployment configuration. Do not infer them from the recovery shell's
  `$HOME` or by scanning `.openclaw*` directories. For example, an agent
  started with `OPENCLAW_HOME=/srv/oc openclaw --profile team ...` normally
  uses `/srv/oc` and `/srv/oc/.openclaw-team`; an explicit
  `OPENCLAW_STATE_DIR` takes precedence.

  The command prompts for those exact absolute paths, examines only the three
  locations checked by the verified release, removes files carrying OpenClaw's
  attestation marker, and fails if it removes no valid record:

  ```bash
  IFS= read -r -p 'Workspace path used by the agent: ' WS
  IFS= read -r -p 'Effective OpenClaw home: ' OC_HOME
  IFS= read -r -p 'Effective OpenClaw state directory: ' OC_STATE_DIR
  node - "$WS" "$OC_HOME" "$OC_STATE_DIR" <<'NODE'
  const crypto = require("crypto");
  const fs = require("fs");
  const path = require("path");

  const HEADER = "openclaw-workspace-attestation:v1\n";
  const MAX_BYTES = 2048;
  const [workspaceInput, homeInput, stateDirInput] = process.argv.slice(2);
  const inputs = [workspaceInput, homeInput, stateDirInput];
  if (inputs.some((value) => !value || !path.isAbsolute(value))) {
    console.error("Workspace, effective home, and state directory must be absolute paths.");
    process.exit(1);
  }

  const workspace = path.resolve(workspaceInput);
  const home = path.resolve(homeInput);
  const stateDir = path.resolve(stateDirInput);
  const hash = crypto.createHash("sha256").update(workspace).digest("hex");
  const targets = [...new Set([
    path.join(stateDir, "workspace-attestations", `${hash}.attested`),
    path.join(home, ".clawdbot", "workspace-attestations", `${hash}.attested`),
    `${workspace}.attested`,
  ])];

  let removed = 0;
  let failed = false;
  for (const target of targets) {
    let stat;
    try {
      stat = fs.lstatSync(target);
    } catch (error) {
      if (error.code === "ENOENT") {
        console.log(`not present: ${target}`);
      } else {
        failed = true;
        console.error(`FAILED: ${target} (${error.message})`);
      }
      continue;
    }

    if (!stat.isFile() || stat.size > MAX_BYTES) {
      console.log(`skipped: ${target} (not an OpenClaw attestation file)`);
      continue;
    }

    let content;
    try {
      content = fs.readFileSync(target, "utf8");
    } catch (error) {
      failed = true;
      console.error(`FAILED: ${target} (${error.message})`);
      continue;
    }
    if (!content.startsWith(HEADER)) {
      console.log(`skipped: ${target} (not an OpenClaw attestation file)`);
      continue;
    }

    try {
      fs.unlinkSync(target);
      removed += 1;
      console.log(`removed: ${target}`);
    } catch (error) {
      failed = true;
      console.error(`FAILED: ${target} (${error.message})`);
    }
  }
  if (failed || removed === 0) {
    if (removed === 0) {
      console.error("No valid attestation record was removed; verify all three input paths.");
    }
    process.exit(1);
  }
  NODE
  ```

  Run the OpenClaw agent that uses the restored workspace. If it is still
  blocked, verify the three inputs instead of deleting additional state
  directories.

- OpenClaw 2026.8.1 (SQLite-backed attestation) — do not edit the SQLite
  database or depend on its private schema. Roll back to a checkpoint created
  after a successful agent conversation, then retry the agent:

  ```bash
  ws-ckpt rollback -w /path/to/workspace -s <known-good-snapshot-id>
  ```

  If no known-good checkpoint exists, there is currently no non-destructive
  command that immediately clears only this workspace's block. The error also
  mentions `openclaw reset --scope full`, but that removes every agent
  workspace and the complete OpenClaw state directory, including credentials,
  sessions, and installed plugins, so it is not recommended for this recovery.

---

## Natural Language Usage (Agent-Driven)

When the ws-ckpt skill is installed, Agents can use checkpoints via natural language:

| Intent | Example Phrases |
|--------|-----------------|
| Create checkpoint | "Save the workspace", "Take a snapshot before I start" |
| Rollback | "Undo all changes", "Go back to the last good state" |
| List checkpoints | "Show all saved states", "List my checkpoints" |
| Diff | "What changed since the last save?" |

---

## FAQ

**Q: What happens if my filesystem is not btrfs?**
A: ws-ckpt creates a btrfs loop image on the host filesystem and loop-mounts it, providing full COW snapshot functionality regardless of the underlying filesystem type.

**Q: Can I use ws-ckpt with multiple workspaces?**
A: Yes. Use `-w` flag with each command to specify the workspace, or configure multiple workspaces via plugins.

**Q: How much disk space do checkpoints use?**
A: With btrfs COW, only changed blocks are stored. Typical overhead is <5% of workspace size per checkpoint.
