# Native Codex CLI adapter

[中文版](codex-cli-adapter_zh.md)

Codex owns model configuration, scheduling, sessions, tool execution and native
permissions. AW integrates through the command Hooks in Codex CLI 0.162.1 and
retains its existing launcher, Provider Host and service lifecycle.

## Ownership after the workspace split

| Owner | Codex integration responsibility |
| --- | --- |
| `aw-adapter` | Capability catalog, argument/version checks, hook inventory and registration, native normalization and reply interpretation |
| `aw-service` | CLI parsing, private binding/chain files, ordered callback execution, RPC, event budget and closure, signals and instance cleanup |
| `aw-host` / `aw-provider` | Existing Provider preparation, invocation and protocol admission |
| Independent `providers/` workspaces | Concrete backend implementations; no Codex-specific implementation is added here |

`crates/aw-adapter/src/adapters/codex/` contains native installation and preflight;
`crates/aw-adapter/native/codex/probe.py` is its packaged inventory bridge.
`NativeContext` supplies only target, instance identity and cwd to normalization.
Typed launch/install options and caller-owned `LaunchFiles` keep CLI parsing and
resource ownership in the service. Probes receive the caller's cancellation flag;
they do not install signal handlers.

The stable `aw codex-hook` command dispatches to
`crates/aw-service/src/cli/hook_chain.rs`. It validates private registration and
binding files, invokes the existing `aw hook` transport in order and closes events.
Native veto interpretation delegates back to `aw-adapter::callback`; no service
RPC, socket, complete binding or concrete Provider dependency enters `aw-adapter`.
Independent Provider releases and package installation are outside this change.

## Registration and launch

`aw install` adds one stable command for each of `PreToolUse` and `PostToolUse`
to the native home `hooks.json`. It preserves unrelated definitions and does not
grant trust. Users review the exact definitions through Codex `/hooks`; changing
the AW executable path requires a new native trust review. A direct Codex launch
has no AW binding, so the installed commands return neutral responses.

Before creating a binding, AW checks the CLI version, admitted arguments and
the public `hooks/list` inventory from a short-lived `codex app-server` process.
`config/read` verifies that Hooks are enabled. This read-only probe creates no
thread and never edits the native trust store. Exactly one enabled, synchronous,
trusted AW handler must exist for each configured event. Its matcher is `.*`
and timeout is 60 seconds. An explicit native trust-bypass flag is recognized
only as an option, never when it appears in a prompt after `--`.

New local TUI launches receive `--no-daemon`; local `exec` already runs in the
foreground. Both inherit native configuration and receive a private chain-file
path through `AW_CODEX_CONFIG`. Config/profile overrides, worktrees, cwd changes,
resume/fork and remote entrypoints are rejected so the checked Hook inventory
matches the launch. The common launcher owns processes, signals and binding cleanup.

## Events and effects

Codex can run matching command Hooks concurrently. AW therefore registers one
handler per event and serializes its own configured steps inside that handler.
Independent user Hooks remain under native scheduling. The chain verifies its
owned binding, step set, failure actions and budget before invoking the existing
`aw hook` callback path. One shared event budget is limited to 55 seconds, leaving
time for bounded callback transport and event closure within the native timeout.

The native `session_id` and `tool_use_id` correlate events. Normalization requires
the expected event, launch cwd, nonempty identifiers and object-valued tool input.
The original native payload and result are retained for Providers and audit policy.

| Event | Adopted effects | Native response |
| --- | --- | --- |
| `tool.before` | `observe`, `block` | Neutral `{}` or exit 2 with denial text on stderr |
| `tool.after` | `observe` | Neutral `{}`, retaining the original tool result |

No reply emits `allow`, rewrites input or overrides native authorization. Security
denial stops later AW steps. Tokenless retry suggestions deny the original call;
the fresh call is checked again. `replace_result` is rejected before Provider
discovery: Codex PostTool blocking feedback is not a normal successful-result
replacement contract. Boolean Hooks and full Providers are supported; raw native
Hook steps remain in Codex configuration, outside the AW chain.

The callback closes skipped event steps after denial or transport failure. Agent
exit and interruption release the binding and temporary launch files; the shared
AW daemon remains available until explicitly stopped. Installed definitions remain
for later sessions. Rollback removes only this executable's two AW definitions and
restores the previous AW configuration/binary.

## Evidence and limits

Daily contracts cover install preservation/idempotence, ordered callbacks, native
failure preservation, rejection of unsupported/untrusted/disabled definitions,
signal cleanup and shared security/Tokenless regressions. Matrix version 2 requires
every capability cell to declare runnable support or admission-rejection coverage;
unsupported replacement cannot silently disappear from the regression contract.

Nightly installs the pinned npm CLI and runs `tests/test_codex_native.py`. Its local
Responses server instructs the real runtime to execute or deny real tools. Exact
case reports cover trust refusal, disabled Hooks, allow/deny, unavailable policy,
fresh-call retry, an independent native denial, observer failure and long-command
completion. Tests use ephemeral sessions, disposable homes and no cloud keys.
Explicit native trust bypass and `danger-full-access` are fixture settings, never
defaults inserted by AW.

This evidence does not certify TUI trust interaction, hosted models, sandbox
enforcement, MCP or code mode. Hosted tools and input sent to a running process via
`write_stdin` have no new AW pre-tool authorization. Long-command completion keeps
the original tool identity. A native outer Hook crash/timeout is distinct from an
AW step failure and cannot be treated as an OS enforcement guarantee.

The integration contract is the [official Hooks reference](https://learn.chatgpt.com/docs/hooks)
and the public app-server schema emitted by the pinned CLI. The
[user guide](../../../../docs/user-guide/en/user-entrypoint/aw/configuration.md#codex)
documents installation and supported launch options.
