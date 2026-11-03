# Configuration and launch

[中文版](../../zh/aw/configuration.md)

`aw.yaml` holds Agent entrypoints, Providers and tool rules. Complete [installation and first run](installation.md) first; this page reuses `AW_HOME`, `AW_PREFIX` and `AW_QODER`.

## Choose capabilities

Install the selected Provider first. Set `AW_TEMPLATE_DIR` to its unpacked
archive's absolute `templates/` directory, then choose a template for `aw config init`.

| Goal | Options | Steps |
| --- | --- | --- |
| Launch an Agent | No template | [Installation](installation.md) |
| Compress tool output | `--template "$AW_TEMPLATE_DIR/tokenless.json"` | [Tokenless](tokenless.md) |
| Check tool commands | `--template "$AW_TEMPLATE_DIR/sec-core.json"` | [Security](security.md) |
| Enable both | `--template "$AW_TEMPLATE_DIR/sec-core-tokenless.json"` | [Tokenless](tokenless.md) and [security](security.md) |

For Provider configuration, use its actual installation prefix in `--prefix`.
Supply the backend paths, socket and RTK checksum using the `--template-var`
arguments shown in those guides. `PROVIDERS_DIR` selects the installed Provider
programs, independently of the AW CLI prefix. `--provider` accepts only `none`
or `command`. The generator supports Qoder and OpenClaw; see [integration guides](../../../developer-guide/en/aw/native-agents.md) for SDK applications and other Agents.

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

Use OpenClaw with an initialized profile; 2026.9.6 is the verified baseline. Other releases must provide the same native plugin interfaces. Stop the Gateway using that profile, then replace these placeholders with absolute paths to the existing installation and configuration.

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

Wait for `AW openclaw native hooks ready`, then use a client connected to that Gateway. `agent --local` bypasses it. Select the corresponding template above to enable security or compression.

### QwenPaw

Use the existing initialized `QWENPAW_WORKING_DIR` with an App/API target from
the [QwenPaw integration guide](../../../developer-guide/en/aw/agents/qwenpaw.md).
Each AW launch installs a separate temporary plugin and preserves existing
plugin names and settings. QwenPaw keeps ownership of `config.json`, plugin
configuration and dynamic MCP updates. Select covered tools explicitly; native
permissions continue to apply to tools outside those selectors.

## Custom rules and service deployment

Custom rules can read tool events and return a JSON boolean; see the [Hook tutorial](../../../developer-guide/en/aw/providers/hooks.md).
Application deployers configure resident [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md), [external process attachment](../../../developer-guide/en/aw/deployment/process-attach.md), and [shutdown and concurrency](../../../developer-guide/en/aw/deployment/lifecycle.md).

## Record native configuration sources

For applications integrating the shared native-loader API, AW can record source
references alongside its policy. Keep native configuration in its original
files. The built-in `aw run` Adapters still select sources through their existing
options; a source declaration alone does not change loading or activate coverage.

With the existing private `AW_HOME` and `qoder` target from installation, record
an already selected native project source:

```bash
cat > "$AW_HOME/native-source.json" <<'JSON'
{"id":"project","kind":"file","scope":"project","reference":"./native.yaml"}
JSON
chmod 600 "$AW_HOME/native-source.json"
aw config add-native-source --config "$AW_HOME/aw.yaml" --agent qoder \
  --definition "$AW_HOME/native-source.json"
aw config native-sources --config "$AW_HOME/aw.yaml" --agent qoder
```

Expect `declared_not_loaded` / `not_admitted` and the source's index and category.
No native contents or credentials are printed. The Adapter owns relative-path
resolution and actual source precedence. To remove the reference, run:

```bash
aw config remove-native-source --config "$AW_HOME/aw.yaml" --agent qoder --id project
```

The original native file is retained. See the [editor and loader reference](../../../developer-guide/en/aw/reference/configuration-editor.md#native-source-references)
for source fields, atomic editing, load/admission states and current integration scope.
