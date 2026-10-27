# Configuration and launch

[中文版](../../zh/aw/configuration.md)

`aw.yaml` holds Agent entrypoints, Providers and tool rules. Complete [installation and first run](installation.md) first; this page reuses `AW_HOME`, `AW_PREFIX` and `AW_QODER`.

## Choose capabilities

Add the appropriate options to `aw config init`. Install the selected Provider first.

| Goal | Options | Steps |
| --- | --- | --- |
| Launch an Agent | No Provider option | [Installation](installation.md) |
| Compress tool output | `--provider tokenless` | [Tokenless](tokenless.md) |
| Check tool commands | `--provider sec-core --socket ABS_SOCKET` | [Security](security.md) |
| Enable both | `--provider sec-core+tokenless --socket ABS_SOCKET` | Start the sec-core backend first |

Replace `ABS_SOCKET` with the backend's absolute socket path. The generator supports Qoder and OpenClaw; see [integration guides](../../../developer-guide/en/aw/native-agents.md) for SDK applications and other Agents.

## Edit configuration

Exit Agents using this configuration, then stop the service and make a backup.

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw config show --config "$AW_HOME/aw.yaml"
```

Edit the YAML directly. Use actual absolute paths; AW does not expand variables such as `$HOME`. Keep the file and parent directory owned by the current user and protected from other writers. `show` includes private configuration values; check before sharing.

Validate and restart after editing.

```bash
aw validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Validation should print `configuration valid`, and launch should open Qoder. Confirm the rule with an actual tool call. To roll back, exit the Agent, stop the service, restore the `.bak` file and restart.

## Select tools for a Hook step

Add `match` to an existing step in `tool.before.steps` or `tool.after.steps`. For example, a configuration declaring both Qoder and OpenClaw can select their Shell tools.

```yaml
match:
  tools: ["native:qoder:Bash", "native:openclaw:exec"]
```

Both event and step selectors must match. Omitting `match` selects all tools. Native names are case-sensitive; scanning another tool also requires its Provider [input mapping](../../../developer-guide/en/aw/providers/sec-core.md#configure-a-provider). See the [configuration reference](../../../developer-guide/en/aw/configuration.md) for selector syntax.

## Choose an Agent

`--agent` selects a target name in `spec.agents`. Qoder starts in the current working directory; arguments after `--` pass to the native program.

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

### OpenClaw

Use OpenClaw 2026.9.6 with an initialized profile. Stop the Gateway using that profile, then replace these placeholders with absolute paths to the existing installation and configuration.

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

Wait for `AW openclaw native hooks ready`, then use a client connected to that Gateway. `agent --local` bypasses it. Add the options above to `config init` to enable security or compression.

## Custom rules and service deployment

Custom rules can read tool events and return a JSON boolean; see the [Hook tutorial](../../../developer-guide/en/aw/providers/hooks.md).
Application deployers configure resident [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md), [external process attachment](../../../developer-guide/en/aw/deployment/process-attach.md), and [shutdown and concurrency](../../../developer-guide/en/aw/deployment/lifecycle.md).
