# Configuration and Agent launch

[中文版](../../../zh/user-entrypoint/aw/configuration.md)

`aw.yaml` contains Agent entrypoints, enabled Providers and runtime data locations.
Model accounts remain in the Agent's own configuration. Complete [installation](installation.md) first.

## Generate configuration

These variables come from the installation page. Each `configure` command creates a new file.
Back up existing configuration and edit it as described below.

```bash
aw-package configure --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
```

| Capability | Add to the generation command |
| --- | --- |
| Launch the Agent only | No Provider arguments |
| Compress tool output | `--provider tokenless` |
| Check tool safety | `--provider sec-core --socket /run/agent-sec-core/daemon.sock` |
| Safety checks and compression | `--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock` |

Install the selected Providers first. The generator supplies managed executable paths;
you do not need to enter an RTK digest manually.

## Edit existing configuration

Exit Agents using the file and stop the service with the original configuration before editing.
Configuration is not hot-reloaded.

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw-package config show --config "$AW_HOME/aw.yaml"
```

Edit YAML directly or use `aw-package config` to add and remove Providers and Hooks.
The file and its parent must belong to the current user and must not be writable by others.
The editor requires an absolute path and rejects symlinks. `show` includes private configuration
values; remove sensitive data before sharing its output.

Validate your edits and start again.

```bash
aw-package config validate --config "$AW_HOME/aw.yaml"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Successful validation prints `Valid configuration`. It checks fields and references;
program availability is checked at launch. Write actual absolute paths in YAML;
AW does not expand variables such as `$HOME`.
For all editor commands and a custom rule example, see the
[configuration tool reference](../../../../developer-guide/en/aw/packages.md#edit-existing-provider-and-hook-configuration).

## Choose an Agent

A configuration can contain multiple targets. `--agent` selects a name under `spec.agents`.
The generator currently supports Qoder and OpenClaw. For QwenPaw, Hermes and SDK applications,
use the [native integration reference](../../../../developer-guide/en/aw/native-agents.md).

| Agent | Current requirements | Entrypoint |
| --- | --- | --- |
| Qoder | CLI 1.1.64 or later with native Hooks available | Interactive or print |
| OpenClaw | 2026.9.6 with initialized native configuration and state | A newly launched Gateway |
| QwenPaw | 2.2.2b4 with AgentScope 2.0.9 | App/API |
| Hermes | Follow the [compatible version and setup](../../../../developer-guide/en/aw/native-agents.md#start-hermes) | Local chat |
| AgentScope, Pi, Claude SDK | Explicit application wiring required | [SDK integration reference](../../../../developer-guide/en/aw/native-agents.md) |

### Qoder

Launch from the intended working directory. Arguments after `--` go to Qoder.

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

For an additional settings file, use AW's `--native-settings /absolute/qoder.settings.json`.
AW preserves existing settings and adds callbacks for this run.

### OpenClaw

Stop any Gateway using the same native profile first. Replace these variables with absolute
paths to your existing installation and profile.

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw-package configure --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

Wait for `AW openclaw native hooks ready`, then use a client connected to that Gateway.
Add the Provider arguments above when generating configuration for compression or safety checks.
`agent --local` bypasses this Gateway.

## Add your own rules

A simple rule reads a tool event from stdin and writes JSON `true` or `false` to stdout.
Configuration chooses observation or blocking when the rule matches. `on_error` controls execution
failures; safety checks typically use `block`. Follow the
[boolean rule tutorial](../../../../developer-guide/en/aw/integration.md) to prepare a script and verify
both allowed and blocked calls.
