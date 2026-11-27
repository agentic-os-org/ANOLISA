# Codex CLI

[中文版](../../../zh/aw/agents/codex.md)

AW launches local Codex CLI through `aw run` and connects its
`UserPromptSubmit`, `PreToolUse` and `PostToolUse` command Hooks. Codex keeps its
model settings and native permissions. AW evaluates configured
Provider steps in order within one command Hook per event.

Use the [sec-core example](../../../../../src/aw/crates/aw-service/examples/aw.codex-security.yaml)
as one `aw.yaml` for prompt, tool-input and tool-output checks. Set the absolute
paths to Codex, the sec-core Provider and CLI, and the running sec-core socket.
Install Codex and sign in separately. From the directory where
`aw.yaml` resides, run:

```bash
aw config validate --config aw.yaml
aw connect --config aw.yaml --agent codex
codex
# In Codex, run /hooks and trust the three exact AW command definitions; then exit.
aw run --config aw.yaml --agent codex
aw service status --config aw.yaml
aw service stop --config aw.yaml
```

`aw connect` adds stable definitions to `CODEX_HOME/hooks.json` (or
`~/.codex/hooks.json`) and preserves existing Hooks. It does not grant trust.
`aw run` records the reported Codex version and checks the enabled Hook inventory, trust, and the selected
AW policy before starting a session. A direct Codex launch leaves the installed
AW commands dormant. Reinstall and review trust if the AW executable path changes.

AW serializes its installers within one Codex home. When updating an existing
Hooks file, it retains the displaced file as `hooks.json.aw-backup`, including
late writes through an editor's open file descriptor. Every reinstall, including
an unchanged retry after interruption or a successful install, requires this
backup to be reconciled first. Stop native Hook editors, review the live file and
the backup, merge any user edits, then move the backup outside this reserved path.
Rerun installation and review native trust after editing Hook definitions.
Cancellation observed before publication leaves the live Hooks unchanged; once
publication commits, AW completes its sync and recovery checks.

For a business client using local app-server stdio, set `spec.agents.codex.argv` to
`[/absolute/path/to/codex, app-server]`. Have the client spawn
`aw run --config aw.yaml --agent codex` as its stdio child. The same AW Provider
steps apply to new threads using
the launch directory and native Codex configuration. This path passed native
prompt denial, tool denial and ordinary model-text replacement on 0.163.0 and
0.166.0. `aw run` admits only local stdio (`app-server` or
`--listen stdio://`), for a trusted fixed-config client. The guarded gateway launch below accepts a fixed set of model settings. AW sets Codex's
remote-control disabled marker for both the inventory probe and the launched
app-server. Codex 0.166.0 consumes this marker before resolving persisted
remote-control state; the launched server reports remote control disabled.

Each Codex AW instance retains at most 1024 cumulative event correlations,
including closed events. A tool's before and after Hooks consume two entries;
prompt Hooks share this capacity, so prompt checks reduce the 512-tool-pair upper
bound. Starting another Codex thread in the same app-server does not reset it.
At exhaustion, AW reports `native_event_limit`; `on_error: block` rejects further
prompt/tool operations and `withhold_result` withholds further results. A `report`
step continues according to its configured failure policy. Plan worker rotation
between completed tasks before the limit: close the stdio child, wait for `aw run`
to exit, then launch a new `aw run` instance. This releases the old correlations
and restores capacity while allowing the shared AW service to remain running.

On the plain stdio path the client must keep thread and turn cwd and configuration unchanged.
Codex accepts `thread/start.config` overrides that can disable Hooks.
AW passes plain stdio RPCs through to Codex. Use this path with a trusted client
that creates new threads under the launch configuration. If the worker syncs
`config.toml` between turns, keep Hooks enabled and the AW definitions intact.
Thread resume, fork, cwd/config overrides and configuration writes require a controlled RPC boundary
before they can carry the same security claim.

The example checks user text for prompt injection and PII before model dispatch.
For Bash tools, sec-core checks code and input PII before execution. The example
blocks unmapped tools until their native input fields are mapped and verified.
When `tool.before` is required, AW rejects other native PreToolUse Hooks at
launch. Codex can execute arguments rewritten by another Hook after AW checks
the original input. Without `required`, retained PreToolUse Hooks may rewrite
the executed arguments; AW attests only to the original input.
PostToolUse records completion. Codex 0.163.0 and 0.166.0 have been verified
with the example's final `scan_tool_output` step, which returns `replace_result`
for detected output PII in ordinary model-visible text. Its
`on_error: withhold_result` supplies fixed withheld text when the Provider
fails or the Hook result lacks a supported text projection; setting
`on_error: report` instead audits the failure and allows the original result
through. A bound PostToolUse
Hook still withholds if callback preparation fails, including inputs above the
1 MiB Hook limit, before Provider scanning. Admission rejects Code Mode and
other PostToolUse Hooks for this configuration. The example sets
`pii.output_mode: replace_result`; sec-core
uses one fixed withheld message for detected PII rather than a fieldwise edit.

This Hook behavior marks the native Hook as stopped. The reported CLI version
is recorded at launch. The fixed-thread gateway also requires 0.166.0. The replacement does not protect Code Mode, tool failures
without PostToolUse, prior result capture,
telemetry, terminal streaming, or a failed native Hook command. MCP structured
results become replacement text in the ordinary model request. Keep output PII
controls in the native tool or another enforcing boundary when full delivery
protection is required.

The admitted launch forms are a new local TUI, `codex exec` and local
`codex app-server` stdio. AW rejects other CLI entrypoints, remote listeners,
unrecognized startup configuration overrides and unknown arguments because they can change
the checked Hook inventory or session boundary. Put a command-line prompt after
Codex's `--` delimiter so it cannot be interpreted as a hidden or future subcommand.
For a TUI target use `aw run --config aw.yaml --agent codex -- -- "Check this project"`;
for an exec target use `argv: [/absolute/path/to/codex, exec]` and the same prompt
delimiter. Options such as `--ephemeral` go before Codex's delimiter. Native
Hooks may run concurrently with the AW Hook when `tool.before` is not required.
The native CLI's own permission
decision still applies. The [capability matrix](../../../../user-guide/en/aw/capabilities.md)
lists admitted effects; the [native matrix](../../../../../src/aw/tests/native-matrix.json)
records the local model acceptance scope.

## Local Responses Gateway for Codex 0.166.0

Use the AW binary and TypeScript SDK built from this revision. The guarded
launch binds one local stdio worker before its first thread. Set its
AW target to the following argv. Keeping the provider selector in the deployment
configuration makes an omitted gateway argument fail at startup.

```yaml
argv: [/absolute/path/to/codex, app-server, --listen, "stdio://", -c, 'model_provider="gateway"']
```

1. Create a private `CODEX_HOME` and choose the canonical workspace cwd before
   prewarming. Keep both fixed for the worker. Put deployment-approved sandbox,
   approval, shell environment, tool and MCP settings in its native `config.toml`.
   Protect the home, workspace configuration layers and gateway from tool writes.
2. Review the three AW command definitions on the deployment image, including
   executable path, matcher, synchronous execution and timeout. Save their native
   `hooks/list.currentHash` values as trusted deployment inputs keyed by
   `user_prompt_submit`, `pre_tool_use` and `post_tool_use`. For a fresh home where
   each AW handler occupies group 0, handler 0, pass the canonical home path
   (using `realpath` when `CODEX_HOME` is reached through a symlink, since Codex
   looks up trust keys by the physical `hooks.json` path), append
   `reviewedHookTrust(home, pins)` to the generated TOML, and run
   `aw connect --config aw.yaml --agent codex` under that home. This skips
   separate native trust discovery for each worker.
   The launch probe still verifies the effective inventory, trust and cwd. Review
   new pins when definitions change. Extra lifecycle Hooks require their own trust.
3. Start the authenticated loopback Responses gateway and pass its key in
   `LOCAL_CODEX_GATEWAY_KEY`. Change the client's spawn site to start AW with
   the following arguments. Use the same home and cwd for installation and launch.

```js
import { spawn } from "node:child_process";
import {
  gatewayArgs, reviewedHookTrust, forwardCodexResponses,
} from "@anolisa/aw-sdk/codex-gateway";

const gateway = { provider: "gateway", name: "Local Gateway", envKey: "LOCAL_CODEX_GATEWAY_KEY" };
const child = spawn(aw, [
  "run", "--config", awConfig, "--agent", "codex", "--", ...gatewayArgs(port, gateway),
], { cwd, env, stdio: ["pipe", "pipe", "pipe"] });
```

The five appended `-c` pairs select the provider name, literal
`http://127.0.0.1:PORT/v1` endpoint, environment key, Responses wire protocol
and disabled WebSockets. Set `gateway.provider` to the same ID as
`model_provider` in the AW target. IDs match `[a-z][a-z0-9_-]{0,63}`.
Use a nonempty display name without quotes or control characters. The bearer
variable name must match `[A-Z][A-Z0-9_]{0,63}` and be dedicated to the gateway.
Reserved names include `HOME`, `PATH`, and names starting with `LD_`, `CODEX_`,
`AW_`, `PYTHON` or `NODE_`. AW checks the same arguments in the inventory process.
The RPC relay accepts `initialize`, `initialized`, `thread/start`, text-only
`turn/start` and `turn/interrupt`. Turns must use a thread ID returned by this
worker. A new thread can select the launch cwd, model, fixed provider, ephemeral
flag, developer instructions, context window and reasoning effort. The relay
rejects resume, fork, dynamic tools, remote environments, Hook injection, config
writes and other overrides. Use a new worker when the workspace changes.
`aw instance prepare` requires a native registration or SDK factory. Codex 0.166.0 supports binding only at launch.

Remove the Bash MCP PostToolUse declaration from the controller's thread
configuration. AW owns the native result decision. At the authenticated model
gateway, pass each parsed Responses body through the same live credential
redactor used when resolving secrets. Put **both** direct Responses forwarding
and Chat conversion, including retries, inside the callback.

```js
return forwardCodexResponses(payload, text => redactor.text(text), (checked, counts) => {
  recordCredentialScan(counts);
  return forwardToConfiguredModel(checked);
});
```

The callback receives only the checked request and counts of scanned and changed
tool results. Unknown item types, structured tool outputs, indirect conversations
and scrubber failures stop forwarding. Return an error to Codex on rejection.
The AW Provider receives the original tool output before the gateway scrub.
The optional credential PreToolUse can be installed and reviewed at startup only
with a policy that permits native input rewriting. Required AW input checks reject
that peer. Shell `aggregatedOutput`, MCP `item/completed.result`, transcripts and
notification consumers can see original data before model egress. Protect each
consumer that stores or exports these fields.
