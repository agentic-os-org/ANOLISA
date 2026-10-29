# OpenClaw

[中文版](../../../zh/aw/agents/openclaw.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start OpenClaw

Install OpenClaw 2026.9.6 separately. The example uses `argv: [openclaw, gateway, run]`;
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
