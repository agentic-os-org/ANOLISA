# Skill Ledger User Guide

This page describes the Python V1 deployment and Agent integrations. For the Rust V2 core,
system signing key and root daemon, use the [SkillSec V2 guide](skillsec-v2.md).

Skill Ledger is the security subsystem of agent-sec-core that maintains a version chain of file hashes, scan results, and cryptographic signatures for AI Agent Skills, helping detect tampered Skills or injected malicious content. The default quick scan runs automatically via the built-in static scanner; an optional deep scan is driven by the Agent following the `skill-vetter` protocol.

---

## Part 1: Quick Tour

### Core Concepts

| Concept | Description |
|---------|-------------|
| **Manifest** | JSON record (`.skill-meta/latest.json`) containing file hashes, scan results, and digital signatures; created and updated by `scan`, `certify`, or the `init` baseline |
| **Version chain** | Append-only ledger — each non-root version names an authenticated parent through `previousVersionId` and `previousManifestSignature`; recovery can start a new signed chain segment |
| **Status** | Per-Skill security state: `pass` ✅ · `none` 🆕 · `drifted` 🔄 · `warn` ⚠️ · `deny` 🚨 · `tampered` 🔴 |

### 1. Initialize Signing Keys

```bash
# Initialize keys and build a quick-scan baseline for Skills in covered directories
agent-sec-cli skill-ledger init
```

The baseline is a batch write operation. Host-backed, read-only packaged system
Skills follow the skip contract described under
[Packaged read-only system Skills](#packaged-read-only-system-skills); key
initialization still completes.

`init --no-baseline` initializes keys without scanning Skills. Repeated initialization
reuses the key pair and returns `keyCreated: false` and `key: null`; newly created
key details are returned in `key` with `keyCreated: true`. `init --force-keys`
replaces the pair and archives the previous public key for signature verification.

Key locations:

| File | Path | Permissions |
|------|------|-------------|
| Private key file | `~/.local/share/agent-sec/skill-ledger/key.enc` | 0600; unencrypted by default, encrypted with `--passphrase` |
| Public key | `~/.local/share/agent-sec/skill-ledger/key.pub` | 0644 |

To protect the private key with a passphrase:

```bash
# Interactive passphrase prompt
agent-sec-cli skill-ledger init --passphrase

# Or via environment variable (suitable for CI)
SKILL_LEDGER_PASSPHRASE="your-secret" agent-sec-cli skill-ledger init --passphrase
```

### 2. Check Skill Integrity

```bash
agent-sec-cli skill-ledger check /path/to/your-skill
```

Outputs JSON; the key field is `status`:

| Status | Meaning |
|--------|---------|
| `none` 🆕 | Neither `latest.json` nor any version JSON/snapshot artifact exists, or an authenticated matching manifest has `scanStatus=none` |
| `pass` ✅ | Manifest authenticity valid + files unchanged + scan passed |
| `drifted` 🔄 | Manifest authenticity valid, but the live Skill differs from the signed file hashes; this is an unscanned divergence, not a scanner-confirmed risk verdict |
| `warn` ⚠️ | Manifest authenticity valid, but the last scan has low-risk findings |
| `deny` 🚨 | Manifest authenticity valid, but the last scan has high-risk findings |
| `tampered` 🔴 | Ledger metadata failed schema, hash, signature, signed-identity, or latest/version-artifact consistency validation, including a missing `latest.json` while version artifacts remain, a missing signature, or signed latest replay |

Skill Ledger authenticates an existing manifest and binds `latest.json` to the newest verified version artifact before comparing its file hashes with the live Skill. A missing `latest.json` is `none` only when no version JSON or snapshot artifact remains; otherwise the incomplete ledger is `tampered`. A missing or invalid signature, or replay of an older signed latest pointer, is also `tampered`, even when the live files have changed; only a verified current manifest can produce `drifted`.

### 3. Quick Scan + Signed Certification

For a machine-readable, read-only assessment before certification, run:

```bash
agent-sec-cli skill-ledger analyze /path/to/your-skill --format json
```

`analyze` runs both `code-scanner` and `static-scanner` against the current
directory. It does not create keys, `.skill-meta`, manifests, snapshots,
signatures, configuration entries, or security events. It is suitable as an
incremental signal for submission services, but does not replace their existing
content rules or approval policy.

The process contract is:

| Exit code | Meaning |
|-----------|---------|
| `0` | Coverage is complete; inspect `status` for `pass`, `warn`, or `deny` |
| `1` | A scanner or file could not be covered; `status=error` and `coverage_complete=false` |
| `2` | Invalid input or protocol usage, including a missing `SKILL.md` |

Protocol errors include a missing Skill root argument (`skill-root-required`)
and unsupported output formats (`unsupported-format`); both return exit code
`2` with a JSON error payload.

Callers must check the exit code, top-level `status`, and
`coverage_complete`. Findings are sorted by `file`, `line`, and `rule`;
scanner results are always ordered as `code-scanner`, then `static-scanner`.
The bundled JSON Schema is
`agent_sec_cli/skill_ledger/analyze.schema.json`.
Analysis accepts at most 2,000 regular files, 50 MiB of aggregate file content,
and 32 directory levels. Exceeding a limit returns incomplete coverage.

Node.js subprocess example:

```javascript
import { spawn } from "node:child_process";

const child = spawn(
  "agent-sec-cli",
  ["skill-ledger", "analyze", skillDir, "--format", "json"],
  { stdio: ["ignore", "pipe", "pipe"] },
);

let stdout = "";
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  stdout += chunk;
});
child.on("close", (code) => {
  const result = JSON.parse(stdout);
  if (code !== 0 || result.status === "error" || !result.coverage_complete) {
    throw new Error("Skill analysis did not complete");
  }
  // Keep existing submission rules; consume result.scanners as extra evidence.
});
```

`analyze` currently ships in the complete `agent-sec-cli` wheel and RPM. A
future packaging change may extract the shared scanners into a scanner-only
wheel or RPM subpackage; the scanner rules must remain single-source.

#### Packaged read-only system Skills

`scan` is a stateful certification command: successful work persists scanner
results, a signed manifest, and a snapshot under `.skill-meta`. It is not a
read-only alias for `analyze`.

For host-backed packaged Skills that are immediate children of
`/usr/share/anolisa/skills/` or `/usr/local/share/anolisa/skills/`, batch write
paths skip an item when its ledger state is not writable. Both `scan --all` and
the default `init` baseline emit this per-Skill result:

```json
{
  "canonicalSkillDir": "/usr/share/anolisa/skills/example",
  "skillName": "example",
  "status": "skipped",
  "reasonCode": "readonly_system_skill",
  "persisted": false
}
```

`skipped` is an operational batch status, not one of the six integrity states.
It does not mean `pass`, does not attest the Skill, and does not fail the batch
by itself; the command exits `0` unless another item returns `status=error`.
No per-Skill scanner result, manifest, snapshot, or `.skill-meta` state is
written for the skipped item, while global key initialization keeps its normal
behavior. An explicit `scan <dir>` remains strict: it exits `1` and points the
caller to `analyze` for read-only findings.

The same batch skip also applies to host-backed Skills directly under
`$XDG_DATA_HOME/anolisa/skills/` (default `~/.local/share/anolisa/skills/`), with
`reasonCode=readonly_default_skill`, when ledger state is not writable and the
Skill is not covered by `managedSkillDirs`. This includes image-provided raw
user Skills made read-only to the runtime user. Writable raw user Skills are
scanned normally; skipped Skills are not added to `managedSkillDirs`.
Explicit scans and writes to managed user Skills still fail on permission
errors. SkillFS backing and resolver errors remain errors.

`check` and `status` are unchanged. A skipped Skill with no prior ledger
artifacts returns `none`, and aggregate health can remain `unscanned`. These
values do not turn the batch skip into an attestation or a safety verdict.

The default certification path uses the built-in quick scanner and does not depend on an LLM. For a single Skill:

```bash
agent-sec-cli skill-ledger scan /path/to/your-skill
```

After scanning, re-check the status:

```bash
agent-sec-cli skill-ledger check /path/to/your-skill
```

For a more thorough semantic review, trigger a deep scan through the Agent. The Agent reads the built-in `skill-vetter-protocol.md` scanning protocol and reviews the target Skill file by file across four phases (origin verification → code review → permission boundary assessment → risk grading), writing results to a findings JSON file. Then pass the findings file to `certify` to complete signed certification:

```bash
agent-sec-cli skill-ledger certify /path/to/your-skill \
  --findings /tmp/skill-vetter-findings-your-skill.json \
  --scanner skill-vetter \
  --delete-findings
```

`scan` runs the built-in quick scanner and signs the result into the ledger; `certify` only imports external findings. `certify` performs, in order:

1. Authenticates any existing manifest, then verifies file consistency (automatically creates a new version if files changed or the manifest is invalid)
2. Normalizes findings and merges them into the manifest's `scans[]` array
3. Aggregates `scanStatus` (`pass` / `warn` / `deny`)
4. Re-signs and writes `.skill-meta/latest.json`

An invalid or unsigned existing manifest is never signed in place and none of its scan results or decisions are inherited. Recovery creates a new version linked only to the newest historical version whose identity, hash, signature, and snapshot all verify. If no such parent exists, the new signed version starts a new chain segment with both previous-version fields set to `null`.

Example output:

```json
{
  "versionId": "v000002",
  "scanStatus": "pass",
  "newVersion": true,
  "skillName": "your-skill"
}
```

### 4. View Overall Security Posture

```bash
# Overall skill-ledger system status (keys, config, health of all Skills)
agent-sec-cli skill-ledger status

# Include per-Skill detailed status
agent-sec-cli skill-ledger status --verbose
```

`status` outputs JSON with three sections:

| Section | Description |
|---------|-------------|
| `keys` | Signing key state (initialized, fingerprint, encrypted, number of archived keys) |
| `config` | Configuration summary (default directories, managedSkillDirs pattern count, registered scanners) |
| `skills` | Aggregate health (discovered Skill count, per-status counts, overall health label) |

`health` label meanings: `healthy` (no critical/attention statuses and not all none; may mix pass/none), `unscanned` (all none), `attention` (drifted/warn present), `critical` (deny/tampered/error present), `empty` (no registered Skills).

`none` means that no attested scanner verdict is available; it covers both a
missing ledger artifact and a verified manifest whose `scanStatus` is `none`.
`unscanned` means that every discovered Skill currently checks as `none`.
Neither value may be interpreted as `pass`. A read-only packaged system Skill
skipped by a batch write remains in those existing states when it has no prior
ledger artifacts.

With `--verbose`, an additional `results` array contains detailed check results for each Skill.

### 5. Audit the Full Version Chain

Deep-verify all historical versions — schema, hash, signature, signed identity, and explicit parent links. A parent link must point to an earlier authenticated version and carry its exact signature; a signed version with both previous-version fields set to `null` is a valid chain-segment root. Invalid historical versions still make the overall audit fail.

```bash
agent-sec-cli skill-ledger audit /path/to/your-skill

# Also verify snapshot file hashes
agent-sec-cli skill-ledger audit /path/to/your-skill --verify-snapshots
```

### 6. Agent-Driven Scanning (Recommended)

The most natural way to use Skill Ledger is through natural-language requests to an AI Agent. A default "scan" performs the quick scan; the `skill-vetter` deep scan runs only when the user explicitly requests it, or confirms continuation after a quick scan:

| Request | Effect |
|---------|--------|
| "Scan /path/to/skill" | Quick-scan certification for the specified Skill |
| "Scan all skills" | Batch quick scan of all Skills configured in `config.json` |
| "Deep scan /path/to/skill" | File-by-file deep review per the `skill-vetter` protocol, then certification |
| "Check skill status" | Output the status triage table only, without scanning |

Skill workflow:

- **Phase 1** (environment preparation and status view): validates CLI and keys, resolves target Skills, outputs a triage table
- **Phase 2** (quick-scan certification): invokes the built-in `code-scanner` and `static-scanner`, then signs into the manifest
- **Phase 3** (optional deep scan): `skill-vetter` four-phase review — origin verification → code review → permission boundary assessment → risk grading — then writes to the version chain via `certify --findings`

---

## Part 2: Protecting Skills via SkillFS Activation, User Decisions, and Host Hook Policies

### Architecture Overview

Skill Ledger is recommended in combination with SkillFS: SkillFS captures Skill changes and notifies the Skill Ledger daemon to scan and refresh `.skill-meta/activation.json`/xattr. Host hooks/capabilities remain available as compatibility paths. Hosts with native approval or notice support default to `policy = "ask"`; Hermes defaults to `policy = "observe"` because its Python plugin API exposes neither mechanism.

```
┌──────────────────────────────────────────────────┐
│                  Agent runtime                    │
│                                                   │
│  ┌──────────────┐      ┌──────────────────────┐   │
│  │  SkillFS     │      │  skill-ledger        │   │
│  │  change      │      │  SKILL.md            │   │
│  │  capture     │      │  (on-demand deep     │   │
│  │      │        │     │   scan)              │   │
│  │      ▼        │      └──────────┬───────────┘   │
│  │ daemon notify │                 │               │
│  │      │        │                 │               │
│  │      ▼        │                 │               │
│  │ activation    │                 │               │
│  │ refresh       │                 │               │
│  └──────┤────────┘                 │               │
│         ▼                         ▼               │
│  ┌──────────────────────────────────────────┐     │
│  │       agent-sec-cli skill-ledger          │     │
│  │   show / export / decide / scan / certify │     │
│  └──────────────────────────────────────────┘     │
│                      │                            │
│                      ▼                            │
│           .skill-meta/latest.json                 │
│           .skill-meta/activation.json + xattr     │
└───────────────────────────────────────────────────┘
```

- **Recommended path — SkillFS + daemon activation**: SkillFS discovers Skill file changes; the daemon refreshes the executable activation target based on the latest signed manifest, user decisions, and the activation policy. The Agent runtime reads activation metadata instead of relying on host hook pre-checks by default.
- **Compatibility path — host hook/capability policy**: OpenClaw, Hermes, copilot-shell, and Qwen Code call `agent-sec-cli skill-ledger show` before a Skill loads; Codex and Qoder CLI run a read-only `agent-sec-cli skill-ledger check` at their respective local Skill trigger boundaries. Hermes supports `observe` / `block` and defaults to `observe`; the other hosts retain their native `observe` / `warn` / `ask` / `block` behavior and defaults.
- **Agent-driven scanning**: `scan` runs the built-in quick scan and signs the result; the `skill-ledger` Skill drives the full four-phase security review when the user requests a deep scan, importing results via `certify --findings`. **Triggered on demand**, initiated by user request.

### Recommended Path: SkillFS + Daemon Activation

**How it works:**

With SkillFS enabled, the runtime entry point of Skill Ledger is handled by the daemon:

1. SkillFS captures Skill directory creation, updates, deletion, or content changes.
2. SkillFS notifies the Skill Ledger daemon's `skill_ledger.skillfs_notify_change` interface.
3. The daemon refreshes `.skill-meta/activation.json` based on the signed manifest, current file state, user decisions, and the activation policy, and writes xattr on a best-effort basis.
4. If the current version cannot be activated directly, the activation metadata points to the previous trusted `pass` / `warn` snapshot; if no trusted fallback exists, it points to a safe pending-review stub; `target: null` is written only for user `block` decisions or fail-safe scenarios.

**Version requirement: SkillFS must be 0.4.0 or newer.**

Since 0.4.0, the `skill_ledger.skillfs_notify_change` call in step 2 uses
**notify v2**, whose business payload carries exactly four fields:
`canonicalSkillDir`, `skillId`, `eventKind`, and `paths`. This is a **breaking
upgrade with no fallback path** — the daemon rejects any request whose
`schemaVersion` is not `2`, and SkillFS performs no version negotiation. The two
components must therefore be upgraded together:

- SkillFS older than 0.4.0 sends notify v1 only, which the current daemon rejects
  request by request;
- a failed notify delivery is only a warning on the SkillFS side and never stops
  the FUSE service.

A version mismatch therefore fails **silently**: newly installed Skills stay
hidden and neither side reports an obvious error. When diagnosing, first confirm
that `skillfs --version` is 0.4.0 or newer.

**Two sockets, opposite directions.** Most joint-deployment wiring mistakes come
from conflating them:

| Socket | Listener | Default path | Purpose |
|---|---|---|---|
| daemon socket | agent-sec-core daemon | `$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock` (override with `AGENT_SEC_DAEMON_SOCKET`) | SkillFS points `--notify-socket` here to send change notifications |
| control socket | SkillFS | `/run/user/<uid>/skillfs/control.sock` (override on the Ledger side with `AGENT_SEC_SKILLFS_CONTROL_SOCKET`) | The daemon queries `skill.resolveLiveSource` back through it and writes activation metadata |

The resolver chooses the control endpoint in this order: an explicit
`socket_path` supplied by an embedding caller, `AGENT_SEC_SKILLFS_CONTROL_SOCKET`,
then the per-effective-UID default above. The selected path must match the
SkillFS control socket. A complete ACS deployment should run SkillFS and the
daemon with the same numeric UID so the default endpoint and key ownership
checks agree across containers.

#### HMAC-Authenticated Joint Deployment

The HMAC integration is optional for legacy deployments but should be enabled
in both directions for a complete ACS deployment:

| Variable | Reader | Purpose |
|---|---|---|
| `AGENT_SEC_SKILLFS_CONTROL_SOCKET` | Ledger resolver | Select the SkillFS control socket |
| `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` | Ledger resolver | Authenticate control requests and responses |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | agent-sec-core daemon | Authenticate incoming SkillFS notifications |

For example, provide these variables to the agent-sec-core daemon and any CLI
process that resolves SkillFS paths, and configure SkillFS with the matching
socket and key material:

```bash
export AGENT_SEC_SKILLFS_CONTROL_SOCKET="/run/user/$(id -u)/skillfs/control.sock"
export AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE="/run/agent-sec-keys/skillfs-control.key"
export AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE="/run/agent-sec-keys/skillfs-notify.key"

skillfs mount /path/to/skills /mnt/skillfs \
  --security --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --notify-auth-key-file "$AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE" \
  --control-socket "$AGENT_SEC_SKILLFS_CONTROL_SOCKET" \
  --trusted-peer-key-file "$AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE"
```

The control and notify variables may point to the same key file, but separate
direction-specific keys are recommended. The control key is loaded on the first
resolve and cached for that resolver client's lifetime; it is not hot-reloaded.
The daemon loads the notify key before binding its socket, so an invalid notify
key prevents daemon startup.

Each key file must satisfy all of these checks:

- the configured path is absolute;
- the target is a regular file, not a symlink, directory, FIFO, or device;
- the file owner is the reader's effective UID;
- no group or other permission bits are set (mode `0600` is recommended);
- the unmodified file content is between 32 and 4096 bytes.

Control resolution deliberately distinguishes legacy availability from an
authenticated deployment requirement:

| Control key | Socket `ENOENT` | Other connection, authentication, or protocol failures |
|---|---|---|
| Not configured | Fall back to the canonical host path | Fail with `skill_root_resolve_failed` |
| Configured | Fail with `skill_root_resolve_failed` | Fail with `skill_root_resolve_failed` |

An authenticated `managed=false` response remains a trusted uncovered result
and uses the canonical host path. After a control key is configured, Ledger
never retries the request as plaintext.

Notify handling follows the same downgrade boundary:

| Notify key | Behavior |
|---|---|
| Not configured | Legacy plaintext notify remains available; an authentication handshake is rejected |
| Configured | `skill_ledger.skillfs_notify_change` must use HMAC; plaintext notify is rejected, while other daemon methods keep their existing plaintext protocol |

For containerized ACS deployments, share the two Unix socket directories and
the required key material between the corresponding containers. Use the same
numeric UID and ensure each mounted key is owned by that UID. Kubernetes Secret
volumes use symlink-based projections, which the key loader intentionally
rejects. Copy each Secret into a private shared volume such as `emptyDir`, set
the owner and mode there, and point the environment variables at the resulting
regular, non-symlink files. Do not point them directly at the projected Secret
paths.

Under the Hermes layout the activation flow carries nested identities
(`category/skill`) rather than flat skill names, and `skillId` keeps both
components.

For the full protocol definition, canonical path semantics, and deployment
boundaries, see
[Skill Ledger's SkillFS integration design](../../../../../src/agent-sec-core/docs/design/SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md)
(Chinese only) and the [SkillFS user guide](../../runtime/skillfs.md).

### Unified Host Hook Controls

Host adapters use `SKILL_LEDGER_HOOK_ENABLED` as the kill switch and
`SKILL_LEDGER_MODE` as the behavior selector. The switch defaults to `true`; the policy
defaults to `ask` on hosts with native approval/notice support. Hermes defaults to `observe` and
accepts only `observe` or `block`; its legacy `warn` / `ask` values fall back to `observe` with a
host diagnostic. `observe` runs the check and audit path without a user-visible message. Legacy
`debug` maps to `observe`, while legacy `deny` maps to `block`. An environment policy overrides
Hermes or OpenClaw capability configuration.

The host Agent reads these variables when it loads the plugin. Restart the Agent process that
hosts the hook after changing them; the hook and agent-sec-core are not separate policy services.

On hosts other than Hermes, a hook that cannot request approval may fall back from `ask` to
`warn`. Hermes never simulates either action by rewriting the assistant's final response; its
unsupported values fall back to `observe`. When another host cannot enforce a block at its
current boundary, it must not claim enforcement.
Set `SKILL_LEDGER_HOOK_ENABLED=false` to skip input processing, key initialization, and CLI calls.

### Compatibility Path: Hook / Capability Policy

When the Agent loads a Skill, the OpenClaw, Hermes, copilot-shell, and Qwen Code hooks resolve the Skill directory, run `agent-sec-cli skill-ledger show <skill_dir>`, and let the unified `policy` control the host-specific behavior. These hooks consume only the `message` in the summary:

| Policy | Behavior |
|--------|----------|
| `observe` | `message != null` only writes audit/debug diagnostics and passes. |
| `warn` | `message == null` passes silently; `message != null` shows a warning and passes. |
| `ask` | Default. `message == null` passes silently; `message != null` requests user confirmation or uses the host approval UI. |
| `block` | `message != null` blocks directly, using the message as the reason or alert text. |

Hermes implements only the `observe` and `block` rows. Its `warn` / `ask` compatibility values
become `observe` with a diagnostic and never enter the assistant response through
`transform_llm_output`.

The trigger rules for `message` are decided uniformly by Skill Ledger: no prompt when the user already has an `allow` / `always_allow` / `rollback` / `block` decision; no prompt when latest is `pass` or `warn` and directly exposable; a prompt when there is no user decision and latest is `deny` / `none` / `drifted` / `tampered`, explaining whether the current active version is a fallback or a safe pending-review stub. `latestStatus=unmanaged` means the daemon cannot manage this root and cannot write `.skill-meta` or record user decisions, so it is returned as diagnostics only with `message=null`, and every hook policy including `block` passes silently.

Codex and Qoder CLI are low-level integrity gates that run `skill-ledger check <skill_dir>` after canonical-path and root-boundary validation. Codex resolves `$skill-name` references at `UserPromptSubmit`; because that boundary cannot request approval, `ask` falls back to `warn`. Qoder CLI registers a dedicated `PreToolUse` hook for the `Skill` tool, builds user → project directory tables from the absolute `cwd` in the event, and parses the `SKILL.md` frontmatter `name` (falling back to the directory name when frontmatter is absent). When Qoder frontmatter exists but `name` is missing, ambiguous, or uses a YAML scalar the hook cannot safely parse, the call is not downgraded to a non-local Skill — it is handled per the current policy. `pass` passes silently; `none` / `drifted` / `warn` / `deny` / `tampered` and `error` are audited silently, warned and passed, sent for confirmation where supported, or blocked per the `observe` / `warn` / `ask` / `block` policy. Qoder CLI unavailability, execution failure, timeout, or unparseable output is also handled by this four-level policy rather than a fixed fail-open; legacy `debug` is only an alias for `observe`.

All six adapters enable Skill Ledger by default. Hermes uses policy `observe`; the other adapters retain policy `ask`. copilot-shell, Codex, Qoder CLI, and Qwen Code register their corresponding hook boundaries in their default manifests. OpenClaw and Hermes can also take capability configuration, while `SKILL_LEDGER_MODE` remains the deployment-level override. Apart from the explicitly documented Qoder CLI low-level gate above, the other compatibility hooks remain fail-open when the CLI infrastructure misbehaves, avoiding blocked Skill loads.

The built-in Ledger discovery entries also include the raw user root.
For this root, an unset, empty, relative, or dot-segment `XDG_DATA_HOME` uses
`~/.local/share`, matching ANOLISA and cosh. `enableDefaultSkillDirs=false`
also disables this built-in entry. The hook keeps the existing policy and
unmanaged-skill behavior; recognizing a directory does not certify its contents.

`agent-sec-cli capabilities --agent cosh --capability skill-ledger --output json`
reports the effective XDG data-root setting without resolving the user's home
or reading Agent configuration. Its fallback is displayed as `~/.local/share`.

The copilot-shell hook currently covers three directory classes — project / user / system: `<cwd>/.copilot-shell/skills/`, `~/.copilot-shell/skills/`, the raw user root `$XDG_DATA_HOME/anolisa/skills/` (default `~/.local/share/anolisa/skills/`), and the RPM and raw-install system roots `/usr/share/anolisa/skills/` and `/usr/local/share/anolisa/skills/`. Skills from custom, extension, remote, or other paths make the hook fail open and skip the skill-ledger check; the OpenClaw plugin extracts the Skill directory from the `SKILL.md` path it reads.

For batch certification or post-install certification, complete directory resolution and certification before letting the Agent read uncertified Skill content: avoid proactively reading an uncertified Skill's `SKILL.md` or auxiliary files before batch certification; after a successful install, locate the final local directory, confirm it contains `SKILL.md`, then run quick-scan certification.

**Enabling in OpenClaw**:

```json
{
  "capabilities": {
    "skill-ledger": {
      "enabled": true,
      "policy": "ask"
    }
  }
}
```

**Enabling in Hermes**:

```toml
[capabilities.skill-ledger]
enabled = true
timeout = 5
policy = "observe"
```

Hermes supports `observe` and native `block` only. Existing `warn` / `ask` values map to
`observe` with a host diagnostic; `enable_block=false` also maps to `observe` for legacy config.
In Hermes `observe` mode, a non-empty exposure summary is logged at `INFO`, while `deny` and
`tampered` statuses or a `reasonCode=tampered` activation result are elevated to `WARNING`.
These log levels do not block the Skill or write content into the assistant response.

**Configuring copilot-shell**: the default Cosh manifest already registers the `skill-ledger` hook. The default policy is `ask`; for observe-only, warning-only, or hard denial, set `SKILL_LEDGER_MODE=observe` / `warn` / `block`. The `debug` value remains an alias for `observe`. This environment variable should be set by a trusted host or deployment environment — not by Skills, project scripts, or untrusted shell startup logic; to prevent policy downgrades via a tampered local shell profile, it should eventually move to a trusted host configuration source.

**Configuring Qoder CLI**: after installing `qoder-plugin`, the plugin automatically registers a `PreToolUse` hook with matcher `Skill`. The default policy is `ask`; a trusted launch environment may set `SKILL_LEDGER_MODE=observe` / `warn` / `block` and adjust the CLI timeout via `SKILL_LEDGER_TIMEOUT` (default 5 seconds). The `debug` value remains an alias for `observe`. The hook covers local Skills under `~/.qoder/skills/` and `<cwd>/.qoder/skills/`, with user-level Skills of the same name taking precedence; only when both directory tables resolve trustworthily with no match is the call treated as a built-in, plugin, or remote Skill — passed and logged at debug. The hook never runs `init` or `scan` automatically. A Skill with neither `latest.json` nor any version JSON/snapshot artifact enters the policy as `none`. A missing `latest.json` while history remains, or an existing latest manifest with a missing or invalid signature, enters as `tampered`. After review, run `agent-sec-cli skill-ledger scan <skill_dir>` explicitly.

The global Skill Ledger `activationPolicy` belongs to SkillFS/daemon activation; the hook `policy` here only controls the user-visible behavior and log level of host hooks/capabilities.

### Reviewing and Deciding on Non-Pass Skills

When a hook or `show` indicates the current skill needs user review, start with the unified exposure summary:

```bash
agent-sec-cli skill-ledger show /path/to/skill
```

Key fields:

| Field | Meaning |
|-------|---------|
| `latestStatus` | Status of the latest skill root or the latest signed version |
| `activeVersionId` | Version currently exposed to SkillFS; `null` means no real active version |
| `target` | Target SkillFS currently reads; pending state points to `.skill-meta/versions/__pending_decision__.snapshot` |
| `userDecision` | Currently matched user decision; `null` means no decision yet |
| `message` | Information to surface to the user; hooks stay silent when `null` |

To fully review a version that is not exposed, export the latest snapshot, manifest, and findings:

```bash
agent-sec-cli skill-ledger export /path/to/skill --version latest --output /tmp/skill-review
```

After review, choose via the unified `decide` command:

```bash
# Allow the current specific version; not inherited by future versions
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason "reviewed manually"

# Allow current and future versions until the user changes or clears the decision
agent-sec-cli skill-ledger decide /path/to/skill --action always_allow --reason "trusted source"

# Fully hide the current skill; the block is not inherited by future new versions
agent-sec-cli skill-ledger decide /path/to/skill --action block --reason "unsafe behavior"

# Roll back to a specific version; without --version, defaults to the current real active version
agent-sec-cli skill-ledger decide /path/to/skill --action rollback --version v000001 --reason "use previous trusted version"

# Clear the user decision on the latest manifest, restoring global activation behavior
agent-sec-cli skill-ledger decide /path/to/skill --clear
```

Note: a hook's `ask` confirmation only lets the current host operation continue — it is not equivalent to a Skill Ledger `allow`. Only `decide` changes the subsequent activation target.

### Agent-Driven Deep Scan

#### Configuring Skill Directories (for batch scans)

Six built-in directories are included by default: `~/.openclaw/skills/*`, `~/.copilot-shell/skills/*`, `~/.hermes/skills/**`, `~/.qoder/skills/*`, `/usr/share/anolisa/skills/*`, `/usr/local/share/anolisa/skills/*`. Project-level Qoder directories are not relative defaults; after an explicit `scan` or `certify` on a project Skill, its absolute directory is written to `managedSkillDirs` via the auto-memoization mechanism. To add other directories, create or edit `~/.config/agent-sec/skill-ledger/config.json`:

```json
{
  "enableDefaultSkillDirs": true,
  "managedSkillDirs": [
    "/opt/custom-skills/*",
    "/opt/custom-skills/my-skill"
  ]
}
```

Default directories are enabled by default; `managedSkillDirs` holds directories dynamically managed by skill-ledger or added by the user, appended after the defaults (deduplicated automatically). Set `enableDefaultSkillDirs` to `false` for isolated runs.

- `"path/*"` — glob pattern: each subdirectory containing `SKILL.md` counts as one Skill
- `"path/to/skill"` — a single Skill directory (must also contain `SKILL.md`)

Non-existent directories are silently ignored. Additionally, running `scan` or `certify` on a Skill auto-appends unregistered directories to the config for later `--all` batch operations. `check` is a read-only status query and never writes config.

For direct children of the raw user root `$XDG_DATA_HOME/anolisa/skills/` (default
`~/.local/share/anolisa/skills/`), auto-remember records only the individual Skill
path, even when sibling Skills exist. Scanning a writable Skill therefore does
not add its read-only siblings to `managedSkillDirs`. Other roots retain the
existing parent-glob heuristic. Existing configuration entries are not rewritten:
an existing glob still makes covered user Skills managed and keeps write failures
strict. If you confirm that a raw-root glob was unintentionally added, replace it
with the individual paths you intend to manage, preserving any intentional coverage.

#### Scheduled Default Quick Scans

To periodically refresh default quick-scan results, put `scan --all` into cron. `scan --all` automatically skips Skills whose files are unchanged and already have complete scan results, re-scanning only new, changed, scan-result-missing, or manifest-anomalous Skills. It also returns `status=skipped` with `reasonCode=readonly_system_skill` for host-backed, read-only packaged system Skills; inspect the JSON results because this run does not create or refresh an attestation for those items.

Without a key passphrase:

```bash
mkdir -p "$HOME/.local/state/agent-sec"
AGENT_SEC_CLI="$(command -v agent-sec-cli)"
CRON_LINE="0 3 * * * $AGENT_SEC_CLI skill-ledger scan --all >> $HOME/.local/state/agent-sec/skill-ledger-scan.log 2>&1"
(crontab -l 2>/dev/null | grep -Fv "skill-ledger scan --all"; echo "$CRON_LINE") | crontab -
```

With a passphrase-protected private key, the scheduled job needs `SKILL_LEDGER_PASSPHRASE`. The command below writes the passphrase in plaintext to the current user's crontab and the system cron spool — use it only in trusted single-user environments; safer alternatives are the default passphrase-less key, or wrapping `scan --all` with a local secret manager / permission-restricted file.

```bash
read -rsp "SKILL_LEDGER_PASSPHRASE: " SKILL_LEDGER_PASSPHRASE; echo
mkdir -p "$HOME/.local/state/agent-sec"
AGENT_SEC_CLI="$(command -v agent-sec-cli)"
CRON_LINE="0 3 * * * SKILL_LEDGER_PASSPHRASE='$SKILL_LEDGER_PASSPHRASE' $AGENT_SEC_CLI skill-ledger scan --all >> $HOME/.local/state/agent-sec/skill-ledger-scan.log 2>&1"
(crontab -l 2>/dev/null | grep -Fv "skill-ledger scan --all"; echo "$CRON_LINE") | crontab -
unset SKILL_LEDGER_PASSPHRASE
```

Inspect installed scheduled jobs:

```bash
crontab -l
```

#### Triggering Scans

Just instruct the Agent in natural language. The default scan runs Phase 1 → Phase 2; Phase 1 → Phase 3 runs when the user explicitly requests a deep scan.

**Deep-scan rule table (skill-vetter):**

| Level | Rule ID | Detection Target |
|-------|---------|------------------|
| deny | `dangerous-exec` | Dangerous process execution (`child_process`, `subprocess`) |
| deny | `dynamic-code-eval` | Dynamic code execution (`eval()`, `new Function()`) |
| deny | `env-harvesting` | Bulk environment variable harvesting + network exfiltration |
| deny | `crypto-mining` | Mining signatures (`stratum`, `xmrig`, etc.) |
| deny | `credential-access` | Credential and sensitive file access (`~/.ssh/`, `.env`) |
| deny | `system-modification` | System file tampering (`/etc/`, crontab) |
| deny | `prompt-override` | Prompt-override instructions |
| deny | `hidden-instruction` | Hidden instructions (zero-width characters, HTML comments) |
| warn | `obfuscated-code` | Code obfuscation (very long lines, base64 + decode) |
| warn | `suspicious-network` | Suspicious network connections (direct IPs, non-standard ports) |
| warn | `exfiltration-pattern` | Data exfiltration patterns (file read + network send combos) |
| warn | `agent-data-access` | Agent identity data access (`MEMORY.md`, etc.) |
| warn | `unauthorized-install` | Undeclared package installation |
| warn | `unrestricted-tool-use` | Unconstrained tool-use instructions |
| warn | `external-fetch-exec` | External fetch-and-execute (`curl | bash`) |
| warn | `privilege-escalation` | Privilege escalation (`sudo`, `chmod 777`) |

### Real-World Scenarios

#### Scenario A: Detecting tampering when loading a third-party Skill

```
# SkillFS/daemon or host hook detects an anomalous status
[skill-ledger] 🚨 Skill 'third-party-tool' metadata signature verification failed
```

The alert indicates someone may have modified the manifest, flipping `scanStatus` from `deny` to `pass` to bypass security checks.

#### Scenario B: Detecting drift after a Skill update

```bash
agent-sec-cli skill-ledger check /path/to/my-skill
# → {"status": "drifted", "added": [...], "modified": [...]}
```

The status becomes `drifted` after updating the Skill. This only reports that the live root differs from the signed version; it is not a scanner-confirmed risk result. Trigger a re-scan to certify the new content and obtain its current scan status:

```
Scan /path/to/my-skill
```

#### Scenario C: Auditing historical integrity

```bash
agent-sec-cli skill-ledger audit /path/to/my-skill --verify-snapshots
```

Per-version verification: schema → hash integrity → signature validity → signed identity → explicit parent links → snapshot consistency.

---

## Command Cheat Sheet

| Command | Purpose |
|---------|---------|
| `agent-sec-cli skill-ledger init` | Initialize keys and build a quick-scan baseline for covered Skills |
| `agent-sec-cli skill-ledger init --no-baseline` | Initialize keys only, without scanning Skills |
| `agent-sec-cli skill-ledger analyze <dir> --format json` | Analyze current content without creating ledger state or an attestation |
| `agent-sec-cli skill-ledger check <dir>` | Check integrity status (JSON output) |
| `agent-sec-cli skill-ledger show <dir>` | Show latest, active, user decision, activation target, findings, and alerts |
| `agent-sec-cli skill-ledger export <dir> --version latest --output <path>` | Export a snapshot, manifest, and findings for full review |
| `agent-sec-cli skill-ledger decide <dir> --action allow|always_allow|block|rollback` | Record a user decision and refresh activation |
| `agent-sec-cli skill-ledger decide <dir> --clear` | Clear the user decision on the latest manifest |
| `agent-sec-cli skill-ledger scan <dir>` | Run a quick scan and sign it into the manifest; a host-backed, read-only packaged system Skill is an error |
| `agent-sec-cli skill-ledger scan --all` | Gap-filling quick scan; host-backed, read-only packaged system Skills return operational `skipped` results |
| `agent-sec-cli skill-ledger certify <dir> --findings <file>` | Sign deep-scan findings into the manifest |
| `agent-sec-cli skill-ledger status` | Overall security posture (keys, config, Skill health) |
| `agent-sec-cli skill-ledger status --verbose` | Overall posture including per-Skill detailed results |
| `agent-sec-cli skill-ledger audit <dir>` | Deep-verify the version chain |
| `agent-sec-cli skill-ledger list-scanners` | List registered scanners |

`activationPolicy` and `show/export --policy` accept `pass_warn_only`. Use
`list-scanners` to find registered names for `init/scan --scanners` and
`certify --scanner`; custom scanners can be registered in `config.json`.

`rotate-keys` is visible in help and is not implemented: invoking it reports
`not implemented` on stderr, exits non-zero, and does not change `key.enc`,
`key.pub`, or the keyring.

## Key Paths

| Path | Purpose |
|------|---------|
| `~/.local/share/agent-sec/skill-ledger/key.enc` | Private key file (unencrypted by default, encrypted with `--passphrase`) |
| `~/.local/share/agent-sec/skill-ledger/key.pub` | Public key |
| `~/.local/share/agent-sec/skill-ledger/keyring/` | Archived historical public keys (after key rotation) |
| `~/.config/agent-sec/skill-ledger/config.json` | Configuration file (managedSkillDirs, scanners) |
| `<skill_dir>/.skill-meta/latest.json` | Current manifest (written by `scan`, `certify`, or the `init` baseline) |
| `<skill_dir>/.skill-meta/versions/` | Version chain history |
