# Integrate a component with AW

[中文版](../../zh/aw/integration.md)

This walkthrough takes a component developer from a Linux source checkout to a
working tool policy, then to the same policy on Qoder, OpenClaw, QwenPaw and
Hermes. Start with a boolean command for a simple rule; implement a full Provider
when your component needs its own operations or structured effects.

AW is a Preview. `anolisa install aw` and RPM distribution are not available.
For prebuilt binaries, follow the [Preview package guide](../../../user-guide/en/aw/installation.md).
The source path below needs no sec-core installation or model account until you
choose the sec-core or native Agent steps.

## 1. Build from source

Use Linux with Git, rustup and a C compiler/linker. The repository pins Rust,
rustfmt and Clippy in `src/aw/rust-toolchain.toml`. Python 3 runs the optional
sample policy; Node.js and Python 3 are also required for AW's development gate.
Framework runtimes and model credentials remain the framework's responsibility.
Package builds have additional prerequisites in the Preview guide.

From a checkout of the desired reviewed commit:

```bash
cd src/aw
cargo build --locked -p aw-cli -p aw-service -p aw-package -p aw-provider -p aw-host --bins --examples
export AW_SOURCE="$PWD"
export AW_BIN="$AW_SOURCE/target/debug/aw"
export AW_HOST="$AW_SOURCE/target/debug/examples/host"
"$AW_BIN" --help
```

This builds debug binaries in `src/aw/target/debug`; it installs no system
service. For distributable release packages, use `aw-build` as described in the
[package build instructions](building.md).
Core builds independently of sec-core. Do not mix installed core and Provider
packages from different source commits or package versions.

## 2. Prepare one configuration for four frameworks

An initial `aw.yaml` must exist before any `aw config add-*` command.
For an installed core with Qoder/OpenClaw, use `aw config init` to create
it; omitting `--provider` creates an empty policy configuration. Follow the
[initialization and editing flow](packages.md#edit-existing-provider-and-hook-configuration)
for `aw config init` → `aw config add-provider` → `aw config add-event` → `aw config add-hook` → `aw config validate`.
`aw config init` verifies an installed package prefix; source binaries alone do not
supply it. It creates a new file and refuses to overwrite an existing one.

For this source walkthrough and for QwenPaw/Hermes, start from the complete
four-framework sample below. The generator currently supports Qoder/OpenClaw
only; the editor does not add Agent entries. The sample already declares both
tool events, so only an absent event needs `aw config add-event` before `aw config add-hook`.

Keep these variables in the same terminal. In another terminal, export the
printed absolute paths again; do not create another demo directory by accident.

```bash
export AW_DEMO="$(mktemp -d "$AW_SOURCE/target/aw-onboarding.XXXXXX")"
export AW_CONFIG="$AW_DEMO/aw.yaml"
install -d -m 700 "$AW_DEMO/workspace" "$AW_DEMO/state"
cp "$AW_SOURCE/crates/aw-service/examples/aw.shared.yaml" "$AW_CONFIG"
cp "$AW_SOURCE/crates/aw-service/examples/check-tool.py" "$AW_DEMO/check-tool.py"
chmod 600 "$AW_CONFIG" "$AW_DEMO/check-tool.py"
printf 'AW_BIN=%s\nAW_CONFIG=%s\nAW_DEMO=%s\n' "$AW_BIN" "$AW_CONFIG" "$AW_DEMO"
```

Edit the copied YAML, replacing the placeholders with the **expanded absolute
paths** below. AW does not interpolate `$AW_BIN`, `$HOME` or other shell variables
inside YAML. Quote paths that contain YAML punctuation.

| Field | Value for this walkthrough |
| --- | --- |
| `spec.daemon.state_dir` | The printed `AW_DEMO` path followed by `/state` |
| `providers.check-tool.transport.argv[0]` | An absolute Python 3 executable; keep `/usr/bin/python3` if available |
| `providers.check-tool.transport.argv[1]` | The printed `AW_DEMO` path followed by `/check-tool.py` |
| `spec.agents.<id>.argv` | The installed, supported framework executable and its entrypoint arguments |

The Agent entries may remain unchanged for the synthetic checks below: those
checks do not execute Agents. Before native launch, ensure the selected command
resolves to the [supported version](native-agents.md#current-support).
For Hermes use its installed Python console script, with an absolute Python
shebang. Keep the `chat`, `app` and `gateway run` arguments in the sample.

`endpoint: auto` uses `<state_dir>/aw.sock`. `agents` names launch targets;
`providers` names commands; `events` attaches ordered steps to tool boundaries.
All four targets share one boolean Hook command, with effects declared per
step. The sample requests a block when a before-tool input contains
`AW_DENY_TEST`, and an observation after a tool completes. It demonstrates wiring, not a production security rule.

The [configuration reference](configuration.md) explains all fields. The
generator `aw config init` currently creates Qoder/OpenClaw entries only;
copying this YAML is the path for a four-framework configuration. The
[configuration editor](packages.md#edit-existing-provider-and-hook-configuration)
can add/remove Providers and Hook steps; it does not edit Agent entries.

```bash
"$AW_BIN" validate --config "$AW_CONFIG"
"$AW_BIN" config show --config "$AW_CONFIG" --event tool.before
"$AW_BIN" config validate --config "$AW_CONFIG"
```

Validation checks document structure and references. It does not run the policy,
check credentials or certify that a framework adopted a block.

## 3. Verify the policy without a model

The existing Host example binds the `qoder` target with synthetic capabilities.
It runs the real boolean Hook command, sends normalized events and prints
candidate effects. The commands in the event data are never executed by this
example.

```bash
cd "$AW_DEMO/workspace"
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_OK"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_DENY_TEST"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.after demo_shell '{"command":"printf AW_OK"}'
```

Expect `candidate_effects: []`, then `block/demo_marker_denied`, then
`observe/demo_tool_observed`; each has `failure: null`. `adoption: not_tested`
is deliberate: these are Hook checks, not native tool execution or durable
service audit. The [local service demo](native-agents.md#run-the-local-service-demo)
separately demonstrates the socket, service client, audit query and shutdown
using its own supplied configuration, without a model.

For a failure check, copy the working configuration to `aw.failure.yaml` in
`AW_DEMO`, change only the `check-tool` script path to an absent file, and repeat
the first Host command with that file. Expect a nonzero exit, a non-null failure
and `failure_action: block`, rather than an empty successful result. A policy
block and a failed policy process are different outcomes. Remove the failure
copy after the check; do not modify an active service's configuration.

## 4. Connect your own component

Choose the smallest contract that fits your component:

| Integration | Implement | Configure |
| --- | --- | --- |
| Boolean command | Read one normalized event from stdin; print JSON `true`/`false`, exit 0; diagnostics on stderr | Omit `protocol` and `operation` for default `aw-hook/v1alpha1` / `invoke`; put `effects` and optional `reason_code` in each step, as in the shared sample |
| Existing native Hook | Follow the selected framework's raw input/output and exit-status contract | `native-hook/v1alpha1`, empty `config`, and `native: {}` in the step; native output is framework-specific |
| Structured Provider | Implement `describe`, `validate_config` and `invoke` over one JSON request/response per process | `aw-provider/v1alpha1`, your executable argv and private `config`; steps select `operation`, `effects` and `on_error` |

The shared script returns true for `tool.after`; its after step requests
`observe`, while the before step requests `block` only on a marker match. AW
admits both steps locally without calling the script's `describe` or
`validate_config`. The script receives only the normalized event, without a
method, request ID or digest, and needs no response object. Existing explicit
`aw policy` bridge configurations remain supported; new simple rules can use the
default Hook form shown here.
A false result imposes no extra restriction; it never grants native permission.
Nonzero exit, malformed output and timeout are execution failures, handled by
the step's `on_error`. Keep stdout free of logging.

The sample expects the normalized `tool.input` field; its value may be null.
AW validates the invocation shape before starting a script. If you test this
script directly with a before-tool JSON missing `tool.input`, it exits nonzero;
that malformed input is an error, not a false match. Do not hide missing fields
by substituting false.

For a full Provider, start with the existing
[Rust example](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-provider/examples/policy.rs), built in
step 1, and the [protocol reference](../../../../src/aw/docs/design/en/providers/provider-protocol.md).
These two calls exercise its discovery and private configuration contract:

```bash
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"demo-describe"}' | \
  timeout 5s "$AW_SOURCE/target/debug/examples/policy"
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"demo-config","config":{"blocked_tools":["delete_demo"]}}' | \
  timeout 5s "$AW_SOURCE/target/debug/examples/policy"
```

Each response must echo `api_version` and `request_id`. An `invoke` response also
echoes the request's `input_digest`; return only admitted effects. A successful
policy block exits 0. Implementing a new Provider does not require modifying AW
or importing your component's crates into it. Your executable and its runtime
dependencies must be installed where the Agent runs. AW starts a fresh process
per method; any long-lived backend belongs to your component.

Use the [sec-core walkthrough](../../../user-guide/en/aw/security.md)
for a concrete bridge to an existing CLI/daemon. Map the **actual tool name and
input JSON Pointer for each framework**; a standard event envelope does not
make every tool's arguments identical. The Preview-generated sec-core mapping
covers Qoder `Bash` and OpenClaw `exec`; extend and validate the mapping before
claiming QwenPaw or Hermes scanning coverage. Unmapped tools are observed without
being scanned.

## 5. Verify native adoption and clean up

Use the [shared-config launch and acceptance steps](native-agents.md#use-one-configuration-for-four-frameworks)
with `AW_BIN`, `AW_CONFIG` and `AW_DEMO` from this terminal. Keep the frameworks'
existing model profiles separate. Test one framework at a time initially, using
its supported entrypoint. A denied tool must have both an AW denial and no tool
side effect; a model refusal or native permission denial does not prove AW ran.

For component changes, run the appropriate checks from `AW_SOURCE`:

```bash
cd "$AW_SOURCE"
cargo test --locked -p aw-host -p aw-service -p aw-cli
```

Before submitting AW implementation changes, run the full Linux gate from the
repository root: `python3 src/aw/scripts/check.py`. This covers contracts and
fixtures; native model sessions remain a separate acceptance step. Record the
commit, framework versions, commands and observed results in the PR. Do not
label a configuration-only or synthetic check as four-framework acceptance.

After native sessions, follow the user guide's shutdown steps. If you ran only
the Host commands, no AW service was created. Once all processes using this
demo have exited, the configuration, copied script, markers and audit records
are all owned by this demo directory. Preserve needed audit evidence, then:

```bash
cd "$AW_SOURCE"
rm -r -- "$AW_DEMO"
test ! -e "$AW_DEMO"
unset AW_CONFIG AW_DEMO AW_HOST AW_BIN AW_SOURCE
```

The framework installations and profiles remain untouched by this cleanup;
Hermes plugin installation has a separate rollback procedure in the user guide.
Cargo build outputs remain under `src/aw/target/`; use `cargo clean` from
`src/aw` only when you want to discard that component's build cache.
