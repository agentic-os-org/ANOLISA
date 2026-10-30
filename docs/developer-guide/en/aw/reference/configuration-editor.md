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
| `aw config add-native-source --config ABS_FILE --agent TARGET --definition FILE` | Append an AW-owned source descriptor |
| `aw config remove-native-source --config ABS_FILE --agent TARGET --id ID` | Remove a descriptor while retaining its native source |
| `aw config native-sources --config ABS_FILE --agent TARGET` | Print redacted source categories and order, without loading sources |

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

## Native source references

`spec.agents.<target>.native_config.sources` records an ordered sequence of
references for an embedding Adapter. Each descriptor requires `id`, `kind`,
`scope` and `reference`; `base_dir` optionally retains the native relative-path
context. `kind` is `file`, `environment`, `argument` or `discovery`. References
hold paths or lookup keys; keep credential values in their native sources.
IDs are unique within a target and independent of native Hook/MCP/plugin names.

The editor never opens these references. It validates the strict AW descriptor
shape, appends without reordering, rejects conflicting IDs and publishes through
the existing atomic editor. Removing a descriptor does not uninstall a native
object. Removing the last descriptor drops the optional `native_config` section;
remove that section from every target before downgrading to an older AW build. The AW YAML file is rewritten on edits; original native files retain
bytes, comments and unknown fields. Native source descriptors are launch settings:
`validate --against` rejects changing them during an AW policy update. Native
file watchers and dynamic configuration stay owned by the native loader.

`native-sources` returns `native: declared_not_loaded` and `aw: not_admitted`.
Only source index and category are printed, including for custom scope labels.
`show` retains its explicit raw-value contract. Saving descriptors does not
change existing `aw run` source selection: built-in Adapters continue using their
documented native argv, environment and settings options. Applications consuming
this new API must wire their native loader as described below.

## Embedding a native loader

`aw_adapter::configuration::NativeConfiguration::new(&config, target)?.load(loader)`
passes the ordered references directly to the caller's native loader. Keep its
original cwd, environment, argv, authentication, permissions and options. The
loader owns parsing, default discovery, precedence, relative paths, Hook order
and dynamic updates; its output may be a live handle. Native errors propagate
unchanged. This API does not execute Hooks to discover capabilities or require
an exhaustive tool inventory.

The loader returns `NativeLoad { value, sources }`: the original value or live
handle and the sources actually consumed in native precedence order. Include
implicitly discovered sources; omit absent or ignored declarations. Runtime
source reports use this Adapter-owned evidence, while offline CLI reports show
declarations only.

The returned `LoadedConfiguration<T>` retains the native output and reports
`loaded` / `not_admitted`. Its `admit(capabilities, provider_evidence)` method
uses existing Provider admission. The immutable result retains the exact AW
configuration, target and admitted steps, including event/step selector
intersections. Reports say `admitted_not_active`; activation still requires the
service's route adoption and revision gate. Consumers must retain this result
with the corresponding runtime revision and keep it unchanged on callback,
Provider or bridge failures. Outside those steps, preserve native decisions;
a failed covered call must follow its admitted failure action.

The required `native_configuration` consumer suite runs the original loader
with and without AW, then executes actual Host policy calls. It covers unknown
fields, source order, live updates, native errors, Provider failures, editing
conflicts and redaction. This certifies the common API with a deterministic
framework fixture. Qoder SDK and QwenPaw business wiring and their native
configuration restrictions remain separate integration work.
