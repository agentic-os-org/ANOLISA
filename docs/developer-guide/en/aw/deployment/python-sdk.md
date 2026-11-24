# Application-owned Python SDK

[中文版](../../../zh/aw/deployment/python-sdk.md)

`anolisa-aw-sdk` provides the framework-free asynchronous Linux client for
Python 3.11+, including `Instance`, `Task`, `Admission`, `TaskScope`,
Deployment Descriptor loading, cooperative control, and the language-level
Source Runtime (`aw_sdk.sources`). Framework-specific integrations ship as
separate optional wheels (`anolisa-aw-sdk-qwenpaw` and `anolisa-aw-sdk-claude`)
rather than being bundled into the core wheel. The application retains native
startup, configuration, permissions and scheduling. Install AW and Providers
separately and run the Service under the application user. These source-built
wheels are release candidates; registry publication follows the AW release
process.

Build AW from the same candidate revision as the language packages. Existing
releases without `aw-sdk/v1` reject connection; cooperative control additionally
requires `aw-sdk-control/v1`. These source-built packages do not upgrade AW.

For framework-free declarative instrumentation (`aw_sdk.sources`) — including
opt-in `replace_input`, `replace_result`, `withhold_result`, strict output
isolation, bounded stream buffering, and direct semantic callbacks (`task.emit`)
— see the full 6-package matrix and rules in
[Runtime SDK integration](native-sdk.md#framework-free-integration).
For wrapped tools that take class instances, that guide describes how to
provide `snapshot_arg` to preserve the checked input through stream iteration.

| Package | Role | Primary import | Descriptor profile |
| --- | --- | --- | --- |
| `anolisa-aw-sdk` (core) | Transport, `Instance`/`Task`/`Admission`/`TaskScope`, Descriptor loader, cooperative control, `LanguageSource` | `aw_sdk`, `aw_sdk.sources` | — (`adapter: sdk`) |
| `anolisa-aw-sdk-claude` (optional) | Resident Claude Agent SDK worker | `aw_sdk_claude` (`ResidentClaude`) | `claude-resident-hooks` |
| `anolisa-aw-sdk-qwenpaw` (optional) | QwenPaw native plugin & deferred after-only plugin | `aw_sdk_qwenpaw` (`QwenPawPlugin`, `AfterOnlyPlugin`) | `qwenpaw-installed-native-hooks` |

## Zero-adapter Python Agent (`aw_sdk.sources`)

Install only `anolisa-aw-sdk`, configure an `adapter: sdk` target named `app`
in `aw.yaml`, start `aw service serve --config /absolute/aw.yaml`, and export
its socket as `AW_SOCKET`:

```python
import asyncio
import os
from aw_sdk.sources import LanguageSource, SourceDenied


class CustomAgent:
    async def ask(self, prompt: str) -> str:
        return await self.echo(text=prompt)

    async def echo(self, *, text: str) -> str:
        return f"echo:{text}"


async def main() -> None:
    agent = CustomAgent()
    async with LanguageSource.create(
        endpoint=os.environ["AW_SOCKET"],
        target="app",
        availability="required",
    ) as source:
        async with source.session(session_id="example-python-agent") as session:
            session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"echo": "echo"},
            )
            try:
                print(await agent.ask("hello"))
            except SourceDenied as denied:
                print(f"AW denied {denied.event}")


asyncio.run(main())
```

For resident Claude Agent SDK workers using `anolisa-aw-sdk-claude`
(`ResidentClaude.from_config` with `--profile claude-resident-hooks` and
`aw connect --sdk-pid`), see the [Claude SDK guide](../agents/claude-sdk.md).

## Install and integrate

Install the core wheel (and only the optional framework adapter wheel your
application uses) in the application's environment:

```bash
python3 -m pip install /absolute/artifacts/anolisa_aw_sdk-0.2.1-py3-none-any.whl
# Optional framework adapters (install only when needed):
python3 -m pip install /absolute/artifacts/anolisa_aw_sdk_qwenpaw-0.2.1-py3-none-any.whl
python3 -m pip install /absolute/artifacts/anolisa_aw_sdk_claude-0.2.1-py3-none-any.whl
```

The top-level `aw_sdk` import loads no Agent framework and the core wheel ships
no framework-private adapter sources. `Instance` accepts an
absolute `endpoint`, configured `target`, native `capabilities`, explicit
`availability` and an `install(routes)` callback. It also accepts `cwd`,
`environment`, `config`, `timeout_ms` and `reconnect_ms`. Context defaults to the
application's directory and environment; use the same directory as the native
Agent. No binary or policy engine is included in the wheel.

For QwenPaw, install `anolisa-aw-sdk-qwenpaw` alongside `anolisa-aw-sdk`, copy
the installed example under
`share/anolisa-aw-sdk-qwenpaw/examples/qwenpaw/` into a new native plugin
directory, enable `plugins.aw-sdk`, and set `AW_CONFIG` to the absolute
deployment configuration path. Legacy `aw_sdk.qwenpaw` imports delegate to
`aw_sdk_qwenpaw` when installed and report an install hint when absent. Keep
`qwenpaw app`, the original model, permissions and other
plugins. The example reads the configuration through AW's authoritative offline
parser; initialization starts neither AW nor sec-core.

```python
from aw_sdk_qwenpaw import QwenPawPlugin

plugin = QwenPawPlugin.from_config("/absolute/aw.yaml")
```

To move configuration parsing into deployment, generate a
[Descriptor](native-sdk.md#deployment-descriptor) with the
`qwenpaw-installed-native-hooks` profile and pass its absolute path:

```python
plugin = QwenPawPlugin.from_config(
    "/absolute/aw.yaml", descriptor_file="/absolute/aw-qwenpaw.json"
)
```

This path validates the exact configuration bytes without invoking `aw`.
Descriptor errors fail initialization before native middleware registration.

The QwenPaw-specific entry also attempts the Adapter's optional plugin-isolation
path acceleration for verified QwenPaw `2.1.1b3`. It starts no services, preserves
native isolation and cleanup, and skips acceleration for other versions or an
unavailable seam. Importing the plugin installs it for subsequent loading;
plugins loaded earlier keep their original startup cost. An application with an
early Python startup entry can cover all plugin loading by calling:

```python
from aw_sdk_qwenpaw import prepare_runtime

accelerated = prepare_runtime()  # Before QwenPaw starts loading plugins.
```

The boolean reports whether acceleration is installed. Repeated calls are safe;
the patch lasts until process exit. This optional Adapter optimization changes
neither SDK capability admission nor the later cooperative PID attachment.
`aw run` uses the same implementation. Disable the AW plugin and remove an
explicit preparation call, then restart the App to restore the original resolver.

A single `qwenpaw` target is selected automatically; pass `target` for multiple
targets. Set an explicit private `daemon.endpoint` or `daemon.state_dir`.
The configuration selects the earliest protected boundary. With `input.submit`
enabled, each reply waits for Ready and its Scope before model dispatch. With only
`tool.before` and/or `tool.after`, model calls can overlap deployment preparation;
the first tool waits for Ready and fixes the Scope before native execution.
The internal admission gate does not execute an unconfigured before policy.
A tool-only reply without tools creates no remote Scope.

After HTTP readiness, prepare sec-core through deployment and run
`aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID`.
This cooperative selector needs the installed plugin; it does not inject into an
arbitrary PID. The configured resident path uses required availability, a default
5000 ms attachment wait, native before blocking and after result replacement or
withholding. Native generators, background work and final result publication
retain the existing resident guard. Keep one AW integration per App.
Adding `input.submit` to a deployment initially wired without it requires drain
and restart before reattachment; an in-flight model request cannot be checked
retroactively. Ordinary policy updates still use the existing idle update gate.

For explicit public-middleware control, `QwenPawPlugin(instance)` retains the
existing `Instance` API, optional availability and per-reply Task ownership. Its
`ready` callback confirms installed routes for the registration and each Scope.
The configured factory above additionally uses the resident result guard;
select the integration according to the required native effects.

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
`native_fallback` requires `required: false` on every enabled event applicable to the registered target. Missing or
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

Native Sources using required availability and the invocation ledger may call
`await task.watch_scope(probe=..., on_failure=...)` after activation. The SDK
renews the Service's validity lease during model and tool waits, latches bridge
failure for the owner, and stops/joins the monitor when the Task closes after
native drain. The configured QwenPaw plugin uses this common Task capability.

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

## Deferred tool-after attachment

The experimental `AfterOnlyPlugin` keeps native `qwenpaw app` startup independent
of AW and sec-core. Copy `share/anolisa-aw-sdk-qwenpaw/examples/qwenpaw-after/` from the
installed `anolisa-aw-sdk-qwenpaw` wheel into a new native plugin directory and enable `plugins.aw-after`.
Set `AW_SOCKET` to the private socket selected by your AW configuration. Use this
plugin alone for the AW integration; it replaces the optional `aw-sdk` plugin.
The application retains its native model, permissions and other Hooks.

Run the native command with the installed plugin. After HTTP readiness, start or
prewarm sec-core through its existing deployment entrypoint and attach from a
second terminal, using the actual application PID:

```bash
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID \
  --timeout-ms 10000
```

With `--sdk-pid` and a configuration using `daemon.startup: on_demand`, the CLI starts or reuses AW on demand, waits
for the application to publish its SDK support, and selects one exact PID/start
identity. Multiple SDK instances in that process require `--sdk-instance` instead.
The command confirms Ready; it never injects into an arbitrary process or launches
the Agent. A supplied `--socket` selects an existing service and does not start it.
The CLI does not start sec-core; its external lifecycle remains deployment-owned.

A prompt can enter the model before attachment. The installed result boundary
waits at the first tool, then opens a Scope, confirms an after-only snapshot and
reuses the resident invocation ledger, Coordinator withholding and reviewed
model/event publication. Only `tool.after` policy is accepted; the internal
`tool.before` observation route supplies correlation without a before policy
scan. `input.submit` and configured before checks are rejected. No-tool replies
can finish without AW. The Scope keeps one revision until native work drains.

The plugin uses `required` availability. `attach_timeout_ms` is 5000 by default
(100–60000); an early tool waits only for the remaining attachment preparation,
and a timeout refuses dispatch. Scanner errors withhold unchecked output while
the HTTP process stays alive. Unknown cleanup retains the resident failure fence.
There is no task fallback or tool replay. `reconnect_ms` defaults to 100 for this
plugin and controls SDK discovery polling. `cwd` optionally selects the instance
working directory; each reply passes its native workspace and clean Hook
environment to the task. `instance.task(cwd=..., environment=...)` also exposes
these copied per-task contexts for direct Python consumers.

To stop, let active work drain, detach the selected SDK instance, and stop AW and
sec-core through their own deployment lifecycle. Disable only `plugins.aw-after`
and restart the native App to remove its installed boundary. Disabling the plugin
removes this additional protection. Existing `aw run` behavior is unchanged.
Acceptance uses QwenPaw 2.2.2.post1 and AgentScope 2.0.10 with a local model and
replacement Provider fixture. Run the pinned `after-only-sdk` native suite for
late attachment, ordinary/replaced results, failed scanning, detached text and
bounded refusal without attachment. This does not establish a startup cost or
business latency guarantee; measure the native/installed-plugin difference,
backend preparation and first-tool residual wait separately.

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
