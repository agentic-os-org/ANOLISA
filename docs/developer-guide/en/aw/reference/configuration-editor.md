# Configuration editor

[中文版](../../../zh/aw/reference/configuration-editor.md)

The editor changes Providers, events and steps in an existing YAML/JSON file. Exit the Agent and stop the service first, or apply edits through [idle updates](../deployment/policy-updates.md) on supported instances.

## Commands

| Command | Behavior |
| --- | --- |
| `aw config show --config ABS_FILE` | Print the complete working configuration as pretty JSON |
| `aw config show --config ABS_FILE --provider NAME` | Print one Provider |
| `aw config show --config ABS_FILE --event EVENT [--id ID]` | Print one event, or a step with its event-scoped ID |
| `aw config add-provider --config ABS_FILE --name NAME --definition FILE` | Add a complete Provider definition |
| `aw config remove-provider --config ABS_FILE --name NAME` | Remove an unreferenced Provider |
| `aw config add-event --config ABS_FILE --event EVENT --definition FILE` | Add an explicitly configured event |
| `aw config add-hook --config ABS_FILE --event EVENT --definition FILE` | Append a complete step, whose `id` is in the definition |
| `aw config remove-hook --config ABS_FILE --event EVENT --id ID` | Remove one event-scoped step |

Use an absolute, symlink-free `--config` path owned by the current user. Neither file nor parent may be group/other writable. Hard links, special permissions and extended access ACLs are rejected; Linux procfs is required.
Definition files hold one complete YAML/JSON object, at most 4 MiB and 32 levels. `show` exposes private values.

Adding an identical definition or removing an absent object preserves the file; different definitions under the same name conflict. Remove referencing steps before their Provider. Each command publishes independently and atomically, rewriting comments and formatting; back up first.
`validate` checks fields and references and accepts relative paths. See the [editing contract](../../../../../src/aw/docs/design/en/configuration/configuration-editing.md) for file safety and concurrency.

## Add a rule to an empty configuration

Complete [installation](../../../../user-guide/en/aw/installation.md), reusing `AW_HOME`, `AW_PREFIX` and `AW_QODER`; Python 3 is also required.
Create a private demo directory and configuration, then add a boolean rule matching `AW_DENY_TEST`.

```bash
export AW_DEMO="$(mktemp -d "$AW_HOME/hook-demo.XXXXXX")"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_DEMO/aw-core.yaml" --state-dir "$AW_DEMO/state" --qoder "$AW_QODER"
```

```bash
cat > "$AW_DEMO/check-tool.py" <<'PYTHON'
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"], ensure_ascii=False)))
PYTHON
cat > "$AW_DEMO/provider.yaml" <<YAML
transport:
  type: stdio
  location: agent
  argv: [/usr/bin/python3, "$AW_DEMO/check-tool.py"]
timeout_ms: 1000
max_output_bytes: 32
YAML
cat > "$AW_DEMO/event.yaml" <<'YAML'
enabled: true
required: true
steps: []
YAML
cat > "$AW_DEMO/hook.yaml" <<'YAML'
id: check-marker
provider: company
effects: [block]
reason_code: demo_marker_denied
on_error: block
YAML
chmod 600 "$AW_DEMO/check-tool.py" "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
"$AW_PREFIX/bin/aw" config add-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company --definition "$AW_DEMO/provider.yaml"
"$AW_PREFIX/bin/aw" config add-event --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/event.yaml"
"$AW_PREFIX/bin/aw" config add-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/hook.yaml"
"$AW_PREFIX/bin/aw" config show --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREFIX/bin/aw" config validate --config "$AW_DEMO/aw-core.yaml"
```

After launch, ask the tool to print `AW_OK` and `AW_DENY_TEST`. The first should execute; the second should receive AW denial. Exit the Agent, stop the service and confirm process exit before removing this demo directory.

```bash
aw run --config "$AW_DEMO/aw-core.yaml" --agent qoder
aw stop --config "$AW_DEMO/aw-core.yaml"
rm -r -- "$AW_DEMO"
```
