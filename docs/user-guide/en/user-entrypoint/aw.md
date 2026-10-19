# AW user guide

[中文版](../../zh/user-entrypoint/aw.md)

AW connects your tool policies and Hook commands to an Agent while preserving
its normal interface. You describe the programs and events in `aw.yaml`; AW
starts or reuses a local service, connects the supported native Hooks and keeps
execution records after the Agent session ends.

The current Linux source build supports Qoder CLI >=1.1.64, OpenClaw 2026.9.6,
QwenPaw 2.2.2b4 / AgentScope 2.0.9, Hermes revision `952c941e` and explicit
Claude SDK 0.2.88 workers with bundled CLI 2.1.161, plus Pi Coding Agent/Core SDK 0.84.2 workers
and AgentScope SDK 2.0.6 through explicitly wired public factories in a foreground Python worker.
Other first-release adapters are delivered separately. AW does not install an
Agent or configure its model account; retain the framework's native configuration.

Start with the [Preview package guide](aw-preview.md) for installation, or the
[component integration walkthrough](../../../developer-guide/en/aw/integration.md)
for source builds and a policy you can verify without a model. Then use the
shared configuration below to verify native tool behavior. Field definitions
live in the [configuration reference](../../../developer-guide/en/aw/configuration.md).


For optional lifecycle observation, see [AgentSight through aw run](aw-agentsight.md).

## Current support

| Capability | Status |
| --- | --- |
| Validate one configuration with all 16 event names | ✅ Recognizing an event does not install a Hook |
| Start Qoder CLI through AW | ✅ Interactive and print entrypoints |
| Run structured Providers before tools | ✅ `observe` and `block` |
| Run structured Providers after tools | ✅ `observe`; success, error and blocked-attempt coverage depends on the framework |
| Run native scripts and commands before/after tools | ✅ Unchanged callback input; byte output and exit status forwarded |
| Preserve existing Qoder Hooks and their scheduling | ✅ Default settings and an explicit extra settings file |
| Keep a shared service and persistent execution metadata | ✅ On-demand or externally started service |
| Start OpenClaw through AW | ✅ a new Gateway with Agent tool hooks |
| Start QwenPaw through AW | ✅ the official App/API entrypoint |
| Start Hermes through AW | ✅ local chat using an explicitly installed native plugin |
| Start a Pi SDK 0.84.2 worker | ✅ Public SDK factory; see [worker guide](#start-a-pi-sdk-worker) |
| Start a Claude SDK 0.2.88 worker | ✅ Explicit public factory Bridge; see [worker guide](#start-a-claude-sdk-worker) |
| Start AgentScope SDK through AW | ✅ [Explicit public factories](#start-agentscope-sdk) in one foreground Python worker |
| Start the other first-release frameworks | ❌ Separate adapters pending; QwenPaw is distinct from Qwen Code |
| Use other events, portable `ask`, input rewriting or OS enforcement | ❌ Not admitted by the current structured Provider path |
| Install a Preview package and generate a Qoder/sec-core configuration | ✅ [Preview guide](aw-preview.md); not a stable release |

For Qoder, `tool.after` maps to successful `PostToolUse` callbacks. Qoder's
`PostToolUseFailure` is a separate event and is not connected in this adapter.
Native Hook commands remain subject to Qoder's own response semantics. Passing
through a native approval response does not establish portable AW approval
support; interactive approval is not part of this delivery's acceptance.

## Install a Preview

Use the [Preview packages](aw-preview.md) for a prebuilt AW + sec-core demo.
The source instructions below are for developers.

## Use one configuration for four frameworks

The [shared YAML](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/aw.shared.yaml)
and [boolean script](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/check-tool.py)
give all four targets the same before-block and after-observe policy. Follow the
[walkthrough](../../../developer-guide/en/aw/integration.md) to copy them, replace
absolute paths and verify the Provider. The commands here use its `AW_BIN`,
`AW_CONFIG` and `AW_DEMO` variables. Installed-package users can use the same
files with `AW_BIN` pointing to the installed binary; Python 3 is the sample
script's dependency.

Prepare each Agent's supported version and native model configuration first.
Only the selected target must be installed to run it. Use separate native
profiles, choose their tool working directory explicitly and start from
`$AW_DEMO/workspace`. The shared YAML changes neither model credentials nor
native permissions. Test one target at a time initially.

| Target | Native prerequisite | Launch detail |
| --- | --- | --- |
| Qoder | CLI >=1.1.64 with native command Hooks, completed login | Interactive or print; optional `--native-settings` |
| OpenClaw | 2026.9.6, configured JSON and existing state directory; stop any Gateway using that profile | AW starts a new `gateway run`; pass both native paths |
| QwenPaw | 2.2.2b4 / AgentScope 2.0.9, initialized App working directory and model | Set `QWENPAW_WORKING_DIR`; use App/API, not ACP/TUI |
| Hermes | Clean official revision `952c941e`, installed Python console script and initialized local chat profile | Install the AW plugin once into that profile; pass the same profile when running |

Launch Qoder:

```bash
cd "$AW_DEMO/workspace"
"$AW_BIN" run --config "$AW_CONFIG" --agent qoder
```

Launch OpenClaw with your existing absolute native paths:

```bash
"$AW_BIN" run --config "$AW_CONFIG" --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

Launch QwenPaw with your initialized directory:

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home "$AW_BIN" run \
  --config "$AW_CONFIG" --agent qwenpaw -- --host 127.0.0.1 --port 8096
```

Install and launch Hermes after reading its [profile requirements](#start-hermes):

```bash
"$AW_BIN" install --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
"$AW_BIN" run --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
```

For OpenClaw and QwenPaw, keep the server terminal open and wait for AW's native
Hook readiness message before sending requests through that Gateway or App.
Use OpenClaw's connected client, not `agent --local`; use QwenPaw's App UI/API,
not another CLI entrypoint. Qoder and Hermes use the launched native chat.
The framework sections below explain coexistence and supported lifecycle scope.

### Verify native behavior

In another terminal, restore `AW_BIN`, `AW_CONFIG` and `AW_DEMO` from the
walkthrough. Set `AW_TARGET` to the target being tested. Start with absent marker
files; for a repeat, use a fresh name. Print the following two prompts and send
them as separate turns through that target's native interface:

```bash
export AW_TARGET=qoder
printf 'Use your shell tool exactly once to run `printf AW_OK > "%s/%s.allowed"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
printf 'Use your shell tool exactly once to run `printf AW_DENY_TEST > "%s/%s.blocked"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
```

These commands only write demo markers if executed. The second input contains
the policy's deny marker. After the turns, check:

```bash
cat "$AW_DEMO/workspace/$AW_TARGET.allowed"
test ! -e "$AW_DEMO/workspace/$AW_TARGET.blocked"
"$AW_BIN" status --config "$AW_CONFIG"
ls "$AW_DEMO/state/journal"
```

Expect `AW_OK`, an AW policy denial for the second turn, and no blocked marker.
Read the relevant journal through AW: select a filename from the listed
directory and use its 64-character name without `.jsonl` as `AW_AUDIT_KEY`.

```bash
export AW_AUDIT_KEY=REPLACE_WITH_EVENT_KEY
printf '{"method":"audit","key":"%s"}\n' "$AW_AUDIT_KEY" | \
  "$AW_BIN" request --socket "$AW_DEMO/state/aw.sock"
```

Files include preparation and event journals. For an event, inspect
`result.records[0].record.plan`: `kind` is `event`, and its instance and binding
must match the `AW instance` reported at launch.
The before-denial call should have `policy_block: true`; the completed after
step should name Provider `check-tool` and have `status: ok`. Audit stores
metadata, not raw inputs/results or effect reason text. A closed event has
`terminal: true`. The following checks complete the acceptance record:

| Check | Evidence |
| --- | --- |
| Allowed tool | Tool ran, allowed marker exists, before and supported after steps succeeded |
| Policy block | AW returned a denial, blocked marker absent, before audit records a policy block |
| Provider failure | After stopping the old sessions/service, launch a copied config with an absent before-script path; the invocation fails, `on_error: block` prevents the marker, and audit reports failure rather than a policy hit |
| Coexistence | Keep one existing native Hook/plugin enabled; confirm its expected behavior still occurs alongside AW |
| Restart | Exit Agents, stop AW, relaunch with the original config; new tools are checked and old audit keys remain readable |

Record framework version, AW commit, actual commands and audit keys. Synthetic
Host tests and startup readiness alone do not complete this table. Model
refusal, missing tool invocation or native permission denial is not an AW block.
After-event coverage differs by framework: for example, Qoder reports successful
tools only. Do not require a fabricated after event for every rejected call.

### Stop and clean up

Exit Qoder/Hermes and use Ctrl-C in the owned OpenClaw/QwenPaw server terminals.
Wait for those processes to exit, then stop the shared AW service:

```bash
"$AW_BIN" stop --config "$AW_CONFIG"
```

The stop response acknowledges the request; wait for the recorded daemon PID
to exit and its `aw.sock` to disappear. QwenPaw removes its owned temporary
plugin on normal exit. Hermes keeps its installed plugin: ordinary Hermes
launches without an AW binding do not activate AW callbacks. To undo installation,
stop Hermes and native config writers, inspect the reported `backup` and
`displaced` files, move `plugins/aw-native-hooks` outside the profile's `plugins`
directory and restore the chosen pre-install configuration. Preserve any later
intentional native changes instead of blindly overwriting them. AW has no
native-plugin uninstall command.

Retain audit evidence before deleting the demo directory, following the
[walkthrough's cleanup](../../../developer-guide/en/aw/integration.md#5-verify-native-adoption-and-clean-up).
Framework profiles remain separate. Package removal is covered by the
[Preview guide](aw-preview.md#6-stop-upgrade-or-uninstall).

## Build and start Qoder

AW is not yet available through `anolisa install` or an RPM. Developers can
build it on Linux with rustup and the repository's pinned toolchain. Install
Qoder CLI separately and verify its version. From the repository root:

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

The [Qoder example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)
uses `argv: [qodercli]`. To select a particular installation, replace that entry
with the absolute path of its executable. `--agent qoder` selects the named
entry under `spec.agents`; the name is yours to choose, while `adapter: qoder`
selects the framework.

AW requires a semantic CLI version >=1.1.64, the previously tested baseline,
instead of one exact release. After surrounding whitespace is trimmed, `--version`
must return a bare SemVer such as `1.1.66`, within 1–128 UTF-8 bytes. A leading
`v`, product names, trailing annotations and additional lines are not accepted.
A format error states this expectation and shows the observed output. AW records
the trimmed output without searching it for a version token. Older or invalid
versions are rejected before binding; a prerelease of 1.1.64 is below the minimum,
while build metadata is retained. No YAML version field or downgrade is needed
for a CLI using the same native Hook contract: `--settings`, exec-form command
Hooks with literal `args`, `PreToolUse`/`PostToolUse`, callback IDs/input and exit 2
for blocking.
Version text alone does not certify compatibility; Hook-disabling settings,
unsupported launch modes and malformed callbacks still fail. Earlier AW builds
that required 1.1.64 must be updated to use this behavior.

For example, name the target `qodercli` and select your installed executable:

```yaml
agents:
  qodercli:
    adapter: qoder
    argv: [/absolute/path/to/qodercli]
```

Merge that block under `spec` in your complete configuration, then run
`aw run --config /absolute/aw.yaml --agent qodercli`. The target name is separate
from the native executable name and version.

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

## Start AgentScope SDK

Use a Python environment with `agentscope==2.0.6`; the SDK is separate from
QwenPaw's verified AgentScope 2.0.9 runtime. `argv` selects the SDK environment's
Python and a `.py` application or `-m MODULE`. AW verifies that exact SDK version,
supplies `aw_agentscope` and a protected runtime configuration, and awaits a
receipt after the real public factory has installed middleware.

From `src/aw`, with that environment's `python3` on PATH:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

The example uses a deterministic offline model and a local Echo tool. It needs no
credentials. Its before Boolean Hook returns false (no block match) and after
returns true (observe). Change the before command to `printf true` to deny the
tool; the native final response becomes denied and after observes that denial.
Stop the service before editing its configuration.

For an existing application, use `AwBridge().create_agent(...)` in **every** public
factory, passing the existing middleware list unchanged. Existing trusted
argument injection must precede the appended AW middleware. A readiness receipt
covers the constructed Agent, not other factories or direct dispatch routes.
The [factory contract](../../../../src/aw/docs/design/agentscope-adapter.md) details
ordering, budgets, invocation identity and coverage.

Before waits and can block before a tool side effect. After awaits one final
`ToolResponse`, preserving success, error or interruption without repeating
stream chunks. Native parameter/permission rejections before `on_acting`, direct
Toolkit calls and external execution requests need separate integration. The
launcher supports one foreground worker; forking Gunicorn supervisors and
multi-worker/remote deployment are outside this contract. Long-lived processes
must manage the service's 1024 event-correlation limit per instance explicitly.

For PreTool retry suggestions and final PostTool compression, copy the
[SDK lifecycle example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml),
replace all paths and map your Toolkit's exact tool names/argument pointers in
sec-core. It runs security checks before Tokenless PreTool, observes the original
completed response, then compresses one successful text slot. Keep custom/native
after Hooks before the final compression step; with replacement enabled AW runs
this chain in declared order. Native and observation Hooks cannot replace the
response; only the final admitted structured replacement step can change its
model-visible text. Reported Hook failures retain the original input for subsequent
steps. Observe-only chains keep native reverse unwind order.

PreTool denies the original call and returns full retry arguments to the model;
each new attempt passes native permissions and sec-core again. An attributed RTK
retry avoids double compression even with a new call ID. Install RTK separately
for PreTool; PostTool works without it. See [Tokenless behavior](aw-tokenless.md)
for text-slot eligibility, failure handling and telemetry configuration. Invalid
final-response replacement directives report failure and retain the original result.

## Start QwenPaw

Install QwenPaw 2.2.2b4 with AgentScope 2.0.9 and initialize its model configuration
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

This build uses local protocol `aw-service/v1alpha3`. A daemon from an older
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
and do not enter the response or audit. Custom hooks default to invoke-only
execution: omit the Provider `protocol` and step `operation`. AW performs local
configuration/effect admission without calling `describe` or `validate_config`,
then invokes your script only for an admitted event. The script receives no
method, request ID or digest and constructs no response object. Explicit
`protocol: aw-provider/v1alpha1` retains the full structured Provider handshake.

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
block into each native Hook response. The generated transport invokes your
command directly. Inside `spec`, a custom hook can be declared as follows:

```yaml
providers:
  command:
    transport:
      type: stdio
      location: agent
      argv: [/usr/bin/python3, /absolute/path/check.py]
    timeout_ms: 1000
    max_output_bytes: 32
events:
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-command
        provider: command
        effects: [block]
        reason_code: parameter_contains_12345
        on_error: block
```

Omitted `protocol` means `aw-hook/v1alpha1`; omitted `operation` means `invoke`.
Both may be declared explicitly. Hook `config` is omitted or an empty object;
pass script options through literal argv. `true` requests all effects in that
step's `effects` list, optionally carrying its `reason_code`; `false` requests
none. Effects still require Adapter support. `block` applies only before a tool;
`observe` can also run after a tool. Full Provider steps use `effects` as an
admitted upper bound and let the Provider choose its returned effects.

The generator creates one required before-tool step with `on_error: block` and
a 1,000 ms command timeout. A nonzero exit, timeout, oversized output, incomplete
stdin delivery or a non-boolean response is an execution error, never a false
result; the step's failure action applies. Boolean stdout is limited to the
smaller of `max_output_bytes` and 32 bytes including whitespace. The service
caps stderr at 65,536 bytes. AW does not insert a shell. Preparation validates
the configuration and Adapter capabilities without executing the user command.
`aw validate` checks the desired document offline.

Existing `aw policy` bridge configurations remain supported, including their
private `config.on_true` effect mapping and full Provider protocol. They need
no migration. To switch a custom rule to the default form, point transport argv
at the boolean script, omit `protocol`, `config` and `operation`, and put the
matched effects and optional reason code on the step. Stop the old service and
Agent sessions before switching configurations. Older AW binaries do not accept
the new default form; retain the earlier configuration for rollback.

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
Unset `GIT_DIR` and `GIT_COMMON_DIR` in the parent and profile dotenv environments;
the adapter requires Git to discover this checkout normally. Earlier skipped PATH
names must be protected against replacement or creation by other users, including
sticky writable parents. Nonempty GIT_CONFIG_SYSTEM, GIT_CONFIG_GLOBAL and
XDG_CONFIG_HOME values, and any HOME value, must be absolute in both environments.
Unset unsupported relative values or replace them with absolute paths. Use UTF-8
working-directory, profile, venv, editable-checkout, AW executable and launch-state paths.
Git must resolve to the same canonical executable in cwd and checkout. Both
directories use their own effective configuration checks, including local and
conditional includes; external filter commands and fsmonitor helpers are refused.
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
With AW before callbacks configured, native Hook and tool-execution middleware
argument rewrites are refused, including nested `execute_code` calls. Hermes can
apply them after those callbacks, so the resulting arguments have not been checked.
Observation-only execution middleware remains supported. Empty and after-only AW
routes retain native rewrite behavior. When AW callbacks are configured, forced
plugin reload during before checks or tool execution is refused before teardown to retain pending
audit callbacks; reload between tools keeps native behavior.
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
| `aw-hook/v1alpha1` (default) | The command reads one normalized event and returns JSON true/false; AW projects the configured effects on a match without a script handshake | `effects`, `on_error`; optional `reason_code`; omit `operation` or use `invoke` |
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
| `aw install --config FILE --agent TARGET [--native-profile PROFILE]` | Explicitly install the Hermes native plugin into an existing profile; Qoder, OpenClaw, QwenPaw, Pi, Claude SDK and AgentScope SDK reject this command |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | Run the service in the foreground |
| `aw status --config FILE` or `aw status --socket ABSOLUTE_PATH` | Inspect the selected service without starting it |
| `aw stop --config FILE` or `aw stop --socket ABSOLUTE_PATH` | Request graceful shutdown |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | Send one developer operation JSON object from stdin; default 5,000 ms |

`--agent TARGET` selects a named entry in `spec.agents`; its `adapter` selects
the implementation. This build implements Qoder CLI >=1.1.64, OpenClaw 2026.9.6,
QwenPaw 2.2.2b4 / AgentScope 2.0.9, Hermes revision `952c941e` and explicit
Claude SDK 0.2.88, Pi SDK 0.84.2 and AgentScope SDK 2.0.6 workers. `aw install`
validates the configuration and dispatches to that adapter. Hermes supports
persistent plugin installation; Qoder, OpenClaw, QwenPaw, Pi, Claude SDK and
AgentScope SDK reject it.
This command does not install AW packages or Agent software, start an Agent,
or configure model credentials. `aw run` does not implicitly call `install`.

| Adapter-specific option | Command | Current support |
| --- | --- | --- |
| `--native-settings JSON_FILE` | `run` | Qoder: optional extra JSON settings merged with generated Hooks; the original file remains unchanged. OpenClaw: required existing native JSON configuration |
| `--native-profile PROFILE` | `run`, `install` | Hermes: required existing absolute profile for `run` and `install`. Qoder and OpenClaw reject it |
| `--native-state-dir DIRECTORY` | `run` | OpenClaw: required absolute native state directory. Qoder rejects it; this is not the AW service's `--state-dir` |

The shared command parser recognizes these adapter-specific options, but that
alone does not enable them for a framework. Pi, Claude SDK and AgentScope SDK
reject all these native override options. QwenPaw also rejects them and uses
`QWENPAW_WORKING_DIR`. Pass
supported AW options before `--`; arguments
after it are forwarded literally to the Agent. `install` accepts no Agent
arguments. Use `aw --help` to see the command syntax and current support.

In a source checkout, use `target/debug/aw` for `aw`. `aw hook` is an internal
callback generated by the launcher; users do not need to construct it. Service
records contain metadata without tool input/results, private Provider
configuration or raw stdout/stderr. A running service or completed invocation
does not prove native policy adoption. Killed or disabled native callbacks can
miss checks; this version does not provide final/protected execution or an OS
fallback.

## Containers and persistent data

Prepare AW binaries, supported framework versions, policy programs and default
configuration in the image. At runtime, supply the native profile and launch
the selected Agent through `aw run`; a Dockerfile cannot retain a live service
or a usable session binding. For Hermes, install the matching plugin into the
**actual runtime profile** before launch. A mount over an image directory may
hide files installed there during the image build; see Docker's
[mount behavior](https://docs.docker.com/engine/storage/bind-mounts/#bind-mounting-over-existing-data).

| Data | Container handling |
| --- | --- |
| AW binaries, scripts, fixed `aw.yaml` | Bake into the image or supply read-only; save editable configuration outside the disposable layer |
| `<state_dir>/journal/` | Persist when execution history must survive container replacement; audit is enabled and metadata-only in this version |
| Socket, locks, `launch-*` and session bindings | Runtime artifacts, not recoverable sessions; each service/Agent launch creates fresh runtime state |
| Native profiles, credentials, conversations and workspaces | Persist according to the selected framework's requirements; these are not AW audit data |
| sec-core backend data | Managed by sec-core independently of AW; see the [Preview backend setup](aw-preview.md#2-optionally-install-and-start-sec-core) |

AW always needs writable local state even when no history is retained across
container replacement. For durable history, set an absolute `daemon.state_dir`
on a local persistent filesystem that honors `fsync`. Keep `endpoint: auto` to
use its `aw.sock`. The directory must belong to the runtime UID with mode 0700;
its parent must also belong to that UID and must not be writable by other users.
Prepare volume ownership for the actual container UID before starting AW.
The default `auto` directory under `XDG_RUNTIME_DIR` or `/tmp` is temporary storage.

Plan the lifetime of long-running Gateway/App launches around this Preview's
limit of 1,024 distinct correlated native events per AW instance. With both
before and after enabled, each completed tool call consumes two entries. On
exhaustion, opening a new correlated event fails with `native_event_limit`
before any step script runs. This is an event-opening failure, not a Provider
result. The AW callback bridge converts the failure into the selected
framework's native response; a before callback with `on_error: block` requests
denial. Final tool behavior follows that framework's native Hook semantics.
Exit and relaunch the AW-managed Agent to create a new instance; starting another
native conversation does not reset this limit.

The current state directory contains both the journal and runtime artifacts.
If you persist the entire directory, graceful shutdown removes the owned socket,
but a forced container kill can leave it behind. Before reuse, confirm that its
old service is gone and remove only that stale `aw.sock`; do not delete live
endpoints or the journal. Do not share one state directory between independent
AW daemons. Arrange container shutdown to exit Agents, stop AW and wait for the
owned processes. A retained journal supports historical queries, not resuming
or replaying interrupted tools. Docker volumes survive container removal; their
deletion is a separate action described in the [volume lifecycle](https://docs.docker.com/engine/storage/volumes/#a-volumes-lifecycle).

## Common setup failures

| Symptom | Next check |
| --- | --- |
| Static validation passes, but launch fails | Check selected native version/entrypoint, executable paths and Provider private configuration; validation does not execute them |
| Unsafe state/profile path | Check runtime UID, directory permissions and the explicit native path; use a private directory owned by that user |
| Configuration changed but behavior did not | Exit old Agents, stop the old service with its original config/socket, then launch the edited file |
| Hook readiness fails | Read the native launch error; confirm the required profile/plugin and supported version before sending requests |
| No audit after a model response | Confirm a supported tool actually reached the callback; native permissions or a model refusal can prevent invocation |
| Existing socket after forced termination | Confirm the old service has exited, then remove its stale socket as described above |

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

Model-visible PostTool compression uses the optional [Tokenless Provider](aw-tokenless.md).

## Start a Pi SDK worker

For a high-code Pi application, use `adapter: pi` with the public Coding Agent/Core
SDKs 0.84.2 and Node.js >=22.19.0. AW owns a foreground SDK worker and connects
both tool Hook points. Install those SDK packages in your application using your
normal dependency workflow, and retain your application's model/auth settings.
The native Pi CLI is a separate entrypoint and is not launched by this adapter.

Expose your existing public factory and foreground task in an absolute `.mjs`
application module:

```javascript
export async function createSession({ sdk }) {
  return sdk.createAgentSession({
    cwd: process.cwd(),
    agentDir: "/absolute/existing-pi-agent-directory",
  });
}

export async function run(session, { args }) {
  await session.prompt(args.join(" "));
}
```

Copy the [Pi example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.pi.yaml),
set its `argv` to `[node, /absolute/application.mjs]`, and configure your before
and after policies. Its included commands are neutral. Then run:

```bash
cd src/aw
target/debug/aw validate --config /absolute/aw.pi.yaml
target/debug/aw run --config /absolute/aw.pi.yaml --agent pi -- 'Your task'
target/debug/aw status --config /absolute/aw.pi.yaml
target/debug/aw stop --config /absolute/aw.pi.yaml
```

`--agent` selects your configuration target; its `adapter` remains `pi`.
`--native-settings`, `--native-profile` and `--native-state-dir` are not Pi SDK
options. AW validates the installed SDK versions before binding, waits for the
factory and installed callbacks, and then reports `AW pi native hooks ready`.
The application task must stay in that foreground worker. For replacement
new/resume/fork/import sessions, use the `createSession` supplied to `run` so each
factory result gets the same Hook composition. Existing before denial/mutation
and after replacement remain authoritative; AW after observes the final result.
Blocked or invalid/pre-aborted tools have no after callback in Pi.

For Tokenless, use the Pi SDK target in the [shared optimization example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider-tokenless/examples/aw.yaml).
Keep security checks before `pre_tool` and observers/custom Hooks before the final
`post_tool` replacement. Retry reasons reach the model; only a fresh tool call can
execute the suggested command. PostTool replaces one successful text block while
preserving SDK metadata; report failures retain the original result. Raw native
Hooks cannot supply replacements. AW uses a fresh invocation UUID for pairing,
retains the original tool ID in `native`, and closes events on early denial.

See the [adapter contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/pi-adapter.md)
for exact callback semantics, identity, cancellation, capacity and reproducible
SDK acceptance. Exit Agents and stop the original service before changing policy;
retain/remove the audit state using the same local-service cleanup procedure.

## Start a Claude SDK worker

Use Claude Agent SDK 0.2.88 with its bundled CLI 2.1.161. The Adapter launches one
foreground Python script or `-m` module and requires the worker's public factory
to use `aw_claude.create_client(options)`. Model credentials remain SDK settings.
AW supplies this module on `PYTHONPATH` for the launched worker; it does not
inject hooks into an arbitrary SDK application automatically.

For the source example, prepare the selected Python environment and run from
`src/aw`. The MCP pin addresses a reproduced incompatibility with MCP 2.x:

```bash
python3 -m venv target/claude-sdk-venv
target/claude-sdk-venv/bin/pip install -r tests/claude-sdk-requirements.txt
```

Copy `crates/aw-service/examples/aw.claude-sdk.yaml` to your configuration and set
its Agent argv interpreter to the absolute `target/claude-sdk-venv/bin/python`
path. Keep the foreground example worker path or replace it with your script.
Its options factory should use this pattern after constructing its normal MCP
tools, permissions, resume settings and existing hooks:

```python
from aw_claude import create_client

async with create_client(options) as client:
    await client.query(prompt)
    async for message in client.receive_response():
        handle_message(message)
```

```bash
target/debug/aw run --config /absolute/aw.claude-sdk.yaml --agent claude -- 'Use echo to return hello.'
target/debug/aw status --config /absolute/aw.claude-sdk.yaml
target/debug/aw stop --config /absolute/aw.claude-sdk.yaml
```

Claude SDK event budgets must be 1..55,000 ms. Each `tool.before` or `tool.after`
route uses `events.<name>.budget_ms` when set, otherwise
`execution.default_event_budget_ms`. `aw run` enforces this adapter-specific
limit after static configuration validation.

The worker's options cwd must equal the launch cwd. Readiness is published after
the real SDK initialization handshake accepts callbacks. AW observes or denies
`PreToolUse`; `PostToolUse` and `PostToolUseFailure` both supply after observations.
Denied tools do not produce a synthetic after. Existing hook vetoes and native
permissions remain effective. Existing pre-hook `updatedInput` and permission
`updated_input` changes are denied, because concurrent callbacks cannot establish
that AW inspected the final arguments. When `tool.before` is active, nonempty
`PermissionRequest` matchers are rejected before initialization; use supported
`PreToolUse` or `can_use_tool` callbacks instead. Caller-specified
`permission_prompt_tool_name` is also rejected with `tool.before`; SDK-owned stdio
permissions for the wrapped `can_use_tool` remain supported. Caller `cli_path` overrides and missing
bundled binaries are rejected. The worker must not detach or start multiple
SDK clients/tasks within one launcher binding; Gunicorn and business deployments
need their own task lifecycle integration.

CLI 2.1.161 emits `PostToolUse` for SDK MCP `isError: true` responses and omits that
flag from callback data. After observation therefore does not classify every
business failure. Native Bash execution failure does use `PostToolUseFailure`.
See the [Adapter contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/claude-sdk-adapter.md)
for identity, cancellation and validation limits.

For model-free native adoption checks, build AW and use the real SDK/bundled CLI
against the local deterministic HTTP fixture. The test creates its own profile
and creates private UDS directories under the output directory. It removes them
after stopping owned services and rejects socket paths over 107 bytes:

```bash
target/claude-sdk-venv/bin/python tests/test_claude_sdk_native.py --aw target/debug/aw --output target/claude-sdk-native
```

Once the evidence and environment are no longer needed, remove only the created
`target/claude-sdk-native` and `target/claude-sdk-venv` directories. The AW policy
is a demo, not a production security rule; business deployment is separate from
these local native checks.

The options factory requires `setting_sources=[]`, no `settings` override and no
CLI plugins. `extra_args` allows only `strict-mcp-config: None`; the SDK native
`strict_mcp_config` option is also preserved. Model/authentication options are
unchanged. Applications which discover project skills through settings sources
need an explicit integration design before using this restricted first bridge.
Only one client can be created in each foreground worker.

The `claude-sdk` adapter supports this restricted foreground worker factory;
`aw install` does not provide persistent installation for it, and all `--native-*`
overrides are unsupported.

### Enable Bash PreTool and PostTool optimization

Use the [shared Tokenless configuration](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider-tokenless/examples/aw.yaml),
select its `claude` target, and replace the absolute executable, worker and state
paths. The source `claude-worker.py` example explicitly exposes and allows native
Bash in addition to its echo tool. Keep your application's own permission policy
when composing its factory. See the [Tokenless guide](aw-tokenless.md) for Provider
installation, RTK requirements and statistics settings.

PreTool blocks the original Bash attempt and returns retry arguments to the model;
the new call passes all policies and native permissions again. PostTool replaces
only completed, non-image, non-interrupted Bash stdout. Put completion observers
and custom steps before the single final `replace_result` step. They see the
original result; stderr and other native fields are retained. Failure/report keeps
the original result. MCP tools remain observable but are not compressed because
the native callback does not expose reliable business-error status.

After building AW, its Tokenless Provider and the current Tokenless Protocol v2
CLI, extend the native acceptance run (RTK must be available on PATH):

```bash
target/claude-sdk-venv/bin/python tests/test_claude_sdk_native.py --aw target/debug/aw --output target/claude-sdk-native --tokenless ../tokenless/target/debug/tokenless
```
