# AW user guide

[中文版](../../zh/user-entrypoint/aw.md)

AW connects your tool policies and Hook commands to an Agent while preserving
its normal interface. You describe the programs and events in `aw.yaml`; AW
starts or reuses a local service, connects the supported native Hooks and keeps
execution records after the Agent session ends.

The current Linux source build supports Qoder CLI 1.1.64, OpenClaw 2026.9.6,
QwenPaw 2.2.2b4 / AgentScope 2.0.8 and Hermes revision `952c941e`.
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
| Start QwenPaw through AW | ✅ the official App/API entrypoint |
| Start Hermes through AW | ✅ local chat using an explicitly installed native plugin |
| Start the other first-release frameworks | ❌ Separate adapters pending; QwenPaw is distinct from Qwen Code |
| Use other events, portable `ask`, result replacement or OS enforcement | ❌ Not admitted by the current structured Provider path |
| Install a Preview package and generate a Qoder/sec-core configuration | ✅ [Preview guide](aw-preview.md); not a stable release |

For Qoder, `tool.after` maps to successful `PostToolUse` callbacks. Qoder's
`PostToolUseFailure` is a separate event and is not connected in this adapter.
Native Hook commands remain subject to Qoder's own response semantics. Passing
through a native approval response does not establish portable AW approval
support; interactive approval is not part of this delivery's acceptance.

## Install a Preview

Use the [Preview packages](aw-preview.md) for a prebuilt AW + sec-core demo.
The source instructions below are for developers.

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

## Start QwenPaw

Install QwenPaw 2.2.2b4 with AgentScope 2.0.8 and initialize its model configuration
through QwenPaw. The example uses `argv: [qwenpaw, app]`. Set
`QWENPAW_WORKING_DIR` to the existing initialized absolute directory. AW adds only
its owned temporary plugin, preserves existing plugins and native configuration,
and removes its plugin after the App exits.

Native middleware retains onion ordering: before runs forward, after unwinds in
reverse, and the host controls concurrent tools. After observes terminal
`ToolResponse` objects, including AW-denied results, without repeating streaming
chunks. The point covers tools executed inside the App; external execution tools
and attempts denied before middleware are outside its coverage. Event budgets
must be 1..55,000 ms. Native commands use the AW plugin's documented convention:
before exit 2 denies, exit 0 continues, and explicit ask/approve is unsupported.
Callback failures are reported and follow the step's `on_error`; `report` preserves
execution and after results, while before `block` denies. Cancellation still propagates.

The native startup hook confirms plugin registration after loading completes.
It does not promise enforcement before the App's public endpoint opens. ACP/TUI,
reload and multi-worker launch remain rejected because this integration does not
verify their plugin lifecycle. Use the App's normal API or interface after the
native Hooks ready message.

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

The [neutral QwenPaw example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
runs commands before and after tools; it installs no security policy. Use the
same `aw-provider/v1alpha1` policy configuration across supported adapters.
The native Hook output dialect and event coverage remain framework-specific.

A configuration with no Providers or event steps also starts the App; AW verifies
plugin readiness without registering any tool middleware.

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

## Boolean policy commands

For a simple rule, let a command read one normalized event JSON from stdin and
write only the JSON boolean `true` or `false` to stdout, then exit 0. `true`
means the rule matched: AW requests the configured effect. `false` requests no
effect and does not override native permissions. Diagnostics may go to stderr
and do not enter the Provider reply or audit. AW's built-in `aw policy` bridge
handles `describe`, `validate_config`, `invoke`, request IDs and digests; your
script implements none of those methods or fields. Full structured Providers
continue to use the existing protocol without optional handshake methods.

For example, save this rule as `$AW_DEMO/check.py`; Python is a dependency of
this chosen script, not AW core:

```python
import json
import sys

def contains(value):
    if isinstance(value, str):
        return "12345" in value
    if isinstance(value, list):
        return any(contains(item) for item in value)
    if isinstance(value, dict):
        return any(contains(item) for item in value.values())
    return False

event = json.load(sys.stdin)
print(json.dumps(contains(event["tool"]["input"])))
```

Using the Preview prefix and paths from the [installation guide](aw-preview.md),
generate the full YAML instead of assembling Provider and step objects by hand.
Supply only the Agent entrypoints you need; this example declares both:

```bash
"$AW_PREVIEW_PREFIX/bin/aw-package" configure --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw-command.yaml" --state-dir "$AW_DEMO/command-state" \
  --qoder "$AW_QODER" --node "$AW_NODE" --openclaw "$AW_OPENCLAW" \
  --provider command --check /usr/bin/python3 --effect block \
  --reason-code parameter_contains_12345 -- "$AW_DEMO/check.py"
"$AW_PREVIEW_PREFIX/bin/aw" validate --config "$AW_DEMO/aw-command.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw-command.yaml" --agent qoder
```

No sec-core package or daemon is needed. Use the same generated configuration
with `--agent openclaw` and its existing native profile options. The script sees
the same normalized tool fields for both frameworks, while AW translates a
block into each native Hook response. The generated Provider transport runs
`["/absolute/prefix/bin/aw", "policy"]`; the command and matched effect are in
`spec.providers.command.config`:

```yaml
config:
  version: 1
  argv: [/usr/bin/python3, /absolute/path/check.py]
  timeout_ms: 1000
  on_true:
    type: block
    reason_code: parameter_contains_12345
```

`on_true.type` supports `block` or `observe`; `block` applies only before a tool,
while `observe` can also be configured after a tool. The generator creates one
required before-tool step, with `on_error: block`, a 2,000 ms Provider timeout
and a 1,000 ms command timeout. A nonzero exit, timeout, oversized output or a
non-boolean response is a Provider error, never a false result; the step's
failure action applies. The boolean output is limited to 32 bytes including
whitespace, and stderr to 65,536 bytes. Command argv is literal; AW does not
insert a shell. Handshake methods validate configuration without executing the
user command. `aw validate` checks the desired document; runtime admission also
validates the bridge's private configuration before installing policy steps.

`configure` creates a new private file and never overwrites an existing one.
To remove a policy, generate a new base document without `--provider` (or with
`--provider none`), then exit the old Agents, stop their service and explicitly
switch to the new configuration. Provider definitions and associated steps are
removed together; editing a running service's file does not reload it. Generic
editing of existing YAML is outside this command.

## Start Hermes

Use the official Hermes checkout at revision `952c941e741e922a9be8fc403c8944c6e96318bb`.
Both installation and launch reject staged or unstaged changes to tracked files;
commit or stash local changes and restore the pinned revision before retrying.
The example uses `argv: [hermes, chat]`; select that installation's Python console
script, such as `/absolute/hermes/venv/bin/hermes`, with an absolute Python
shebang. Explicit paths and PATH lookup both require executable regular files;
FIFOs, directories and non-executable files are rejected before native probes.
Shell wrappers and `python -m` entrypoints are unsupported. AW uses that
interpreter to enter the same native CLI process, preserving its arguments and
working directory. Native profile loading determines the effective
`HERMES_SAFE_MODE`, including profile `.env` overrides; safe mode is rejected,
and plugin registration is required before chat can dispatch tools.
Initialize its profile and model account through Hermes first. `aw install` is an
explicit one-time installation of the native AW plugin in the selected profile.
It saves the original configuration before enabling that plugin, retains unknown
configuration fields, and refuses a conflicting non-AW plugin. The native YAML
writer edits a private candidate before AW atomically exchanges it with the live
config. The command reports the exact initial `backup` and a permanently retained
`displaced` file (`<backup>.displaced`) containing the original inode, including
later writes through an already-open descriptor. Stop native config writers
during installation and rollback, and review both recovery files before restoring.
A writer failure or conflict detected before publication leaves the live config
unchanged; a conflict detected at the exchange boundary reports failure with the
candidate already published and preserves the displaced content for review.
Success also requires removing the config staging directory and syncing its parent.
A cleanup or sync failure reports the already-published configuration and both
recovery paths, without reporting success or rolling back.
This exchange does not provide compare-and-swap against writers that ignore AW's
lock. Installation does not hot-load or restart existing Hermes services.
The canonical profile directory, configuration and bundled plugin files must
be owned and not writable by others. If the profile has a `.env`, it must also
be a caller-owned regular file without group/other write permission (0600 or 0644
is accepted). AW checks it without following symlinks before any native probe,
including `--version`, because importing the CLI loads that file.
Every ancestor must belong to the caller or root and prevent group/other writes,
except sticky directories such as `/tmp` that protect owned children. AW rejects
unsafe paths without changing their permissions. Ctrl-C or SIGTERM cancels native
installation probes/writers, removes staging copies and retains the signal exit status.
Installation and launch probe errors remain visible before signal exit, including
unverified process-group cleanup. Before probing or modifying the profile, installation
validates configured native arguments using the same local-chat restrictions as launch.
An executable-only `argv` remains supported; supply `chat` and its options after
`aw run`'s `--` in that case.
Chat `--help`/`-h` exits before plugin readiness, so AW rejects these options in
configured arguments and arguments appended to `aw run`, before native probes or
profile writes. Run `hermes chat --help` directly to view native help. Literal
query values such as `--query=--help` remain supported.

An AW upgrade can change the bundled plugin bytes. If launch or installation
reports a mismatched or incomplete plugin, stop Hermes sessions and plugin writers, review
`<profile>/plugins/aw-native-hooks`, and move that directory to a backup outside
`<profile>/plugins`. Rerun the same `aw install` command to install matching files;
the saved directory remains available for inspection or rollback.

An empty `providers` and `events` configuration launches with zero AW callbacks.
Existing native Hooks still run; AW records the session lifecycle without
claiming per-tool policy checks or audit records. Explicit installation remains
required.

Subsequent `aw run` calls retain that profile's credentials, history and working
directory. Native before/after callbacks run in Hermes registration order;
a before block does not prevent later registered callbacks from running. Post
callbacks also observe blocked and failed attempts. AW preserves native Hook
parsing and the Agent's own approval flow; portable AW `ask` is not supported.
This adapter covers local `chat`, not the native TUI, Gateway or ACP. AW
pins the entrypoint with native `--cli`; full chat options are required, and
profile/worktree/resume switches and option abbreviations are unsupported. Event
budgets are limited to 55,000 ms and must fit the native callback timeout.

```bash
target/debug/aw install --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

The [neutral Hermes example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.hermes.yaml)
runs commands before and after tools; it installs no security policy. Use the
same `aw-provider/v1alpha1` policy configuration across supported adapters.
The native Hook output dialect and event coverage remain framework-specific.

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
| `aw install --config FILE --agent TARGET [--native-profile PROFILE]` | Explicitly install the Hermes native plugin into an existing profile; Qoder, OpenClaw and QwenPaw reject this command |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | Run the service in the foreground |
| `aw status --config FILE` or `aw status --socket ABSOLUTE_PATH` | Inspect the selected service without starting it |
| `aw stop --config FILE` or `aw stop --socket ABSOLUTE_PATH` | Request graceful shutdown |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | Send one developer operation JSON object from stdin; default 5,000 ms |

`--agent TARGET` selects a named entry in `spec.agents`; its `adapter` selects
the implementation. This build implements Qoder CLI 1.1.64, OpenClaw 2026.9.6,
QwenPaw 2.2.2b4 / AgentScope 2.0.8 and Hermes revision `952c941e`. `aw install`
validates the configuration and dispatches to that adapter. Hermes supports
persistent plugin installation; Qoder, OpenClaw and QwenPaw reject it.
This command does not install AW packages or Agent software, start an Agent,
or configure model credentials. `aw run` does not implicitly call `install`.

| Adapter-specific option | Command | Current support |
| --- | --- | --- |
| `--native-settings JSON_FILE` | `run` | Qoder: optional extra JSON settings merged with generated Hooks; the original file remains unchanged. OpenClaw: required existing native JSON configuration |
| `--native-profile PROFILE` | `run`, `install` | Hermes: required existing absolute profile for `run` and `install`. Qoder and OpenClaw reject it |
| `--native-state-dir DIRECTORY` | `run` | OpenClaw: required absolute native state directory. Qoder rejects it; this is not the AW service's `--state-dir` |

The shared command parser recognizes these adapter-specific options, but that
alone does not enable them for a framework. QwenPaw rejects all these native
override options and uses `QWENPAW_WORKING_DIR`. Pass supported AW options before
`--`; arguments
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
