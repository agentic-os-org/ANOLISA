# AW user guide

[中文版](../../zh/user-entrypoint/aw.md)

AW connects your tool policies and Hook commands to an Agent while preserving
its normal interface. You describe the programs and events in `aw.yaml`; AW
starts or reuses a local service, connects the supported native Hooks and keeps
execution records after the Agent session ends.

The current Linux source build supports Qoder CLI 1.1.64 and OpenClaw 2026.9.6.
Other first-release adapters are delivered separately. AW does not install an
Agent or configure its model account; retain the framework's native configuration.

## Current support

| Capability | Status |
| --- | --- |
| Validate one configuration with all 16 event names | ✅ Recognizing an event does not install a Hook |
| Start Qoder CLI 1.1.64 through AW | ✅ Interactive and print entrypoints |
| Run structured Providers before tools | ✅ `observe` and `block` |
| Run structured Providers after tools | ✅ `observe`; success, error and blocked-attempt coverage depends on the framework |
| Run native scripts and commands before/after tools | ✅ Unchanged callback input; byte output and exit status forwarded |
| Preserve existing Qoder Hooks and their scheduling | ✅ Default settings and an explicit extra settings file |
| Keep a shared service and persistent execution metadata | ✅ On-demand or externally started service |
| Start OpenClaw through AW | ✅ a new Gateway with Agent tool hooks |
| Start the other first-release frameworks | ❌ Separate adapters pending; QwenPaw is distinct from Qwen Code |
| Use other events, portable `ask`, result replacement or OS enforcement | ❌ Not admitted by the current structured Provider path |
| Install a published AW package or generate a default configuration | ❌ Copy the example manually |

For Qoder, `tool.after` maps to successful `PostToolUse` callbacks. Qoder's
`PostToolUseFailure` is a separate event and is not connected in this adapter.
Native Hook commands remain subject to Qoder's own response semantics. Passing
through a native approval response does not establish portable AW approval
support; interactive approval is not part of this delivery's acceptance.

## Build and start Qoder

AW is not yet available through `anolisa install` or an RPM. Developers can
build it on Linux with rustup and the repository's pinned toolchain. Install
Qoder CLI 1.1.64 separately and verify its version. From the repository root:

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

The [Qoder example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)
uses `argv: [qodercli]`. If another version is on `PATH`, replace that entry with
the absolute path of the supported executable. `--agent qoder` selects the named
entry under `spec.agents`; the name is yours to choose, while `adapter: qoder`
selects the framework.

This example consumes Hook input and returns `{}` before and after tools. It
adds no restriction and is not a security policy. Ask Qoder to run a harmless
read-only tool to exercise both callbacks. A reply produced without a tool call
does not exercise them. Arguments after `--` go to Qoder, for example:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder -- -p 'Read the current directory name with a tool.'
```

AW opens Qoder's native terminal interface, then returns to the original shell
when Qoder exits. It releases that session's binding and unfinished work. The
shared daemon remains available to later sessions using the same configuration.

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

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

## Upgrade the service

This build uses local protocol `aw-service/v1alpha2`. A daemon from an older
build is rejected with `protocol_version` before Agent binding or callbacks.
Before replacing the executable, exit its Agent sessions and use the **old**
`aw stop --config FILE` (or `--socket ABSOLUTE_PATH`) to stop each old daemon.
Then update the CLI and daemon together and launch again. Do not delete a live
socket or its audit history. The AW configuration and Provider protocol remain
`aw/v1alpha1` and `aw-provider/v1alpha1`.

## Connect your programs

Each named object in `spec.providers` describes a program. An event step refers
to its name through `provider`. Choose the protocol for the program you have:

| Protocol | Input and result | Step fields |
| --- | --- | --- |
| `aw-provider/v1alpha1` | AW performs `describe`, `validate_config` and `invoke`; responses contain checked candidate effects | `operation`, `effects`, `on_error` |
| `native-hook/v1alpha1` | The command receives the selected adapter's native callback bytes; the adapter handles stdout, stderr and exit status | `native: {}`, `on_error`; omit `operation` and `effects` |

For a native Hook, replace the example's `transport.argv` with the executable
and literal arguments for your script. AW does not insert a shell; shell syntax
requires an explicit `/bin/sh -c ...`. Keep `config: {}` for this protocol:
there is no Provider configuration exchange. Qoder-specific native output is
not automatically portable to another framework.

For structured policies, use `aw-provider/v1alpha1` and put Provider-owned
settings in `config`. The runnable [policy example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.yaml)
shows before-tool `observe`/`block` and after-tool `observe`. Build its executable
with `cargo build --locked -p aw-provider --example policy` and run from `src/aw`
because its command path is relative. Its blocked tool name is illustrative;
replace it with a tool actually used by your Agent when testing a block. The
sample Provider is not sec-core.

Raw commands use the actual callback environment, including profile-loaded
variables. Structured Providers keep the environment pinned at binding.
Environment contents are excluded from events and audit.

`timeout_ms` limits one command; `default_event_budget_ms` limits the whole event.
The Qoder launcher accepts event budgets from 1 to 55,000 milliseconds.
The output ceiling bounds the returned bytes. `on_error` controls execution
failures, separately from a Provider's successful policy block. For native
commands, an ordinary nonzero exit remains native output, not an AW transport
failure. Qoder interprets exit 2 as a blocking response where supported and
other nonzero exits as nonblocking errors. An observe result adds no permission
and does not override Qoder's own tool permissions.

## Keep existing Hooks and scheduling

Qoder continues to load its normal user, project and local settings. AW creates
session-specific callback entries without editing those files. To include an
existing JSON file that you normally pass through Qoder's `--settings`, use:

```bash
target/debug/aw run --config ./aw.yaml --agent qoder --native-settings ./qoder.settings.json
```

AW preserves that file's other fields and Hook entries, adds its callbacks and
passes the merged settings to Qoder. Each AW step becomes a separate synchronous
native Hook. Qoder determines serial or parallel execution, including how the
new callbacks coexist with other matching Hooks.

Set `spec.agents.qoder.qoder.sequential: true` to mark the generated groups as
sequential. If any matching synchronous Qoder group requests sequential
execution, all matching synchronous Hooks run in sequence. Omitting this field
or setting it to false does not override an existing group's true value.
Steps in one native event share the AW budget; later callbacks do not reset it.

This adapter supports scripts whose input remains unchanged across AW steps.
Sequential input-rewrite chains are not supported: if a command returns
`updatedInput`, a later callback with changed input is rejected and follows that
step's `on_error`. Forwarding native bytes does not promise every native effect
combination or approval flow.

The launcher rejects settings and arguments that would disable or replace its
Hooks rather than overriding a user's disabled-hook choice. This includes raw
`--settings`, `--setting-sources`, `--headless-fast-hooks` and conflicting native
settings. Custom configuration roots, alternate Qoder configuration-directory
modes, resumed sessions, remote sessions and worktree launch are outside the current adapter's
scope. Pass short options separately rather than combining them, and launch from
the intended working directory. Existing native Hooks remain
responsible for their own behavior and audit records.

## Service lifetime and records

`spec.daemon.startup: on_demand` starts a service if none exists at the selected
endpoint; `external` requires one to be running already. An existing service is
reused only when its protocol and exact configuration revision match. With
`endpoint: auto` and `state_dir: auto`, AW selects a private configuration-specific
directory under `XDG_RUNTIME_DIR`, or under `/tmp/aw-UID` when that variable is
unset. Explicit paths must be absolute and agree on the `aw.sock` location.

The service retains a fixed configuration snapshot. Editing `aw.yaml` does not
reload it. Stop a service using its original file or socket before retiring that
configuration; auto paths for changed configuration may select a different
service. Exiting an Agent does not stop other sessions or the shared daemon.
The local endpoint is a same-user boundary, not a sandbox.

| Command | Purpose |
| --- | --- |
| `aw validate --config FILE` | Check syntax and static references without executing programs |
| `aw run --config FILE --agent TARGET [OPTIONS] -- ARGS` | Start the configured Agent and connect supported Hooks |
| `aw install --config FILE --agent TARGET [--native-profile PROFILE]` | Dispatch persistent native Hook setup; no current adapter supports it, including Qoder and OpenClaw |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | Run the service in the foreground |
| `aw status --config FILE` or `aw status --socket ABSOLUTE_PATH` | Inspect the selected service without starting it |
| `aw stop --config FILE` or `aw stop --socket ABSOLUTE_PATH` | Request graceful shutdown |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | Send one developer operation JSON object from stdin; default 5,000 ms |

`--agent TARGET` selects a named entry in `spec.agents`; its `adapter` selects
the implementation. This build implements Qoder CLI 1.1.64 and OpenClaw 2026.9.6. `aw install` validates the configuration and dispatches to that adapter;
Both use temporary launch configuration and reject persistent installation.
This command does not install AW packages or Agent software, start an Agent,
or configure model credentials. `aw run` does not implicitly call `install`.

| Adapter-specific option | Command | Current support |
| --- | --- | --- |
| `--native-settings JSON_FILE` | `run` | Qoder: optional extra JSON settings merged with generated Hooks; the original file remains unchanged. OpenClaw: required existing native JSON configuration |
| `--native-profile PROFILE` | `run`, `install` | Adapter profile selector; no current adapter accepts it. Qoder rejects it for `run` and rejects `install` altogether |
| `--native-state-dir DIRECTORY` | `run` | OpenClaw: required absolute native state directory. Qoder rejects it; this is not the AW service's `--state-dir` |

The shared command parser recognizes these adapter-specific options, but that
alone does not enable them for a framework. Hermes and QwenPaw remain
unsupported in this build. Pass supported AW options before `--`; arguments
after it are forwarded literally to the Agent. `install` accepts no Agent
arguments. Use `aw --help` to see the command syntax and current support.

In a source checkout, use `target/debug/aw` for `aw`. `aw hook` is an internal
callback generated by the launcher; users do not need to construct it. Service
records contain metadata without tool input/results, private Provider
configuration or raw stdout/stderr. A running service or completed invocation
does not prove native policy adoption. Killed or disabled native callbacks can
miss checks; this version does not provide final/protected execution or an OS
fallback.

## Run the local service demo

This demonstration needs no Agent account or model request. From `src/aw`:

```bash
cargo build --locked -p aw-provider --example policy
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

In a second terminal in `src/aw`, use the printed absolute socket path:

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- "$AW_DEMO_SOCKET"
```

The example supplies synthetic capabilities and events. It checks `read_demo`,
blocks `delete_demo`, observes an after event and prints audit keys. No native
Agent or tool is started. Replace `AUDIT_KEY` with a reported preparation key or
event ID to inspect its records:

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

`terminal: false` means no terminal record is present; the operation may be
active or interrupted. A timed-out call must not be retried as a new step.
The stop response acknowledges the request; wait for the foreground service to
exit before removing this demo directory in its original terminal:

```bash
rm -r -- "$AW_DEMO_ROOT"
```

Normal shutdown retains audit history and removes the owned socket. After a
forced kill, AW reports a stale socket instead of deleting it automatically.
Verify that the old service has stopped before removing its owned `aw.sock`;
keep its lock and journal when retaining the service directory. Restarting
creates a new service identity; historical records remain queryable without
resuming or replaying old events.

The [configuration reference](../../../developer-guide/en/aw/configuration.md)
covers every field and event name. The [local service contract](../../../../src/aw/docs/design/local-service.md)
describes operation JSON, deadlines and lifecycle for Adapter developers.

Connect sec-core scanning through `aw-provider-sec-core`; see the [Provider guide](aw-sec-core.md).
