# AW user guide

[中文版](../../zh/user-entrypoint/aw.md)

AW is being built to let you use one policy configuration across different
Agents. You keep using the Agent's own interface, while AW connects its tool
Hooks to the rules and processing programs you choose. The first release targets
QwenPaw, Qoder CLI, OpenClaw and Hermes.

The goal is to distribute AW with an `aw.yaml` file, reuse that policy when
switching Agents, and keep deployment status and audit records in one service.
The current version provides a standalone Linux service, a local client and
persistent execution records from a source build. Agent startup and native policy
integration are still being built.

## Available today

✅ means available in this version. ❌ means planned and not yet available through
this configuration. Earlier experiments do not establish support in this version.

| What you want to do | Status | What to expect |
| --- | --- | --- |
| Start from a configuration template | ✅ Available | Starter and full examples are included |
| Check field names, types and Provider references | ✅ Available | The offline checker reports configuration errors |
| Declare any of the 16 event names | ✅ Available | Recognizing a name does not connect its native Hook |
| Try a local Provider with synthetic tool events | ✅ Source example | The service prepares and invokes Providers; no Agent is launched |
| Run AW independently of a shell or Agent | ✅ Source build | Start the foreground service and use its local client |
| Start or attach an Agent through AW | ❌ Planned | Native adapters and `aw run` are not implemented |
| Run Providers before and after native tools | ❌ Planned | Each framework needs its adapter and effect validation |
| Apply sec-core rules to block tools or redact results | ❌ Planned | Requires a sec-core Provider, supported effects and proof that the Agent uses the response |
| Query persistent Provider execution records | ✅ Available | Read preparation and event metadata through the local service |
| Verify that a native Agent applied a policy | ❌ Planned | A running service and successful Provider call do not prove adoption |
| Install AW and generate a default configuration | ❌ Planned | The starter file is copied manually today |
| Request user approval or enforce policy below native Hooks | ❌ Later work | Active `ask` steps are currently rejected; OS enforcement is not provided |

All four first-release Agent IDs are accepted in configuration. Runtime
integration remains ❌ for each in this version. QwenPaw is a separate target
from Qwen Code. Runtime support will be documented by Agent version and operation
as adapters are delivered.

## Run the local service demo

AW is not yet published through `anolisa install` or an RPM. Developers can build
it on Linux with rustup; the checkout selects its pinned Rust toolchain. From the
repository root, build the CLI and sample Provider, then start the foreground
service:

```bash
cd src/aw
cargo build --locked -p aw-provider --example policy
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-service/examples/aw.yaml
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

Use a second terminal in the same checkout's `src/aw` directory. Replace
`AW_DEMO_SOCKET` with the absolute socket path printed in the first terminal:

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- \
  "$AW_DEMO_SOCKET"
```

The example supplies synthetic Qoder capabilities and tool events. It prepares
the sample policy, checks an unrestricted `read_demo` and a blocked `delete_demo`
before-tool event, then observes an after-tool event. It prints results and audit
keys. These are local Provider outcomes; Qoder is not started and no native tool
runs. The sample policy is not sec-core.

The [demo configuration](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.yaml)
uses `./target/debug/examples/policy`. Keep the example's working directory at
`src/aw`; Provider paths resolve against the context supplied by the client.
The starter template later in this guide contains no Provider and therefore
cannot produce this demonstration's effects.

## Commands and records

| Command | Purpose |
| --- | --- |
| `aw validate --config FILE` | Check syntax and static references without executing commands |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | Run one immutable configuration in the foreground |
| `aw status --socket ABSOLUTE_PATH` | Inspect service identity, resource counts and audit health |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | Send one operation JSON object from stdin; default timeout is 5,000 ms |
| `aw stop --socket ABSOLUTE_PATH` | Request cancellation and graceful shutdown |

In a source checkout, use `target/debug/aw` for `aw`. Successful checks print
`configuration valid`; control commands print JSON. Errors return nonzero.
`request` is a developer interface for binding, event and audit operations. Its
input is an operation object, not a complete protocol envelope; the client adds
service identity and a deadline. See the [local service contract](../../../../src/aw/docs/design/local-service.md#local-protocol)
for all operation fields.

Replace `AUDIT_KEY` below with a preparation key or event ID returned by the
example. Recorded preparation failures also report an audit key.

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
```

The result contains verified records and `terminal`. Records keep execution
metadata, without tool input/results, private Provider configuration or raw
stdout/stderr. `terminal: false` can mean still active or interrupted; it does
not prove a crash. A terminal result does not prove that an Agent used the policy.
If a call times out, query its known audit key rather than retrying the step.

## Stop and restart

```bash
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

Wait for the foreground `serve` command to exit: the stop reply only acknowledges
the request. Shutdown cancels unfinished calls and removes the owned socket.
The state directory retains `service.lock` and `journal/` for later inspection.

The absolute state directory needs an existing parent owned by your user and
not writable by its group or others. The example creates that private parent
with `mktemp`; it does not change permissions on an existing build directory. AW
creates it with mode 0700, or requires that mode if it already exists. Explicit
`spec.daemon.state_dir` and `endpoint` values must match the selected directory
and its `aw.sock`; `auto` leaves that choice to the caller. On-demand startup and
supervisor installation remain unavailable.

After a forced kill, AW refuses to overwrite a leftover socket. First confirm that
the previous service process has exited; then remove only that demo's socket:

```bash
rm -- "$AW_DEMO_SOCKET"
```

Keep the lock and journal. Retain the first terminal's `AW_DEMO_ROOT` value and
restart with the same `serve` command to retain audit
history. Each restart has a new service identity: old clients, bindings and event
handles cannot be reused. Previous records can be queried, but execution is never
resumed or replayed automatically. After all service processes have exited, remove only this run's demo directory
from the first terminal if you no longer need its audit history:

```bash
rm -r -- "$AW_DEMO_ROOT"
```

## Start with a small configuration

Copy the [starter file](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.minimal.yaml)
to your chosen `aw.yaml` location. It declares Qoder and requests tool-before and
tool-after events. It contains no policy program and enables no security rule.

```yaml
# Starter configuration for offline validation.
# No policy program is configured; runtime integration is still being built.
apiVersion: aw/v1alpha1
kind: AWConfiguration
metadata:
  name: local-agent
spec:
  daemon:
    startup: on_demand
    endpoint: auto
    state_dir: auto
  execution:
    guarantee: native_hook
    default_event_budget_ms: 5000
  audit:
    enabled: true
    payload: metadata_only
  agents:
    qoder:
      adapter: qoder
      argv: [qodercli]
  providers: {}
  events:
    tool.before:
      enabled: true
      required: true
      steps: []
    tool.after:
      enabled: true
      required: true
      steps: []
```

The outer fields should look familiar if you use Kubernetes. `apiVersion` selects
the file format, `kind` identifies an AW configuration, and `metadata.name` names
it. `spec` holds what you want AW to use. AW is designed to run independently of
Kubernetes; no cluster or CRD is needed to check this file.

Inside `agents`, `qoder` is a name you choose for this target. `adapter` selects
the framework, and `argv` gives its executable and arguments. Add another named
Agent to share the same Provider definitions and event routes. The full example
includes Qoder and OpenClaw; QwenPaw and Hermes launch details will be verified
with their adapters.

The empty `providers` object leaves policy programs unconfigured. Empty `steps`
lists make no Provider calls. Both events set `required: true`, declaring that
runtime admission must reject a target whose supplied capabilities cannot provide them.

The remaining settings choose local service defaults and request metadata-only
auditing. The event budget is 5,000 milliseconds. These are explicit values in
the template; the checker does not start a service, write audits or enforce a
timer. It does not search for a default file or fill missing fields into yours.

## Check the starter file

After building the CLI above, run from `src/aw`. Replace the final path with your
own `aw.yaml` when ready; relative paths are resolved from this directory.

```bash
target/debug/aw validate --config crates/aw-config/examples/aw.minimal.yaml
```

A successful check prints `configuration valid`.

This confirms the field structure and static relationships. The checker does
not require Qoder to be installed and does not run any configured command.
Before policies can take effect, the service must also verify the installed
Agent and the Provider's actual capabilities.

## Add your policy programs

A Provider is a program that checks or processes an event, such as a security
engine or your team's tool-result handler. In `spec.providers`, give each instance
a name, specify its command and put its own settings in `config`.

An event step refers to that name through `provider` and selects an `operation`.
In the [full example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.yaml),
`business-before` refers to the `business` Provider, while the final tool-before
check refers to `security`. Native step scheduling remains part of the future
Agent integration. Local invocation is available through the service demo;
running Providers around real Agent tools remains ❌ in the current version.

The full example shows all 16 event names and a disabled result-redaction step.
Its business executable and sec-core command are illustrative. Replace them with
real implementations when integrating with an Agent. The full example includes
capabilities outside the current Host's supported tool events and is not its
runnable template. Changing `enabled` changes the configuration being checked, without
installing a Hook or activating protection.

## Use the configuration with an Agent

The planned workflow starts with AW reading your file and checking that the
chosen Agent can carry out the requested actions. AW then installs its own native
Hook or plugin entries and opens the Agent's normal interface. Provider rules run
at those supported points; the AW service records deployment state and outcomes.

For the Qoder and OpenClaw targets in the full example, the intended commands are
shown below. They remain ❌ planned commands and cannot be run in this version.

```bash
aw run qoder --config ./aw.yaml
aw run openclaw --config ./aw.yaml
```

A required safety action that the Agent cannot enforce must prevent binding.
Optional observation gaps must be visible. The service is intended to stay
running after an Agent session ends, so another session can reuse its
configuration and records.

For field limits, omitted-field behavior and the full event vocabulary, use the
[configuration reference](../../../developer-guide/en/aw/configuration.md).
