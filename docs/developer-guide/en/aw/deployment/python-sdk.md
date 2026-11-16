# Application-owned Python SDK

[中文版](../../../zh/aw/deployment/python-sdk.md)

`anolisa-aw-sdk` provides an asynchronous Linux client for Python 3.11+ and an
optional QwenPaw plugin. The application retains native startup, configuration,
permissions and scheduling. Install AW and Providers separately and run the
Service under the application user. This source-built wheel is a release
candidate; registry publication follows the AW release process.

Build AW from the same candidate revision as the language package. Existing
releases without `aw-sdk/v1` reject connection; cooperative control additionally
requires `aw-sdk-control/v1`. These source-built packages do not upgrade AW.

The client also accepts framework-free semantic callbacks through `task.emit`;
see [Runtime SDK integration](native-sdk.md#framework-free-integration).

## Install and integrate

Install the wheel in the application's environment:

```bash
python3 -m pip install /absolute/artifacts/anolisa_aw_sdk-0.1.1-py3-none-any.whl
```

The top-level `aw_sdk` import loads no Agent framework. `Instance` accepts an
absolute `endpoint`, configured `target`, native `capabilities`, explicit
`availability` and an `install(routes)` callback. It also accepts `cwd`,
`environment`, `config`, `timeout_ms` and `reconnect_ms`. Context defaults to the
application's directory and environment; use the same directory as the native
Agent. No binary or policy engine is included in the wheel.

For QwenPaw, copy the installed example under
`share/anolisa-aw-sdk/examples/qwenpaw/` into a new native plugin directory and
enable `plugins.aw-sdk` in the application's own configuration. Set `AW_SOCKET`
to the independent Service's private socket and configure the target named
`qwenpaw` with Adapter `qwenpaw`. Keep the original model, plugins, permissions
and configuration sources. Native plugin loading installs the middleware before
its startup hook publishes cooperative support and returns without waiting for AW
registration. Shutdown releases this plugin's instance.
Use one AW integration per App to avoid duplicate policy callbacks.

The example's `install` callback delegates to `QwenPawPlugin.ready`, which
confirms registered middleware and validates the supplied routes. A custom
integration must likewise return `True` only after installing its callbacks.
The client repeats this confirmation for each task's immutable policy snapshot.
Each Agent reply owns a task, and that Scope remains until its native tool
generators also finish. Separate Agent instances retain independent tasks.

## Async lifecycle and availability

Call `await instance.connect()` for one connection attempt, or `instance.start()`
for one background reconnect loop. `state` and `last_error` expose readiness and
known failures. `await instance.stop()` stops reconnection;
`await instance.close()` releases its tasks and registration. Neither call owns
the native App or shared Service. For direct integrations, obtain a task with
`await instance.task()`, invoke a native route with
`await task.invoke_native(hook, payload)`, and close it in `finally` or with
`async with`. The result is `(code, stdout_bytes, stderr_bytes)`.

The explicit modes and policy constraints match the
[TypeScript SDK](native-sdk.md#availability-and-task-ownership).
`native_fallback` requires `required: false` on every enabled event. Missing or
lost transport fixes current tasks in fallback after the first bounded check;
subsequent callbacks return neutral immediately. Recovery applies to new tasks.
Lifecycle RPCs default to 1000 ms. Callback RPCs allow at least the event budget
plus 1000 ms transport time; the Service retains the original event deadline. There is no replay of uncertain callbacks or native tools.

`asyncio.CancelledError` remains cancellation. Configuration/protocol errors,
Provider failures and explicit denials never become transport fallback. Native
before denial prevents tool entry. Post-processing errors propagate through the
native task; this public middleware does not implement the resident Coordinator's
strict streaming-result withholding or managed MCP/credential seams.
Other middleware that changes inputs after AW is outside the final-input guarantee.

`required` transport loss retires the connection and fails its existing tasks;
background reconnection continues, and only new tasks use the recovered registration.
While the Service remains reachable, cancelled or failed callbacks await event
drain while preserving the original exception. To stop between native steps, call
`await task.close_event(hook, original_payload)` before continuing with another
event in the same Task. Finish the Task after native generators have drained.

## Cooperative control

Call `instance.listen(automatic=False)` after installing native callbacks to
publish an initially detached application, or pass `automatic=True` for automatic
attachment and CLI control. The QwenPaw plugin defaults to explicit attachment. Start `qwenpaw app` normally,
select its discovery ID and attach using the linked commands. Set
`QwenPawPlugin(instance, automatic=True)` to wait for initial connection during
startup and reconnect automatically.
`application_id` exposes the discovery ID. The commands, request deadlines,
idle detach and restart rules match [cooperative control](native-sdk.md#cooperative-attach-and-detach).
Use `await instance.stop()` to withdraw the listener, and `await instance.close()`
to release application-owned resources.

## Verified coverage

The default gate builds and installs a wheel outside the repository, checks
framework-free imports and runs shared lifecycle, transport and cooperative-control tests. Real App
acceptance used QwenPaw 2.2.2.post1 / AgentScope 2.0.10 with a controlled model,
original configuration and a native observer plugin. It checks absent/online
Service, loss, recovery, explicit AW denial, public native-event records and
exactly-once shell side effects. Other versions negotiate their capabilities;
this acceptance result remains specific to the versions tested.

Native Skill, result/input rewriting, background offload and managed MCP
combinations require separate acceptance before business rollout. The existing
required entrypoint and its wider integration are documented in the
[QwenPaw guide](../agents/qwenpaw.md).
