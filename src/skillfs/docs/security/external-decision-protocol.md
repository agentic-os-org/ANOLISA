# SkillFS External Decision Protocol

## Purpose

This document defines a stable CLI/JSON protocol that external tools can
implement to tell SkillFS how to expose a skill through
`/skills/<skill>`. SkillFS is the filesystem enforcement and mapping
layer. External tools own all security judgment (signature checks,
scans, manifest verification, risk classification). SkillFS only
consumes the protocol result and translates it into a filesystem-level
decision (`current`, `fallback`, or `hidden`).

The protocol is intentionally generic. It is **not** tied to any
specific implementation. `agent-sec-cli skill-ledger` is one possible
provider; an internal scanner, a demo script, a wrapper around a future
daemon, or any other tool that produces a compatible JSON document on
stdout is equally valid.

## Architecture

```text
Agent
  |
  v
SkillFS mount path  --(spawn)-->  External Decision Provider
  ^                                       |
  |                                       v
  +-----<-- JSON decision (stdout) <-----+

           SkillFS consumes the JSON.
           SkillFS owns path safety.
           Provider owns security judgment.
```

The external decision provider can be:

- `agent-sec-cli skill-ledger` (current reference implementation),
- an internal scanner or in-process check wrapper exposed as a CLI,
- a small demo script that returns canned decisions,
- a future thin wrapper around a daemon/socket protocol.

SkillFS does not require any specific provider. SkillFS only requires
that the provider implements the invocation contract and produces a
compliant JSON object on stdout.

## Invocation Contract

SkillFS invokes the provider through a generic **decision-command**
prefix that the operator supplies once at mount time
(`--decision-command <COMMAND>`). The prefix can be either a single
binary or a whitespace-split command prefix with fixed arguments. From
that prefix SkillFS appends two subcommands:

```bash
<decision-command> scan <skill_dir> --json
<decision-command> resolve <skill_dir> --json
```

Where:

- `<decision-command>` is the operator-supplied command prefix. The
  first whitespace-separated token is the executable path or
  PATH-resolvable name; subsequent tokens are fixed arguments
  prepended to every per-call argv.
- `scan` is the optional pre-step. Providers may use it to refresh
  internal scan state, recompute findings, etc. SkillFS does **not**
  parse the scan stdout in D1.3.1; only the exit status is consumed.
  Exit `0` lets SkillFS proceed to `resolve`; a non-zero exit
  short-circuits the resolve and applies the demo failure behavior.
- `resolve` is the load-bearing subcommand. It is the only command
  whose JSON SkillFS validates and turns into an active mapping
  decision.
- `<skill_dir>` is the path to the physical skill directory.
- `--json` requests machine-readable output on stdout.

Examples:

```text
--decision-command "/usr/local/bin/xxx-cli"
spawns: /usr/local/bin/xxx-cli scan <skill_dir> --json
        /usr/local/bin/xxx-cli resolve <skill_dir> --json

--decision-command "agent-sec-cli skill-ledger"
spawns: agent-sec-cli skill-ledger scan <skill_dir> --json
        agent-sec-cli skill-ledger resolve <skill_dir> --json
```

### Parsing rules (D1.3.1)

- The command string is split on ASCII whitespace. Consecutive
  whitespace is collapsed.
- Shell quoting and backslash escaping are **not** supported in
  D1.3.1; a path containing spaces cannot currently be expressed.
- An empty or whitespace-only `--decision-command` is rejected at
  startup.
- SkillFS spawns the program directly via `std::process::Command`.
  There is no `sh -c`, so neither shell metacharacters nor PATH
  expansion beyond what `Command::new` already provides apply.
- `--ledger-demo-mode` requires `--decision-command`; passing demo
  mode without a decision-command is a startup error.

## Input

- `skill_dir`: the absolute or caller-provided path to the physical
  skill directory. SkillFS passes the path that it has already
  classified as a skill root.
- The provider may inspect `.skill-meta/`, signatures, manifests,
  version snapshots, scan results, audit ledgers, or any other private
  state it owns.
- SkillFS does not prescribe how the provider computes the decision.
  Providers are free to cache, batch internally, or short-circuit.

## Output

The provider MUST print exactly one JSON object to stdout on success.
Stdout MUST contain only JSON. Free-form logs, progress lines, or
warnings MUST go to stderr.

### Top-level fields

| Field            | Type                | Notes                                              |
|------------------|---------------------|----------------------------------------------------|
| `schemaVersion`  | integer             | Must be `1`.                                       |
| `skillName`      | string              | Must equal `basename(skill_dir)`.                   |
| `declaredName`   | string or null      | Optional `SKILL.md` frontmatter name observed by provider. |
| `status`         | string enum         | See allowed values below.                          |
| `decision`       | string enum         | One of `current`, `fallback`, `hidden`.            |
| `reason`         | string or null      | Human-readable explanation (may be `null`).        |
| `currentVersion` | string or null      | Provider-known current version, if any.            |
| `trustedVersion` | string or null      | Provider-known last trusted version, if any.       |
| `target`         | string or null      | Filesystem target for `fallback`; else `null`.     |
| `targetKind`     | string enum or null | `relative_to_skill_dir` for `fallback`; else null. |

Additional provider-private fields (for example `findingsSummary`,
`diffSummary`) are allowed. SkillFS MUST ignore unknown fields.

### Allowed `status` values

- `none`
- `pass`
- `warn`
- `deny`
- `drifted`
- `tampered`
- `error`

### Allowed `decision` values

- `current`
- `fallback`
- `hidden`

### Allowed `targetKind` values

- `null`
- `relative_to_skill_dir`

## Decision Semantics

### `current`

- SkillFS exposes the live skill directory.
- `target` MUST be `null`.
- `targetKind` MUST be `null`.

### `fallback`

- SkillFS exposes a trusted snapshot path relative to the skill
  directory.
- `target` MUST be non-null.
- `targetKind` MUST be `relative_to_skill_dir`.
- `target` MUST be exactly under
  `.skill-meta/versions/<version>.snapshot`.
- `target` MUST be relative; MUST NOT contain `..`; MUST NOT be
  absolute; MUST NOT escape the skill directory.

### `hidden`

- SkillFS omits the skill from `/skills` listings.
- `lookup` for the skill returns `ENOENT`.
- `target` MUST be `null`.
- `targetKind` MUST be `null`.

## Recommended Decision Table

Providers SHOULD follow this mapping when emitting `decision` based on
`status`:

| Provider status                              | Trusted version available | Recommended `decision` |
|----------------------------------------------|---------------------------|------------------------|
| `pass`                                       | any                       | `current`              |
| `none`                                       | no                        | `hidden`               |
| `warn` / `deny` / `drifted` / `tampered`     | yes                       | `fallback`             |
| `warn` / `deny` / `drifted` / `tampered`     | no                        | `hidden`               |
| `error`                                      | any                       | `hidden` (demo mode)   |

A future strict mode may override the `error` row by failing the mount
or refusing to expose the skill instead of silently hiding it.

## Example JSON

### `current`

```json
{
  "schemaVersion": 1,
  "skillName": "demo-weather",
  "status": "pass",
  "decision": "current",
  "reason": null,
  "currentVersion": "v000003",
  "trustedVersion": "v000003",
  "target": null,
  "targetKind": null
}
```

### `fallback`

```json
{
  "schemaVersion": 1,
  "skillName": "demo-weather",
  "status": "deny",
  "decision": "fallback",
  "reason": "current version has high-risk findings",
  "currentVersion": "v000003",
  "trustedVersion": "v000001",
  "target": ".skill-meta/versions/v000001.snapshot",
  "targetKind": "relative_to_skill_dir"
}
```

### `hidden`

```json
{
  "schemaVersion": 1,
  "skillName": "demo-weather",
  "status": "none",
  "decision": "hidden",
  "reason": "skill not yet certified",
  "currentVersion": null,
  "trustedVersion": null,
  "target": null,
  "targetKind": null
}
```

### `error` mapped to `hidden`

```json
{
  "schemaVersion": 1,
  "skillName": "demo-weather",
  "status": "error",
  "decision": "hidden",
  "reason": "provider failed to evaluate skill state",
  "currentVersion": null,
  "trustedVersion": null,
  "target": null,
  "targetKind": null
}
```

## Validation Rules SkillFS Applies

SkillFS MUST reject provider output that violates any of the following.
Rejected output MUST NOT be mapped to a filesystem target.

### Top-level

- Reject `schemaVersion != 1`.
- Reject unknown values for `decision`, `status`, or `targetKind`.

### `skillName`

`skillName` is the canonical SkillFS identity and must equal
`basename(skill_dir)` from the request SkillFS made. It is not derived from
`SKILL.md` frontmatter. If `SKILL.md` declares a different `name:`, the provider
may report that as optional `declaredName` and decide `warn`, `deny`, `hidden`,
or `fallback`, but SkillFS must not let declared content rename the path
identity.

Reject any `skillName` that is:

- different from `basename(skill_dir)` for the current request,
- empty,
- `.` or `..`,
- contains `/` or `\\`,
- contains a NUL byte,
- absolute,
- not a single normal path component,
- longer than `MAX_SKILL_NAME_LEN`.

### `declaredName`

`declaredName` is optional and represents the provider observation of the
`name:` field inside `SKILL.md`, if any. SkillFS does not use it as a path key.
Providers can use mismatch between `declaredName` and `skillName` as a security
or quality signal, but the active mapping remains keyed by `skillName` / the
physical directory name.

A `declaredName` that disagrees with the directory name does **not** create an
alias under `/skills/<declaredName>`. The provider may classify the mismatch
however it wants — `warn`, `deny`, `hidden`, or `fallback` — and SkillFS will
honor the decision against the canonical `skillName` only.

### `skillName` mismatch handling

When a resolve response carries a `skillName` that does not equal
`basename(skill_dir)` for the request that produced it, SkillFS rejects the
result before installing it into the active mapping. In demo mode the rejection
maps to the same handling as any other failed resolve:

- `HideOnFailure` (default): the canonical skill is hidden until a subsequent
  resolve succeeds.
- `KeepPreviousMapping`: the previous active mapping survives the rejection.

The bogus `skillName` is never used as an alternate key, so the mismatched name
cannot materialize under `/skills` even transiently.

### `fallback` target

Reject any `target` that:

- is absolute,
- contains `..`,
- pairs with a `targetKind` that is not `relative_to_skill_dir`,
- is not under `.skill-meta/versions/`,
- does not end in `.snapshot`.

### Demo policy (D1.x)

- Invalid or failed provider output MAY downgrade to a warning and
  hide the affected skill.
- A future strict mode MAY instead refuse the mount or fail the
  resolve outright.

## Exit Code And Stderr

- Exit code `0` with valid JSON on stdout: SkillFS accepts the
  decision.
- Non-zero exit code: provider failure. SkillFS treats this as an
  invalid resolve in the current demo mode.
- Stderr is diagnostic only and MUST NOT be parsed by SkillFS.
- Stdout MUST contain JSON only. All logs and progress messages MUST be
  written to stderr.

## Security Boundaries

- SkillFS enforces path safety on the returned `target`. SkillFS does
  not decide security status.
- The provider owns scan, check, signature, manifest, and risk logic.
- The snapshot target remains effectively read-only from the
  Agent-facing fallback path because any mutating open or write is
  redirected to the live source. Snapshots are not modified through
  the Agent view.
- `.skill-meta` writes are still protected by SkillFS policy
  (`SkillMetaProtectionPolicy`). Trusted writer identity for
  `.skill-meta` mutations is future work.
- This protocol is **not** an authorization protocol for processes
  yet. It does not authenticate the Agent or the provider, and it does
  not gate per-process access to skills.

## Non-Goals

The following are explicitly **out of scope** for the External Decision
Protocol as defined here:

- Daemon or socket transport.
- Streaming events from the provider to SkillFS.
- `check` / `scan` / `certify` command shapes (these remain provider-
  specific until a future package standardizes them).
- Trusted writer identity for `.skill-meta`.
- Skill lifecycle transition semantics (staging, certified,
  quarantine, archive).
- Production-grade fail-open / fail-closed policy.
- Agent process sandboxing or per-caller authorization.

These remain candidates for follow-up packages and are intentionally
left undefined here so that the External Decision Protocol can ship as
a stable minimal contract.
