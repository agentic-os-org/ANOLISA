# OpenClaw Compatibility Deployment & Upgrade Guide

This guide covers deploying, upgrading, rolling back, and troubleshooting the AgentSecCore OpenClaw plugin.

## Scope

The OpenClaw host compatibility boundary of the AgentSecCore OpenClaw plugin is `>=2026.4.14`.
The boundary is kept consistent in the following locations:

- `openclaw.install.minHostVersion` in `openclaw-plugin/package.json`
- `openclaw.compat.pluginApi` in `openclaw-plugin/package.json`
- `peerDependencies.openclaw` in `openclaw-plugin/package.json`

The following matrix records the earlier CI validation:

| OpenClaw host | Result |
|---------------|--------|
| `2026.4.14` | Pass |
| `2026.4.23` | Pass |
| `2026.4.24` | Pass |
| `2026.4.29` | Pass |
| `2026.5.7` | Pass |
| `2026.5.28` | Pass |
| `2026.6.10` | Pass |
| `latest` | Pass |

Validation evidence: GitHub Actions `OpenClaw Plugin E2E` run `28774739252`.

Local E2E validation also passed on `2026.8.1` and the legacy `2026.4.23` host
with Node `22.22.3` and installed `agent-sec-cli 0.13.0`: installation, runtime
loading, Gateway traffic, all four policy cases, and 14 direct hook probes.
The suite uses a mock model and deterministic deny fixtures; on `2026.8.1`,
the approval case verified blocking when no approval route was available.

OpenClaw 2.0 is release `2026.8.1`. It requires Node `>=22.22.3 <23`,
`>=24.15.0 <25`, or `>=25.9.0`. Its plugin install records live in host-managed
SQLite state; use `plugins inspect` to verify installation rather than reading
the retired `plugins.installs` config key.

## Prerequisites

The deployment host needs:

- `openclaw`
- `agent-sec-cli`
- `jq`
- A built OpenClaw plugin `dist/index.js`
- `openclaw-plugin/openclaw.plugin.json`

`deploy.sh` checks these before running; it fails immediately with the reason when anything is missing.

## Deploying from Source

Run in the `agent-sec-core` repository root:

```bash
make build-openclaw-plugin
```

This target builds the TypeScript and places `openclaw.plugin.json`, `package.json`,
`dist/`, and `scripts/` into `target/openclaw-plugin/`.

Then install to the target directory. The default source-install path is controlled
by the Makefile variable `OPENCLAW_PLUGIN_DIR`, defaulting to
`/usr/local/lib/anolisa/sec-core/openclaw-plugin`:

```bash
sudo make install-openclaw-plugin
```

Finally, run the deploy script to register the plugin:

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
```

To install to the path used by the RPM profile, pass `OPENCLAW_PLUGIN_DIR` explicitly:

```bash
sudo make install-openclaw-plugin OPENCLAW_PLUGIN_DIR=/opt/agent-sec/openclaw-plugin
sudo /opt/agent-sec/openclaw-plugin/scripts/deploy.sh \
    /opt/agent-sec/openclaw-plugin
```

In a source development environment you can also build and deploy directly inside the plugin directory:

```bash
cd openclaw-plugin
npm install
npm run build
./scripts/deploy.sh "$(pwd)"
```

If OpenClaw uses a non-default state directory, pass `OPENCLAW_STATE_DIR` at deploy time:

```bash
OPENCLAW_STATE_DIR=~/.openclaw-dev ./scripts/deploy.sh "$(pwd)"
```

## What deploy.sh Does

`deploy.sh` handles install-time compatibility:

- Reads `openclaw --version` and requires OpenClaw `>=2026.4.14`
- Reads `openclaw plugins install --help` to confirm `--force` support
- Passes `--accept-capabilities` when supported, accepting the declared capabilities of agent-sec (required by OpenClaw 2.0)
- On older hosts without capability consent, passes `--dangerously-force-unsafe-install` only when advertised; on modern hosts that legacy flag is a no-op
- Preserves operator `security.installPolicy` decisions; capability acceptance does not bypass a policy block
- Writes `plugins.entries.agent-sec.hooks.allowConversationAccess=true` on OpenClaw `>=2026.4.24`
- Skips `allowConversationAccess` on OpenClaw `2026.4.14` through `2026.4.23`
- Validates the install record via `openclaw plugins inspect agent-sec --json`
- Validates runtime loading via `openclaw plugins inspect agent-sec --runtime --json` when the current OpenClaw supports `plugins inspect --runtime`
- Fails when the inspected plugin status is not `loaded`

`deploy.sh` never starts, stops, or restarts the OpenClaw gateway.

## Restarting the Gateway

After deploying or upgrading the plugin, restart the OpenClaw gateway:

```bash
openclaw gateway restart
```

If the environment uses a systemd user service, use the corresponding service restart command:

```bash
systemctl --user restart openclaw-gateway-dev.service
```

## Verifying the Installation

First verify the OpenClaw install record:

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.id == "agent-sec"'
```

If the current OpenClaw supports runtime inspect, also verify runtime loading:

```bash
openclaw plugins inspect agent-sec --runtime --json | jq -e '.plugin.status == "loaded"'
```

On OpenClaw versions without `--runtime`, use `plugin.status` from the plain inspect:

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.status == "loaded"'
```

After deploying on OpenClaw `>=2026.4.24`, also confirm the config contains:

```bash
openclaw config get plugins.entries.agent-sec.hooks.allowConversationAccess
```

The expected value is `true`.

## Default Security Policy

The default configuration is observation-first:

- `promptScanBlock=false`: the prompt scanner logs an alert on `deny` findings but does not block the model call
- `codeScanRequireApproval=false`: the code scanner logs an alert on risks but does not prompt for approval
- `piiScanUserInput=true`: scans user input for PII and credentials
- `piiIncludeLowConfidence=false`: excludes low-confidence PII findings
- `pii-scan-user-input.enableBlock=false`: PII deny does not block by default
- `skill-ledger.policy=ask`: prefers user confirmation when there is a user-visible message
- `observability.enabled=true`: enables observability recording

Enable prompt blocking:

```bash
openclaw config set plugins.entries.agent-sec.config.promptScanBlock true
```

You can also influence prompt scanner behavior at deployment time with these environment
variables. `PROMPT_SCANNER_HOOK_ENABLED=false` skips hook registration regardless of plugin
configuration:

| Environment variable | Default | Behavior |
|----------------------|---------|----------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | Set to `false` to skip prompt-scan hook registration entirely |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | Scan strength passed to `scan-prompt`: `fast` / `standard` / `strict` |

The OpenClaw plugin does not read `PROMPT_SCANNER_MODE` or `PROMPT_SCANNER_TIMEOUT`. The prompt
policy on OpenClaw is governed by `promptScanBlock` above, and the scanner timeout is fixed at
10 seconds.

Restart the OpenClaw gateway after changing these variables.

Enable code-scan approval:

```bash
openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true
```

For deployment-level overrides, `CODE_SCANNER_HOOK_ENABLED=true|false` takes precedence over `capabilities["scan-code"].enabled`, and `CODE_SCANNER_MODE=observe|ask|block` takes precedence over `codeScanRequireApproval`. `debug` aliases `observe`, `deny` aliases `block`, and `warn` or invalid values are treated as unset and fall back to plugin configuration. In `ask` mode, ordinary findings return `requireApproval`; in `block` mode, ordinary findings return `{ block: true, blockReason }`. The existing self-protect rule remains a forced-block exception regardless of mode. OpenClaw does not consume `CODE_SCANNER_TIMEOUT` and keeps its fixed 10-second timeout.

Enable PII deny blocking:

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.pii-scan-user-input.enableBlock' true
```

Configure Skill Ledger to block directly:

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.skill-ledger.policy' block
```

## Runtime Compatibility Policy

Do not write persistent hook-disable configuration for older OpenClaw versions.

AgentSecCore's policy is:

- `before_dispatch`, `before_tool_call`, and `after_tool_call` are the core security hooks within the support matrix
- `model_call_started` and `model_call_ended` are optional model-call observability hooks
- `llm_input`, `llm_output`, and `agent_end` require `allowConversationAccess`
- When an older OpenClaw lacks the optional observability hooks, the plugin degrades observability gracefully
- After upgrading OpenClaw, re-run `deploy.sh` and restart the gateway to gain the hook behavior supported by the new version

If missing hooks were written as persistent disable configuration, they may stay disabled by the stale config after an OpenClaw upgrade — so do not do that.

## Upgrade Procedure

Upgrade the AgentSecCore OpenClaw plugin:

```bash
make build-openclaw-plugin
sudo make install-openclaw-plugin
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

After upgrading the OpenClaw host, also re-run `deploy.sh`:

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

The reason is that a newer OpenClaw may support configuration or hooks the previous
version lacked, such as `plugins.entries.agent-sec.hooks.allowConversationAccess`.

## Rollback Procedure

If a newly deployed plugin version needs to be rolled back:

1. Restore the previous plugin directory to the target path.
2. Re-run `deploy.sh` from the previous version's directory.
3. Restart the OpenClaw gateway.
4. Verify status with `openclaw plugins inspect agent-sec --json` and the runtime inspect.

Example:

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
openclaw plugins inspect agent-sec --json | jq -e '.plugin.id == "agent-sec"'
```

## Troubleshooting

### Plugin installation fails

First confirm the commands are available:

```bash
openclaw --version
agent-sec-cli --help
jq --version
```

Then confirm the plugin directory exists:

```bash
test -f /usr/local/lib/anolisa/sec-core/openclaw-plugin/openclaw.plugin.json
test -f /usr/local/lib/anolisa/sec-core/openclaw-plugin/dist/index.js
```

### Runtime inspect is not `loaded`

Run:

```bash
openclaw plugins inspect agent-sec --runtime --json
```

and inspect `diagnostics`. If the current OpenClaw does not support `--runtime`, use:

```bash
openclaw plugins inspect agent-sec --json
```

### Conversation observability hooks are blocked

OpenClaw `>=2026.4.24` requires:

```bash
openclaw config set plugins.entries.agent-sec.hooks.allowConversationAccess true
openclaw gateway restart
```

OpenClaw `2026.4.14` through `2026.4.23` does not support this configuration. Core security hooks remain available, but session-level observability hooks degrade.

### Observability unchanged after upgrading OpenClaw

Re-run:

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

Do not manually keep hook-disable configuration written under the previous version.
