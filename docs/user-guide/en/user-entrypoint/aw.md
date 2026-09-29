# AW user guide

[中文版](../../zh/user-entrypoint/aw.md)

AW is being built to let you use one policy configuration across different
Agents. You keep using the Agent's own interface, while AW connects its tool
Hooks to the rules and processing programs you choose. The first release targets
QwenPaw, Qoder CLI, OpenClaw and Hermes.

The goal is to distribute AW with an `aw.yaml` file, reuse that policy when
switching Agents, and keep deployment status and audit records in one service.
The current version lets you prepare and check the configuration. Agent startup
and policy execution are still being built.

## Available today

✅ means available in this version. ❌ means planned and not yet available through
this configuration. Earlier experiments do not establish support in this version.

| What you want to do | Status | What to expect |
| --- | --- | --- |
| Start from a configuration template | ✅ Available | Starter and full examples are included |
| Check field names, types and Provider references | ✅ Available | The offline checker reports configuration errors |
| Declare any of the 16 event names | ✅ Available | Recognizing a name does not connect its native Hook |
| Start or attach an Agent through AW | ❌ Planned | The daemon and product CLI are not available yet |
| Run Providers before and after native tools | ❌ Planned | Each framework needs its adapter and effect validation |
| Apply sec-core rules to block tools or redact results | ❌ Planned | Requires Provider execution and proof that the Agent uses the response |
| View applied policy and persistent audit records | ❌ Planned | The service will maintain these records |
| Install AW and generate a default configuration | ❌ Planned | The starter file is copied manually today |
| Request user approval or enforce policy below native Hooks | ❌ Later work | Active `ask` steps are currently rejected; OS enforcement is not provided |

All four first-release Agent IDs are accepted in configuration. Runtime
integration remains ❌ for each in this version. QwenPaw is a separate target
from Qwen Code. Runtime support will be documented by Agent version and operation
as adapters are delivered.

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
future runtime admission must reject a target that cannot provide them.

The remaining settings choose local service defaults and request metadata-only
auditing. The event budget is 5,000 milliseconds. These are explicit values in
the template; the checker does not start a service, write audits or enforce a
timer. It does not search for a default file or fill missing fields into yours.

## Check the file now

The checker currently runs from a source checkout. With rustup installed, enter
`src/aw` to select the pinned Rust toolchain and check the starter file. Run the
commands below from the repository root. Replace the final path with your own
`aw.yaml` when ready; relative paths are resolved from `src/aw`.

```bash
cd src/aw
cargo run --locked -p aw-config --example validate -- \
  crates/aw-config/examples/aw.minimal.yaml
```

A successful check prints the following message.

```text
Configuration is statically valid; runtime admission has not run.
```

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
check refers to `security`. Steps run in their configured order once the execution
service is available. Provider invocation remains ❌ in the current version.

The full example shows all 16 event names and a disabled result-redaction step.
Its business executable and sec-core command are illustrative. Replace them with
real implementations when the Provider protocol and runtime are delivered.
Changing `enabled` today changes the configuration being checked, without
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
