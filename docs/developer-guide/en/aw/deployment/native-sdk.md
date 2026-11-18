# Application-owned Runtime SDK

[中文版](../../../zh/aw/deployment/native-sdk.md)

Use `@anolisa/aw-sdk` or the asynchronous `anolisa-aw-sdk` Python package when
an application owns startup and needs shared AW policy and lifecycle management.
Both clients support framework-free semantic callbacks. Optional Qoder Hooks and
QwenPaw Middleware remain thin native integrations. Linux is required; the Node
package needs Node.js 22+. Install AW and Providers separately. These source-built
candidate packages await publication through the AW release process.

Build AW from the same candidate revision as the language package. Existing
releases without `aw-sdk/v1` reject connection; cooperative control additionally
requires `aw-sdk-control/v1`. These source-built packages do not upgrade AW.

## Configuration-driven Qoder integration

Use `createQoderBridge` to retain the native Node entrypoint, daemon, authentication,
model settings, permissions and Hook groups. The bridge reads the same owned
`aw.yaml` used by deployment, installs public Qoder Hooks and publishes cooperative
attachment support. A deployment-generated Descriptor lets initialization read
validated wiring without starting a configuration-parser subprocess. Without
`descriptorFile`, AW's offline `config show` parser reads YAML or JSON once through
a bounded CLI invocation. Neither path starts AW or sec-core.

```javascript
import { createQoderBridge } from "@anolisa/aw-sdk/qoder";
import { query } from "@qoder-ai/qoder-agent-sdk";

const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  query,
});
const session = aw.query({ prompt, options: nativeOptions });
for await (const message of session) deliver(message);
// At application shutdown:
await aw.close();
```

Set `spec.daemon.endpoint` or `spec.daemon.state_dir` to an explicit private
absolute path; this bridge rejects configurations with both paths set to `auto`.
A single `qoder-sdk` target is selected automatically; pass `target` when the
configuration contains several. The bridge defaults to `availability: "native_fallback"`:
AW loss leaves native Queries, Hooks, tools and model execution running. Set
`required: false` on every enabled event applicable to this target; `on_error: block` and explicit denials
still apply while AW is reachable. A conflicting required policy rejects bridge
initialization. Choose `availability: "required"` explicitly when protected work
must stop on AW loss. Availability is an SDK lifecycle option; policy remains in the file.
The bridge supports input observation/blocking when configured, tool-before
observation/blocking and tool-after observation. Unsupported policy effects or
Skill routes fail admission; result replacement and final-result gates retain
their existing managed entrypoints.

After native application readiness, deployment prepares sec-core in the background
and runs under the application user:

```bash
aw instance attach --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

With `daemon.startup: on_demand`, this command starts or reuses AW. The PID must
belong to the Node application running the installed bridge. Preparation can
overlap model inference. Without `input.submit`, the bridge installs no
`UserPromptSubmit` callback and creates no Scope before the first native tool.
The first `PreToolUse` fixes the Task policy; required mode waits for attachment,
while optional mode immediately fixes native fallback if AW is not Ready; internal
before observation also correlates an after-only policy. Text-only replies do not
wait. `attachTimeoutMs` defaults to 5000 ms: timeout denies a required tool. Optional mode does not wait for attachment. Once admitted, each Query
retains one Scope and the existing loss/recovery semantics. Changing the file to
add `input.submit` requires restarting the bridge before attachment.

`aw.query` accepts the original Query input and returns a proxy preserving native
methods. Consumption completion, early iterator return and explicit `close()`
close the native Query and its Scope. Application shutdown closes remaining
Queries. Each Query's `cwd` and copied environment define its task context;
`QODER_PERSONAL_ACCESS_TOKEN` stays with native authentication and is excluded from
AW Hook environments. Other sensitive variables should be excluded through the
bridge's `environment` option. Keep one AW integration per Query. This path uses
public Hooks and adds no module patch or preload to the native SDK.

## Shared admission boundary

TypeScript and Python export `Admission` alongside `Instance` and `Task`.
A Source installs its native callbacks first, then creates one Admission for each
native request with the installed event set and copied workspace/environment.
Call `prepare()` before the earliest protected input or first tool. The common
SDK owns bounded Ready waiting, one Task, cancellation and close; the Source owns
native capture, field mapping and adoption. Service, Host and executor retain
policy admission and Provider execution.

An installed `input.submit` route waits for preparation even with optional
availability. Optional input may fall back only after its attachment deadline;
explicit errors and cancellation still propagate. Required tool admission waits;
optional tool-only admission fixes native fallback immediately when detached.
Calling `close()` cancels pending admission, so stop/drain native work before
releasing its Task. Sources must call `prepare()` at the appropriate boundary;
the SDK does not infer model dispatch or tool execution from a PID.

The Qoder bridge and configured [QwenPaw plugin](python-sdk.md#install-and-integrate)
reuse this common lifecycle. Deployment Descriptors use the existing installed
capability profiles. Automatic Source discovery and resolution remain planned.

## Deployment Descriptor

Generate a Descriptor during deployment, using the same candidate AW build and
exact configuration file that the application and Service will use. For a target
named `sdk` with `adapter: qoder-sdk`, run:

```bash
umask 077
aw config descriptor --config /absolute/aw.yaml --agent sdk \
  --profile qoder-public-hooks > /absolute/aw-sdk.json
```

Check the command's exit status before publishing the output. Generate into a
staging location and install the successful JSON together with its configuration
before starting the application. Pass both absolute paths to the factory:

```javascript
const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  descriptorFile: "/absolute/aw-sdk.json",
  query,
});
```

For QwenPaw, select `qwenpaw-installed-native-hooks` and its configured target;
pass `descriptor_file` to [the Python factory](python-sdk.md#install-and-integrate).
Compilation applies the existing capability admission rules and records the
target, profile, explicit socket, input boundary and availability. It starts no
Service, Provider or native application. The QwenPaw profile requires service
availability; Qoder records whether applicable events require protection. In both
loading paths, Qoder retains the caller's availability choice and defaults to
`native_fallback`. Pass `availability: "required"` explicitly when the descriptor
requires it; a conflicting choice fails before installing the listener.

SDK loading validates the schema version, exact profile and optional explicit
target, then hashes the raw configuration bytes. Each input must be an application-user-owned regular
file of at most 4 MiB, with one hard link, no special mode bits and no group/world
write permission. Use an absolute path without symlinks or `..`; the immediate
parent must also be application-user-owned and not group/world writable. A missing, malformed, mismatched or stale Descriptor fails
initialization; an explicit Descriptor never falls back to `config show` or
native execution. Even a whitespace-only configuration edit requires regeneration.
The loading path invokes no AW executable and parses no YAML.

The Descriptor compiler and both SDK readers support ext2/3/4, XFS, Btrfs, tmpfs and OverlayFS,
checking both the open file and its pinned parent. Other filesystems are rejected;
select the existing CLI-loading API explicitly when deploying there. Node accepts
read-only POSIX ACL entries when their effective write mask passes the mode check.
Python and the CLI additionally reject extended file ACLs and native ACLs on the
parent. Deploy plain files without extended ACLs for the same behavior in both
languages.

The Descriptor and configuration, including their parent directories, must be
controlled by the trusted deployment owner. The digest detects a mismatched
configuration; it does not authenticate the publisher or prevent a writer from
changing wiring hints. Service still owns policy admission and execution.
Drain the application, deploy a matching pair and restart when wiring changes.
To return to offline CLI loading, omit `descriptorFile` / `descriptor_file` and
restart with the corresponding AW executable available.

## Framework-free integration

Configure a target named `app` with `adapter: sdk` and the application's ordinary
`argv`. The application starts itself; `aw run` cannot launch this profile.
Start AW separately with the complete configuration. Use the
[TypeScript example](../../../../../src/aw/sdk/typescript/examples/application.mjs)
or [Python example](../../../../../src/aw/sdk/python/examples/application.py), set
`AW_SOCKET` to its private socket, and install the matching language package.
The examples perform one tool call, adopt a before-hook block, emit the resulting
observation and release their task. The configured policy determines the decision.

Declare only the installed semantic boundaries, using `version: aw-hook-source/v1`
and `entrypoint: application`. `install(routes)` confirms every requested route
before Ready. `task.emit` accepts input submission, tool-before and tool-after
payloads and returns a decision plus coverage. Applications adopt `block` before
native execution, preserve native permission checks and close tasks after work
has drained. Exact fields and supported effects are in the
[semantic ingress contract](../../../../../src/aw/docs/design/en/runtime/native-sdk.md#shared-semantic-ingress).

A native Hook or optional instrumentation source can feed these same callbacks.
Source discovery and automatic coverage selection remain planned. Adding an
application with these semantics needs no new framework-specific Rust Adapter.
For richer native effects, use the optional integrations below and the
[Python/QwenPaw guide](python-sdk.md).

## Install and connect

Install the candidate tarball in the application directory:

```bash
npm install /absolute/artifacts/anolisa-aw-sdk-0.1.1.tgz
```

Use an absolute socket path in a private directory owned by the application
user. Run AW under that same user with the complete configuration containing
`agents.qoder.adapter: qoder-sdk`. For `native_fallback`, set `required: false`
on every enabled event applicable to this target; explicit Provider denials still apply. Start the service
independently with `aw service serve --config /absolute/aw.yaml` and select the
socket reported by that service. The SDK never starts a service or an Agent.

The installed `examples/qoder.mjs` uses the configuration-driven bridge.
For explicit lifecycle control, Keep the native application's authentication, daemon, model,
permissions, setting sources and Hooks. Pass its existing options to
`qoderOptions(task, options)`, then create the native Query and immediately
`bind(query)` on the returned integration. Every Query needs its own task; close
the Query before closing the task. Close the Instance when the application exits.
Do not combine this integration with the existing AW Query factory on one Query.

Construct `Instance` with an explicit endpoint, target, capabilities and
availability. `cwd` and `environment` default to the application values and form
the task context; pass the native Query directory and copied environment to
`instance.task({cwd, environment})` when they differ. `install(routes)` must return `true`
only when the application has installed a router for those events. It runs on
registration and each task's policy snapshot before Ready acknowledgement.
The example routes all Queries through `qoderOptions` and rejects unknown events.
Ready records the application's installation acknowledgement; it does not prove
that each future Query has installed Hooks or adopted returned effects.

`await instance.connect()` performs one bounded connection attempt. It returns
`false` only for native-fallback transport absence or an unfinished registration.
Call `instance.start()` to enable a single background reconnect loop; inspect
`state` and `lastError` for readiness or known errors. `await instance.stop()` stops reconnection;
`close()` also releases this application's Scopes and registration. These calls
do not stop the shared Service or the native daemon.

## Availability and task ownership

| Mode | Service absent or connection lost | Explicit error or denial |
| --- | --- | --- |
| `required` | Reject the operation | Preserve the failure or denial |
| `native_fallback` | Return neutral AW callbacks after bounded detection | Preserve the failure or denial |

`timeoutMs` defaults to 1000 ms for lifecycle RPCs. Callback RPCs allow at least
the admitted event budget plus 1000 ms for transport; Service retains the shared
event deadline across ordered steps. The first transport failure retires the
connection. In `native_fallback`, its active tasks remain in fallback and later
callbacks return immediately. In `required`, those tasks remain failed and new
protected tasks are rejected until recovery. Background reconnection retries
transport unavailability in both modes; explicit protocol/configuration errors
stop it. Reconnection registers and confirms Ready for new tasks only. No callback, tool or uncertain Provider invocation is replayed.
Already adopted input/result changes remain part of the native task.

Pass `AbortSignal` to `instance.task({signal})` or `task.invoke`. User cancellation
remains cancellation. Protocol mismatch, malformed input, configuration rejection,
remote deadline errors and Provider failures remain errors. Qoder before-hook
failures deny the tool; post-hook Provider failures stop the Query and are also
available as `integration.failure`. Transport fallback leaves the Query running.
Check `integration.failure` after consumption to surface policy execution errors.

When the Service remains reachable, failed or cancelled callback RPCs drain
their event before propagating the error.
If native code stops between ordered steps, call
`await task.closeEvent(hook, originalPayload)` for that event. Cleanup uses the
original Scope and an independent bounded request, even when the caller's signal
is aborted. A successful close preserves the Task and prevents late steps from
reopening that event. Completed events retain Service automatic retirement.


## Cooperative attach and detach

An application can publish its installed SDK support with `instance.listen()`.
It may start before AW. Once the configured Service is available, discovery lists
the application; it remains detached until requested. Use
`instance.listen({automatic: true})` for automatic registration plus CLI control.
Choose either `start()` or `listen()` as the background owner; await `stop()`
before starting another listener. Both attach modes
use the same registration and Ready path. Direct `connect()` remains available
for explicit application startup before enabling the listener.

From a terminal under the same application user, select the exact
`application_id` returned by the first command:

```bash
AW_SOCKET=/absolute/private/aw.sock
aw instance list --socket "$AW_SOCKET"
AW_SDK_INSTANCE=application_id_from_list
aw instance attach --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
aw instance detach --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
```

The result reaches `completed` only after the application confirms Ready or
releases its idle registration. Running tasks reject detach with
`sdk_control_busy`; finish them and retry. The selected instance stays detached
through Service recovery until another attach. Existing fallback tasks stay
native; only new tasks use a recovered registration. Detach leaves the native
application and Service running. `await instance.stop()` withdraws discovery and stops polling;
`close()` additionally releases owned tasks and registration.

Two instances in one PID have distinct IDs. Service restart changes discovery
IDs; list again before selecting. An application without an active SDK listener
cannot be attached. The private socket accepts only the Service user and the
application must acknowledge with its authenticated PID/start time. The CLI
cannot alter the application's availability mode or install Hooks remotely.

`--timeout-ms` defaults to 5000 and accepts 100–30000. A timeout or interrupted
CLI leaves confirmation unknown; inspect `instance list` before another action.
The retained request expires at its Service deadline and late Ready cannot
activate that request. List includes the latest request outcome and connected
registration; general `aw service status` also reports SDK readiness and owner.

## Verified coverage

The initial Qoder integration uses public `UserPromptSubmit`, `PreToolUse`,
`PostToolUse` and `PostToolUseFailure` callbacks and canonical Adapter effect
handling. Existing Hook groups are preserved and AW callbacks are appended. The Worker may
schedule groups concurrently. AW evaluates its callback snapshot; native Hooks
that later rewrite arguments are outside this integration's final-input guarantee.
SDK 1.0.56 / Worker 1.1.78 acceptance covers real Bash execution, native Hook
coexistence, AW loss during a Query, recovery for a new task and exactly-once
side effects, plus AW and native denials with no tool side effect. The package does not import Qoder at its root or access private
binding files. Other framework versions negotiate their declared capabilities;
native acceptance remains version-specific.

The resident `aw run` path retains its broader managed MCP, credential and
pinned Worker integration. This public-Hook SDK does not yet provide those
additional managed seams. Keep native MCP configuration in the application's
options. Skill checks use the canonical Adapter resolver and require its native
query interfaces. Input/result rewriting and Skill/MCP combinations require
their own native acceptance before business rollout.

See the [SDK protocol](../../../../../src/aw/docs/design/en/runtime/native-sdk.md)
for ownership and wire contracts and the
[Qoder guide](../agents/qoder-sdk.md) for the existing required entrypoint.
