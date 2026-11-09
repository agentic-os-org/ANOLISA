# OpenClaw

[中文版](../../../zh/aw/agents/openclaw.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start OpenClaw

Install OpenClaw separately; 2026.9.6 is the verified baseline. AW records the
installed version and requires native plugin registration and Gateway readiness
for other releases. The example uses `argv: [openclaw, gateway, run]`;
set an absolute executable path if needed. Supply the existing native JSON
configuration and an existing absolute state directory. AW writes a private
configuration overlay and retains that directory's authentication and sessions.
It refuses conflicting Gateway ownership and does not attach to a running one.
AW disables Node compile caching for the owned Gateway to prevent launcher
respawns from changing its readiness PID; custom wrappers must exec the Gateway.

Each AW step becomes one native plugin handler. OpenClaw runs before handlers
serially by native priority and after handlers concurrently; other plugins retain
their priorities. Event budgets must be 1..12,000 ms. Blocked attempts can still
produce after callbacks with an error, so after does not imply successful execution.
The supported path is Agent execution inside the new Gateway; operator
`tools.invoke` is not a complete before/after entrypoint. AW reports native Hooks
ready after the Gateway startup callback, separately from service availability.

```bash
target/debug/aw run --config crates/aw-service/examples/aw.openclaw.yaml --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

The [neutral OpenClaw example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.openclaw.yaml)
runs commands before and after tools; it installs no security policy. Use the
same `aw-provider/v1alpha1` policy configuration across supported adapters.
The native Hook output dialect and event coverage remain framework-specific.

## Prepare native startup and update policy

For the verified OpenClaw 2026.9.6 foreground Gateway, install the startup bridge
once in a dedicated directory. Use a dedicated shell for its environment, and
restart the Gateway once. Node must satisfy the installed OpenClaw release's
runtime requirements. Keep the same Gateway arguments in `aw.yaml` and in the
native command.

```bash
aw instance setup --config /absolute/aw.yaml --agent openclaw \
  --directory /absolute/aw-openclaw-startup \
  --native-settings /absolute/openclaw.json \
  --native-state-dir /absolute/openclaw-state > openclaw-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("openclaw-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
export NODE_OPTIONS="$(python3 -c 'import json; print(json.load(open("openclaw-startup.json"))["environment"]["NODE_OPTIONS"])')"
openclaw gateway run
```

The preload prepares the binding before native startup; the plugin confirms
readiness at `gateway_start`. The private overlay grants only the AW plugin
conversation-hook access so it can reserve each native run. The bridge retains
session/run identities and policy metadata; it does not export the conversation
received by that lifecycle hook. The user's native configuration stays intact.

From another terminal, update an idle instance using the same configuration:

```bash
aw instance update --config /absolute/aw.yaml --agent openclaw
```

The policy stays fixed from `before_agent_run` through completed tool callbacks
and successful `agent_end`. Both tool events support adding or removing steps
between runs, including an initially empty policy. Missing completion, native
cancellation, duplicate active run identities and uncertain cleanup keep updates busy;
restart that Gateway before resuming management. Tool calls without admitted
runs are blocked. Native `tools.invoke` remains outside this managed entrypoint.

The bridge retains completed identities to reject delayed callbacks and run-ID
reuse. Each binding admits at most 4,096 distinct runs, including active and
completed runs. At that limit, new runs are blocked; restart the Gateway to
create a new binding before continuing.

The stable bridge executes before steps sequentially and ordinary after
observers concurrently. A replacement policy executes its final replacement
step after its preceding observations. Native completion after a denied tool is
still audited. Event budgets remain at most 12,000 ms.

To stop using this startup bridge, exit the Gateway and the dedicated shell,
then remove the files listed by `setup` from the selected startup directory.
A subsequent native launch uses the original configuration. Existing Gateway
processes need their own restart before this preparation takes effect.
