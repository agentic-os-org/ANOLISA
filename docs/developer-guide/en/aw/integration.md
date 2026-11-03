# Run a tool policy

[中文版](../../zh/aw/integration.md)

Use a boolean script to verify allow, block and failure behavior locally, then connect a real Agent.
Requires Linux, rustup, a C compiler and Python 3. The repository pins Rust; native acceptance additionally requires the selected Agent and model credentials.

## 1. Build and prepare configuration

Run from the repository root and retain these variables in the same terminal.

```bash
cd src/aw
cargo build --locked -p aw-cli -p aw-host --bins --examples
export AW_SOURCE="$PWD"
export AW_BIN="$AW_SOURCE/target/debug/aw"
export AW_HOST="$AW_SOURCE/target/debug/examples/host"
export AW_DEMO="$(mktemp -d "$AW_SOURCE/target/aw-onboarding.XXXXXX")"
export AW_CONFIG="$AW_DEMO/aw.yaml"
install -d -m 700 "$AW_DEMO/workspace" "$AW_DEMO/state"
cp "$AW_SOURCE/crates/aw-service/examples/aw.shared.yaml" "$AW_CONFIG"
cp "$AW_SOURCE/crates/aw-service/examples/check-tool.py" "$AW_DEMO/check-tool.py"
chmod 600 "$AW_CONFIG" "$AW_DEMO/check-tool.py"
printf 'AW_BIN=%s\nAW_CONFIG=%s\nAW_DEMO=%s\n' "$AW_BIN" "$AW_CONFIG" "$AW_DEMO"
```

Edit `$AW_CONFIG`, replacing placeholders with expanded absolute paths. AW does not perform shell interpolation in YAML.

| Field | Value |
| --- | --- |
| `spec.daemon.state_dir` | Printed AW_DEMO plus `/state` |
| `spec.providers.check-tool.transport.argv[0]` | Absolute Python 3 path |
| `spec.providers.check-tool.transport.argv[1]` | Printed AW_DEMO plus `/check-tool.py` |
| `spec.agents.<id>.argv` | Selected framework entrypoint before native acceptance; local Host checks can retain placeholders |

The example requests before denial for input containing `AW_DENY_TEST` and after observation. It is a wiring test rule.
Applications with installed core can instead use `aw config init` for Qoder/OpenClaw, then follow the [editing tutorial](reference/configuration-editor.md).

## 2. Verify the rule

```bash
"$AW_BIN" validate --config "$AW_CONFIG"
cd "$AW_DEMO/workspace"
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_OK"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_DENY_TEST"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.after demo_shell '{"command":"printf AW_OK"}'
```

Expect `candidate_effects: []`, `block/demo_marker_denied` and `observe/demo_tool_observed`, each with null `failure`.
Host sends synthetic events, reports `adoption: not_tested` and does not execute Shell commands from event data.

Copy the configuration to `$AW_DEMO/aw.failure.yaml`, change only the script path to a missing file, then repeat the first Host command with that copy.
Expect a nonzero exit, non-null failure and `failure_action: block`. Delete the copy after checking.

## 3. Connect a native Agent

Prepare the framework and profile through the [Agent index](native-agents.md). Reuse `AW_BIN`, `AW_CONFIG` and `AW_DEMO` in [native behavior validation](validation.md).
Check AW denial, tool markers and audit together; if the model refuses independently, choose a case that reaches the tool callback.

Choose a protocol when extending the rule.

| Form | Program implementation | Reference |
| --- | --- | --- |
| Boolean Hook | Read event, print JSON true/false, exit 0 | [Hook tutorial](providers/hooks.md) |
| Native Hook | Framework-specific input, output and exit status | [Hook tutorial](providers/hooks.md) |
| Structured Provider | `describe`, `validate_config`, `invoke` | [Protocol](providers/protocol.md) |

Security Providers also map actual tool names and input pointers; see [sec-core](providers/sec-core.md).

## 4. Clean up

Exit the Agent, stop its shared AW service and confirm owned processes have exited. Retain required evidence, then remove this run's directory.

```bash
cd "$AW_SOURCE"
rm -r -- "$AW_DEMO"
test ! -e "$AW_DEMO"
unset AW_CONFIG AW_DEMO AW_HOST AW_BIN AW_SOURCE
```

Keep framework profiles separately; follow [Hermes rollback](agents/hermes.md#upgrade-and-cleanup) for its plugin. Build artifacts remain under `src/aw/target/`.
See [contributing](../../../../src/aw/CONTRIBUTING.md) for checks after AW implementation changes.
