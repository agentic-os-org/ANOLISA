# agent-sec OpenClaw Plugin

OpenClaw security plugin that hooks into the agent lifecycle via `agent-sec-cli`, providing code scanning, skill integrity verification, prompt analysis, PII checking, and best-effort agent observability logging.

---

## Prerequisites

| Dependency     | Version   | Check                        |
|----------------|-----------|------------------------------|
| Node.js        | See OpenClaw 2.0 compatibility below | `node --version` |
| npm            | >= 10     | `npm --version`              |
| OpenClaw       | >= 2026.4.14 | `openclaw --version`       |
| agent-sec-cli  | (latest)  | `agent-sec-cli --help`       |
| jq             | >= 1.6    | `jq --version`               |

Development and test builds use OpenClaw `2026.8.1` (OpenClaw 2.0), pinned in `package.json` so TypeScript checks the target release's hook definitions. The OpenClaw runtime does not need to match that dev dependency. Runtime compatibility is capability-based: `2026.4.14` and `2026.4.23` are supported legacy baselines for core security hooks, while `model_call_started` and `model_call_ended` telemetry may degrade cleanly on those versions.

## Compatibility Contract

The installable package declares the OpenClaw compatibility boundary in `package.json`:

- `openclaw.install.minHostVersion: ">=2026.4.14"` makes older OpenClaw hosts fail clearly during plugin install or non-bundled manifest loading.
- `openclaw.compat.pluginApi: ">=2026.4.14"` declares the plugin SDK/runtime API floor used by OpenClaw install compatibility checks.
- `peerDependencies.openclaw: ">=2026.4.14"` keeps npm peer metadata aligned with the OpenClaw installer contract.

Package entrypoints follow the current OpenClaw plugin contract: both `openclaw.extensions` and `openclaw.runtimeExtensions` point at the built JavaScript entry. `openclaw.plugin.json` keeps its legacy `extensions` declaration pointed at the same built runtime entry for older manifest readers.

Deployment & upgrade guide: [OpenClaw Compatibility Deployment & Upgrade Guide](../../../docs/user-guide/en/agent-security/agent-sec-core/openclaw-deploy.md) ([中文版](../../../docs/user-guide/zh/agent-security/agent-sec-core/openclaw-deploy.md)).

---

## Project Structure

```
openclaw-plugin/
├── src/                        # TypeScript source
│   ├── index.ts                # Plugin entry point (definePluginEntry)
│   ├── types.ts                # SecurityCapability interface
│   ├── utils.ts                # CLI invocation utility (callAgentSecCli)
│   ├── capabilities/           # Security capability entry files
│   │   ├── skill-ledger.ts     #   before_tool_call hook
│   │   ├── code-scan.ts        #   before_tool_call hook
│   │   ├── prompt-scan.ts      #   before_dispatch hook
│   │   ├── pii-scan.ts         #   PII hooks
│   │   └── observability.ts    #   observability hook registration
│   └── helpers/                # Capability support code
│       └── observability/      #   OpenClaw → agent-sec observability adapter
│           ├── schema.ts       #     hook mapping + metric allowlist
│           ├── record.ts       #     record assembly + metadata validation
│           ├── metrics.ts      #     hook-specific metric extraction
│           ├── extractors.ts   #     response/error extraction helpers
│           ├── helpers.ts      #     generic parsing helpers
│           └── types.ts        #     shared observability types
├── tests/                      # Test utilities (not compiled into dist/)
│   ├── e2e/                    # OpenClaw plugin E2E pilot runner
│   ├── test-harness.ts         # Mock OpenClaw API for local testing
│   ├── smoke-test.ts           # Smoke test for all capabilities
│   └── unit/                   # Unit tests
│       ├── code-scan-test.ts   #   scan-code handler tests
│       ├── observability-test.ts # observability handler tests
│       └── skill-ledger-test.ts #  skill-ledger handler tests
├── scripts/
│   └── deploy.sh               # Deployment and registration script
├── dist/                       # Compiled JS output (gitignored)
├── openclaw.plugin.json        # Plugin manifest
├── package.json
└── tsconfig.json
```

---

## Build

### Install Dependencies

```bash
cd src/agent-sec-core/openclaw-plugin
npm install
```

### Compile TypeScript

```bash
npm run build
```

This runs `tsc --project tsconfig.json` and outputs compiled JS to `dist/`.

### Verify Build Output

```bash
ls dist/
# Expected: capabilities/  index.js  index.d.ts  types.js  types.d.ts  utils.js  utils.d.ts
```

> **Note:** Test files in `tests/` are excluded from `dist/` since they live outside `src/`.

---

## OpenClaw 2.0 compatibility

OpenClaw `2026.8.1` requires Node `>=22.22.3 <23`, `>=24.15.0 <25`, or
`>=25.9.0`. Use a compatible Node version for dependency installation and E2E.
Deployment accepts this plugin's declared capabilities through the host's
`--accept-capabilities` flag; `--force` alone does not grant consent. Operator
`security.installPolicy` blocks still apply. Conversation access remains enabled
separately for content inspection hooks.

Hook callbacks infer their types from `api.on`; they do not import the hook types
removed from `plugin-runtime`. E2E policy assertions use `sessions.get`, so they
work with both legacy JSONL and 2.0 SQLite transcripts.

## Deploy to OpenClaw

### Option A: Deploy from Source (Development)

Point `deploy.sh` directly at the source directory:

```bash
# Build first
npm run build

# Deploy — pass the plugin directory as argument
./scripts/deploy.sh "$(pwd)"
```

### Option B: Deploy from Packaged Tarball

```bash
# Create tarball
npm run pack
# Output: agent-sec-openclaw-plugin-0.x.y.tgz

# Extract to target directory
mkdir -p /opt/agent-sec/openclaw-plugin
tar -xzf agent-sec-openclaw-plugin-0.x.y.tgz \
    --strip-components=1 \
    -C /opt/agent-sec/openclaw-plugin

# Deploy
./scripts/deploy.sh /opt/agent-sec/openclaw-plugin
```

### Option C: Install via Makefile (Development/Testing)

```bash
# From agent-sec-core root directory
cd src/agent-sec-core

# Build the plugin
make build-openclaw-plugin

# Install files to the default source-build path:
# /usr/local/lib/anolisa/sec-core/openclaw-plugin/
sudo make install-openclaw-plugin

# Register the plugin with OpenClaw
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin

# Restart gateway to load the plugin
openclaw gateway restart
```

> **Note:** `make install-openclaw-plugin` only copies files. You must run `deploy.sh` separately to register the plugin.
> To install into `/opt/agent-sec/openclaw-plugin`, pass `OPENCLAW_PLUGIN_DIR=/opt/agent-sec/openclaw-plugin` to `make install-openclaw-plugin`.

---

## What `deploy.sh` Does

The deployment script performs these steps:

1. **Pre-checks** — Verifies `agent-sec-cli` and `jq` are in PATH; validates `openclaw.plugin.json`, `dist/`, and `dist/index.js` exist
2. **OpenClaw version check** — Reads `openclaw --version` and requires OpenClaw `>=2026.4.14`
3. **Installer capability check** — Reads `openclaw plugins install --help`, requires `--force`, and detects capability-consent or legacy install flags
4. **Inspect capability check** — Reads `openclaw plugins inspect --help`, requires `--json`, and detects whether `--runtime` is available
5. **Plugin installation** — Runs `openclaw plugins install <path> --force`, adding `--accept-capabilities` when supported; older installers use `--dangerously-force-unsafe-install` only when advertised
6. **Conversation access policy** — Sets `plugins.entries.agent-sec.hooks.allowConversationAccess=true` on OpenClaw `>=2026.4.24`; older supported hosts skip it and degrade conversation observability hooks
7. **Runtime verification** — Uses `openclaw plugins inspect agent-sec --runtime --json` when supported, otherwise `openclaw plugins inspect agent-sec --json`, and fails unless the plugin status is `loaded`
8. **User guidance** — Displays instructions to restart the OpenClaw gateway and optional policy commands (does NOT restart automatically)

> **Important:** `deploy.sh` installs the plugin, applies supported OpenClaw config, and verifies the plugin load status. It does **NOT** start/stop/restart the gateway service.
> 
> To restart the gateway:
> ```bash
> openclaw gateway restart  # Recommended: OpenClaw CLI
> # Or
> systemctl --user restart openclaw-gateway-dev.service  # If using systemd user service
> ```

### Custom State Directory

```bash
OPENCLAW_STATE_DIR=~/.openclaw-dev ./scripts/deploy.sh "$(pwd)"
```

---

## Verify Installation

First verify the OpenClaw install record:

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.id == "agent-sec"'
```

If the current OpenClaw supports runtime inspect, verify the runtime load state:

```bash
openclaw plugins inspect agent-sec --runtime --json | jq -e '.plugin.status == "loaded"'
```

Older supported OpenClaw versions that do not support `--runtime` use the ordinary inspect status:

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.status == "loaded"'
```

OpenClaw `>=2026.4.24` should also have conversation access enabled:

```bash
openclaw config get plugins.entries.agent-sec.hooks.allowConversationAccess
```

---

## Testing

### Smoke Test (Mock Mode)

Runs all capabilities against mock events without requiring a real `agent-sec-cli` installation:

```bash
npm run smoke
```

### Smoke Test (Live Mode)

Runs against the real `agent-sec-cli` binary:

```bash
AGENT_SEC_LIVE=1 npm run smoke
```

### OpenClaw Plugin E2E Pilot

`npm run e2e:openclaw` is the reusable pilot entry for
`OPENCLAW-PLUGIN-E2E-PILOT`. It uses isolated state directories, builds and
packs this plugin, starts `agent-sec-daemon`, installs the plugin into the
selected OpenClaw runtime, starts a local Gateway, verifies runtime plugin
loading, and writes structured evidence to `pilot-result.json`.

```bash
npm run e2e:openclaw -- \
  --openclaw-bin /path/to/openclaw \
  --agent-sec-cli /path/to/agent-sec-cli \
  --agent-sec-daemon /path/to/agent-sec-daemon
```

If the binaries are already on `PATH` or in the repository root `.venv/bin/`,
the explicit `--agent-sec-*` arguments can be omitted. From the repository root,
`source .venv/bin/activate` also works for manual runs, but the pilot runner
does not require the shell to be activated because it detects `.venv/bin`
directly. The same values can be provided with
`AGENT_SEC_OPENCLAW_PILOT_AGENT_SEC_CLI` and
`AGENT_SEC_OPENCLAW_PILOT_AGENT_SEC_DAEMON`. Use `--workdir <dir>` to keep
logs and artifacts in a stable directory. An existing work directory must be owned by the current user and have mode `0700`, as required by the daemon.

Optional pilot arguments:

| Option | Purpose |
|--------|---------|
| `--workdir <dir>` | Store isolated OpenClaw state, daemon data, logs, and `pilot-result.json` under a stable directory. |
| `--openclaw-bin <path>` | Use a specific OpenClaw executable or `openclaw.mjs`. |
| `--agent-sec-cli <path>` | Use a specific installed `agent-sec-cli` binary. |
| `--agent-sec-daemon <path>` | Use a specific installed `agent-sec-daemon` binary. |
| `--port <port>` | Bind Gateway to an explicit port. Without this, the runner picks a local port and retries initial startup if that port is claimed before Gateway binds. |
| `--gateway-timeout-ms <ms>` | Override the Gateway health wait budget. |
| `--gateway-token <token>` | Override the generated local Gateway token. |
| `--skip-gateway` | Stop after install/runtime inspection and skip Gateway traffic plus policy matrix probes. |
| `--skip-failure-probes` | Skip direct hook fail-open probes for missing, broken, invalid, and slow CLI behavior. |

The pilot runner does not control unsafe-install behavior. It calls `deploy.sh`
directly and records the actual install command path through the deploy
stdout/stderr logs.

The runner starts the local Gateway with token auth and redacts the token in
recorded command arguments. Override the generated local token with
`--gateway-token` or `AGENT_SEC_OPENCLAW_PILOT_GATEWAY_TOKEN` only when needed.
The default Gateway health wait is 180 seconds because first startup from an
OpenClaw source checkout may build Control UI assets before binding the port.

The runner records:

- `openclaw --version`, `agent-sec-cli --version`, and package tarball path
- isolated `OPENCLAW_STATE_DIR`, `OPENCLAW_CONFIG_PATH`, daemon socket, and
  Gateway URL
- `agent-sec-daemon` health over the Unix socket
- `deploy.sh` stdout/stderr and whether the unsafe install flag was used
- `openclaw plugins inspect agent-sec --runtime --json` summary
- Gateway-driven `sessions.create` / `sessions.send` / `agent.wait` evidence
  against a local OpenAI-compatible mock model, including a model-issued `exec`
  tool call and tool-result follow-up request
- Gateway log evidence that `prompt-scan` and `scan-code` ran on that real turn
- `AGENT_SEC_DATA_DIR/observability.jsonl` records for the same Gateway run ID
- hook-level capability probes for `prompt-scan`, `scan-code`,
  `pii-scan-user-input`, `skill-ledger`, and `observability`
- negative fail-open probes for missing CLI, nonzero exit, invalid JSON, and
  timeout

The Gateway traffic probe proves the installed plugin participates in a real
OpenClaw chat/tool turn. The direct hook probes remain as supplemental capability
and fail-open coverage for edge cases that are hard to force through a single
model turn. The matrix task should keep this pilot as the bootstrap lane and
run the same acceptance checks per supported host version.

---

## Plugin Capabilities

| Capability         | Hook                  | Priority | Behavior                                             |
|--------------------|-----------------------|----------|------------------------------------------------------|
| `pii-scan-user-input` | `before_dispatch`, `before_tool_call`, `after_tool_call`, `llm_output` | 200 before dispatch/tool call | Scans user text, tool parameters, tool output, and model output for PII/credentials; applies the unified hook policy |
| `prompt-scan`      | `before_dispatch`     | 190      | Scans inbound messages for prompt injection attacks   |
| `scan-code`        | `before_tool_call`    | 0 (default) | Scans tool commands for security issues              |
| `skill-ledger`     | `before_tool_call`    | 80       | Checks Skill Ledger exposure summary when SKILL.md is read; default policy asks on actionable messages |
| `observability`    | selected typed hooks  | varies   | Sends observability records to agent-sec-cli          |

### Configuring `code-scan`

The `scan-code` capability intercepts `exec` tool calls and scans commands via `agent-sec-cli scan-code`. By default, security issues are logged (`api.logger.warn`) but the tool call is allowed to proceed. This avoids blocking TUI users who cannot see Dashboard approval cards.

Set `codeScanRequireApproval: true` to enable approval mode, which pops a confirmation card on the Dashboard for `warn` and `deny` verdicts:

```bash
openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true
```

`CODE_SCANNER_HOOK_ENABLED=true|false` overrides `capabilities["scan-code"].enabled`, and `CODE_SCANNER_MODE=observe|ask|block` overrides `codeScanRequireApproval`. `debug` aliases `observe`; `deny` aliases `block`. `ask` returns `requireApproval` for scanner `warn` / `deny` verdicts, while `block` returns OpenClaw's existing `{ block: true, blockReason }` response for those ordinary findings. `warn` and invalid values are treated as unset, emit a bounded host-logger diagnostic, and use the original plugin configuration. The existing self-protect finding remains a forced-block exception regardless of mode. OpenClaw keeps its fixed 10-second timeout and does not read `CODE_SCANNER_TIMEOUT`.

### Configuring `pii-scan-user-input`

The `pii-scan-user-input` capability scans the current inbound user text in `before_dispatch`, tool parameters in `before_tool_call`, tool results/errors in `after_tool_call`, and assistant text in `llm_output`. It intentionally does not scan assembled prompt history, memory, or RAG context, so older PII does not trigger repeated warnings on later turns.

By default, `capabilities["pii-scan-user-input"].policy` is `observe`, so findings are audited without a user-visible warning. `warn` logs redacted warnings, `ask` requests approval for supported pre-tool calls and otherwise falls back to `warn`, and `block` rejects pre-execution `deny` verdicts. Tool output and model output cannot undo side effects and therefore fall back to warnings. Legacy `enableBlock: true/false` maps to `block/warn` when `policy` is absent.

### Configuring `observability`

The `observability` capability is enabled by default and invokes:

```bash
agent-sec-cli observability record --format json --stdin
```

Set `OBSERVABILITY_HOOK_ENABLED=false` before starting the OpenClaw gateway to
disable all observability hook work without changing plugin configuration. An
unset or invalid value keeps it enabled; restart the gateway after changing the
variable. Setting `capabilities.observability.enabled` to `false` also disables
the capability, so either setting can turn observability off.
`OBSERVABILITY_TIMEOUT` controls the timeout in seconds for each PII redaction
and observability record CLI call and defaults to `5`. Empty, invalid, or
non-positive values also use `5`, and larger values are capped at `5`.

Each hook emits one JSON record with `hook`, `observedAt`, `metadata`, and hook-specific `metrics`. The plugin registers OpenClaw hook names, but sends the generic `agent-sec-cli` hook name in `payload.hook`. Failures, missing CLI, malformed output, and timeouts are fail-open and never block OpenClaw behavior.

OpenClaw runtimes that expose `model_call_started` and `model_call_ended` provide model-call telemetry. Older runtimes load the plugin but skip unknown telemetry sources. Newer OpenClaw versions may provide richer fields on those hooks; the plugin sends whichever accepted metrics are present.

Observed hooks and metrics:

| OpenClaw hook | agent-sec-cli hook | Metrics sent |
|---------------|--------------------|--------------|
| `llm_input` | `before_agent_run` | `prompt`, `system_prompt`, `user_input`, `history_messages_count`, `images_count`, `context_window_utilization`, `model_id`, `model_provider` |
| `model_call_started` | `before_llm_call` | `model_id`, `model_provider`, `api`, `transport` |
| `model_call_ended` | `after_llm_call` | `latency_ms`, `outcome`, `error_category`, `failure_kind`, `request_payload_bytes`, `response_stream_bytes`, `time_to_first_byte_ms`, `upstream_request_id_hash` |
| `llm_output` | `after_agent_run` | `response`, `output_kind`, `stop_reason`, `assistant_texts_count`, `tool_calls_count`, `tool_calls` |
| `before_tool_call` | `before_tool_call` | `tool_name`, `parameters` |
| `after_tool_call` | `after_tool_call` | `result`, `error`, `duration_ms`, `status`, `exit_code`, `result_size_bytes` |
| `agent_end` | `after_agent_run` | `success`, `error`, `duration_ms`, `total_api_calls`, `total_tool_calls`, `final_model_id`, `final_model_provider` |

If an OpenClaw hook does not provide required metadata or any metric accepted by the current `agent-sec-cli` schema, the plugin skips the record instead of sending an invalid payload.
`llm_input` and `llm_output` are run-level OpenClaw hooks in current runtimes, so the plugin maps them to `before_agent_run` and `after_agent_run`. Per-call telemetry remains on `model_call_started` and `model_call_ended`.
`agent_end` records run status and aggregate counters only; final response content comes from `llm_output`.

Supported OpenClaw plugin entry config:

```json
{
  "plugins": {
    "entries": {
      "agent-sec": {
        "config": {
          "promptScanBlock": false,
          "codeScanRequireApproval": false,
          "piiScanUserInput": true,
          "piiIncludeLowConfidence": false,
          "capabilities": {
            "scan-code": { "enabled": true },
            "prompt-scan": { "enabled": true },
            "pii-scan-user-input": { "enabled": true, "enableBlock": false },
            "skill-ledger": {
              "enabled": true,
              "policy": "ask"
            },
            "observability": { "enabled": true }
          }
        },
        "hooks": {
          "allowConversationAccess": true
        }
      }
    }
  }
}
```

Set a capability's `enabled` value to `false` to skip registering only that capability while keeping the rest of the `agent-sec` plugin active. `skill-ledger` is enabled by default with `policy: "ask"` so actionable Skill Ledger exposure messages request user approval instead of silently hiding the context.
Set `enableBlock` on supported capabilities to control whether matching security findings block or ask the user for approval.

`llm_input`, `llm_output`, and `agent_end` require OpenClaw to allow conversation access for this external plugin with `plugins.entries.agent-sec.hooks.allowConversationAccess=true`. Without that OpenClaw setting, those hooks are blocked by OpenClaw before this plugin sees them.

### Configuring `skill-ledger`

The recommended Skill Ledger deployment is SkillFS + Skill Ledger daemon activation: SkillFS observes skill changes and the daemon refreshes `.skill-meta/activation.json`/xattr. The OpenClaw `skill-ledger` capability is still mounted by default and calls `agent-sec-cli skill-ledger show` so hook prompts, manual `show`, and activation resolution share the same exposure summary.

Default behavior:

- `enabled: false` fully disables registration.
- `policy: "ask"` is the default. It allows silent summaries and returns OpenClaw `requireApproval` when `show.message` is non-empty.
- `policy: "warn"` logs warning-level diagnostics for non-empty `show.message` but allows the read.
- `policy: "observe"` logs debug diagnostics for non-empty `show.message` and allows the read.
- `policy: "block"` blocks the read when `show.message` is non-empty and uses that message as the block reason.
- `latestStatus: "unmanaged"` is a Skill Ledger diagnostic state with `show.message: null`; every policy, including `block`, allows it silently.
- Legacy configs without `policy` still map `enableBlock: true` to `block` and `enableBlock: false` to `warn`.
- Legacy `debug` and `deny` policy values are accepted as aliases for `observe` and `block`.

Deployment environment variables override capability configuration. Use
`SKILL_LEDGER_HOOK_ENABLED`, `PII_CHECKER_HOOK_ENABLED`,
`PROMPT_SCANNER_HOOK_ENABLED`, `SKILL_LEDGER_MODE`, and `PII_CHECKER_MODE` for
deployment-level control. This plugin does not read `PROMPT_SCANNER_MODE`; its
prompt policy is governed by `promptScanBlock`.
Disabling Skill Ledger short-circuits before key initialization;
disabling prompt-scan short-circuits before hook registration.
  `blockStatuses` is accepted as deprecated configuration metadata but no longer controls runtime decisions.

Set `policy: "warn"` when wanting visible diagnostics without approval:

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.skill-ledger.policy' warn
```

Set `policy: "block"` to reject any Skill Ledger exposure summary that carries a user-visible message:

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.skill-ledger.policy' block
```

Skill Ledger global `activationPolicy` belongs to SkillFS/daemon activation. OpenClaw `policy` only controls this host hook's user-visible behavior and log level. User decisions must be made with `agent-sec-cli skill-ledger decide`; approving an OpenClaw prompt does not write a Ledger decision.

**Prerequisites**: `agent-sec-cli skill-ledger show` must be available. Signing keys are auto-initialized (no passphrase) if not present.

---

## Upgrade

To upgrade the plugin to a new version:

### Development Environment

```bash
cd src/agent-sec-core/openclaw-plugin

# Pull latest changes
git pull

# Rebuild
npm install
npm run build

# Re-register plugin (updates to new version)
./scripts/deploy.sh "$(pwd)"

# Restart gateway
openclaw gateway restart
```

### Production Environment (Installed via Makefile)

```bash
cd src/agent-sec-core

# Rebuild and install files
make build-openclaw-plugin
sudo make install-openclaw-plugin

# Re-register plugin
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin

# Restart gateway
openclaw gateway restart
```

The `openclaw plugins install --force` command automatically updates the plugin to the new version. Other plugins are unaffected.
