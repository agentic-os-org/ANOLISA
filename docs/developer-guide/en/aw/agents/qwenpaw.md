# QwenPaw

[中文版](../../../zh/aw/agents/qwenpaw.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start QwenPaw

Install QwenPaw 2.2.2.post1 with AgentScope 2.0.10 and initialize its model configuration
through QwenPaw. The example uses `argv: [qwenpaw, app]`. Set
`QWENPAW_WORKING_DIR` to the existing initialized absolute directory. AW adds only
its owned temporary plugin, preserves existing plugins and native configuration,
and removes its plugin after the App exits.

Ordinary launches verify resident middleware on each constructed Agent and open
an acknowledged task scope. Before Hooks run forward; after Hooks unwind in
reverse. The host controls native tool concurrency. Offloaded tools keep their
scope until native generators, Hooks and background completion callbacks finish.
Background HTTP output and next-request hints become visible only after all required
native callbacks succeed. Denials retain policy text and Tokenless retry arguments.
Completed invocations retire their state, allowing long sessions to continue
within the bounded active-call limit.

After Hooks receive redacted terminal `ToolResponse` objects, including AW-denied
results. The application credential extension supplies one private execution
lease per Agent reply, shared by that reply's tools, and owns its cleanup; see
[Private execution leases](../../../../../src/aw/docs/design/en/adapters/private-execution.md).
Redactors must be idempotent across repeated delivery/Hook checks.
Authorization-bearing arguments remain unchanged. `on_error: report`
preserves callback error reporting; before `block` denies execution. For structured
Providers and boolean Hooks, after `withhold_result` prevents unverified result
delivery and later dispatch. This action is admitted only on ordinary resident
launches. Strict after processing withholds streaming chunks for matching tools.
Task-private execution also withholds intermediate chunks until terminal redaction.
Native Hook or lifecycle failures fence the Agent even when AW steps use `report`;
restart the failed Agent after correcting the cause. Cleanup timeout or uncertain finish/release acknowledgement fences
new requests process-wide and requires a worker restart. Credentials remain owned
until native work is confirmed drained. Shutdown shares one ten-second budget
with nested cleanup, including cancellation. Event budgets must be
1..55,000 ms;
ask/approve directives remain unsupported. Native permissions run before these
callbacks; external execution and earlier permission denials have separate native
paths.

The startup receipt confirms plugin loading; each Agent additionally passes the
actual installation guard before its request starts. Use App/API after the native
Hooks ready message. ACP/TUI and multi-worker launches remain rejected.
`--reloadable` retains its separate request-idle update contract. Ordinary
launches provide resident child scopes, long-session retirement and explicit
updates between fully drained replies. A reused Agent reads the active policy
for each new reply after reservation. App startup confirms update readiness before
the first reply; startup policy remains active until an update succeeds.
Native evidence capture is rejected for ordinary resident launches because that
child-scope observation contract is unavailable.

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

The [neutral QwenPaw example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
runs commands before and after tools; it installs no security policy. Use the
same `aw-provider/v1alpha1` policy configuration across supported adapters.
The native Hook output dialect and event coverage remain framework-specific.

An ordinary configuration with no Providers or event steps still installs
resident admission and cleanup middleware.
