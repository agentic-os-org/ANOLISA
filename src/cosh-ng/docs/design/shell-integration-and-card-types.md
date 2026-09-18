# Shell Integration and Typed Cards

[中文版](shell-integration-and-card-types_zh.md)

## Status

Implemented for an explicit hook-free Native startup, Enhanced Assisted as the
default, typed input ownership, inline Agent input, and same-process routing
switching inside Enhanced sessions. Direct Exec remains an internal reserved
type with no user entry point or visible state.

## Decision

`cosh` starts with `ShellIntegration::Enhanced` in the Assisted routing
substate. This preserves the pre-change product behavior: marker and OSC
command events, implicit natural-language classification, slash commands, and
Agent handoff remain available by default.

Users may select `ShellIntegration::Native` at startup. The child bash or zsh
then owns all ordinary input and loads its normal startup files. Cosh does not
generate a marker rcfile, install a `DEBUG` trap, change `PROMPT_COMMAND`,
enable `extdebug`, `functrace`, or `errtrace`, expose a marker token, observe
commands, or provide post-command insights.

```toml
[shell]
integration = "native"
```

```bash
COSH_SHELL_INTEGRATION=native cosh-shell
```

Invalid integration values reject startup with a visible error. Integration is
fixed for the life of the child Shell because enabling Enhanced inside a
running native Shell would require injecting state into that process.

Within an Enhanced session, `Shift+Tab` at an empty main prompt toggles AI
routing without restarting the child Shell. The Shell process, working
directory, variables, functions, and jobs remain intact. This is a routing
substate of Enhanced, not a conversion to hook-free Native integration: OSC
markers remain installed so Cosh can prove prompt ownership and switch back
safely.

## Input Ownership and Visible State

Symbols primarily describe who owns editable input before Enter, not output
that has already been produced. `InputOwner` stores this state; renderers never
infer routing from the first character of user text.

| Symbol | State | Owner | Behavior |
|---|---|---|---|
| None | Native | Child Shell | Every byte goes directly to the PTY. Cosh does not decorate the user's original prompt or observe command events. |
| `◌` | Enhanced Shell-only | Child Shell, observed by Cosh | Ordinary input, including `hello`, `/`, and `??`, remains Shell input. Enhanced marker integration stays loaded so post-command insights and safe switching remain available. |
| `◇` | Enhanced Assisted | Shell executes, Cosh may route | Cosh may observe, classify, or route the submitted line before Shell execution. |
| `◆` | Agent | Agent runtime | `/agent` opens a borderless inline Composer that continuously shows `◆ ` before editable text. Ordinary text, including `ls`, is an Agent request; leading slash controls dispatch locally. |
| `/` | Cosh Command | Cosh control plane | Explicit slash command, intercepted only in Enhanced Assisted. |

`◇` and `◌` occupy a separate outer-terminal status line before PS1/PROMPT.
The Enhanced hook's `prompt_ready` boundary publishes that line. Returning
from an Agent or panel and changing routing also publish the current state;
ordinary candidate, ghost, and authenticated slash-guard redraws repaint only
the original prompt and input. These redraws must not append another status
line. Status lines scroll with output, and an explicit routing change leaves
the earlier state in terminal history rather than guessing its screen row.

PS1/PROMPT and PTY dimensions remain unchanged. The child Shell starts its
prompt at column zero, so its own Readline/ZLE model accounts for every cell
of the prompt and input, including ANSI, CJK, combining and multiline text.
The status is not a reserved terminal row: a Shell-owned clear-screen/redraw
may remove it until the next publication or control return.

Bash's existing private-history submission guard still inserts one leading
blank when accepting a cursor-edited draft whose mirror cannot prove it is
non-secret. This is an accept-line display change, separate from editable
geometry; argument bytes and the privacy guard remain intact.

At an empty Enhanced main prompt, `Shift+Tab` publishes a `◌` status line and
disables Cosh input interception. Pressing it again publishes `◇` and restores
routing, without submitting an empty command or restarting the child Shell.
A non-empty
Shell line receives the key sequence unchanged, and an active prompt ghost or
card keeps its existing `Shift+Tab` behavior. This prompt-boundary gate keeps
the shortcut out of PS2, heredocs, foreground programs, and full-screen apps.

`DirectExec` and its `▶` symbol are reserved in the internal type model for a
possible future structured-`argv` executor. Because no executor or user-facing
entry exists, `▶` is not part of the current visible input states.

`/mode analysis` values `manual`, `smart`, and `auto` govern background failure
analysis and suggestion policy. They do not change the current input owner;
input ownership and background analysis are orthogonal states.

Native input bypasses candidate buffering, prompt ghosts, slash routing, and
card capture. Terminal control such as signals, resizing, and EOF still follows
the PTY lifecycle.

## Composer Command Submission

The Composer capture explicitly carries its Agent input origin. Command
candidates reuse public registry names, deduplicate by name, and retain the
complete set while rendering the selected six-row window. The input thread
computes slash candidates synchronously from the live editor text and cursor,
so Tab or Enter in the same read chunk cannot accept a stale candidate. Enter
reuses completion before submission only for a single-line, single-token command
draft, emitting the updated editor snapshot before the submit event. Arguments
and multiline drafts retain their text. Runtime path
and Skill completions must match their text and cursor snapshot before acceptance.

The capture and submit event retain the Composer's workspace snapshot. The input
bridge attaches it to the slash intercept's cwd, independent of ShellReady timing
or the global prompt cwd cache. The submit event carries a slash routing flag;
the input bridge emits exactly one slash or Agent intercept. Slash submissions
leave no pending Composer request metadata. Existing slash/control consumers own
parsing, confirmation, execution, and prompt restoration. Rendering never executes commands or reopens
the Composer over a subsequent card. Ordinary drafts and shell path completion
keep their existing routing.

## Output Event Cards

Output identity is stored as `CardKind`. Output symbols describe event types;
they do not participate in input routing or grant permission.

| Symbol | Event | Contract |
|---|---|---|
| None | Agent Response | The title and frame already identify an Agent response, so the input-state `◆` is not repeated. |
| `/` | Slash Command | Cosh control-plane result. |
| `*` | Tool Call | Structured Agent tool invocation. |
| `!` | Permission | System-created request bound to a concrete run, request, tool use, tool name, and input. |
| `·` | System | Read-only status or notice. |

The current UI renders event symbols for slash panels, tool invocations,
permission cards, and system notices. Agent responses keep their framed title
without `◆`. Shell output remains the native terminal stream.

Permission cards can be constructed only from a structured
`ToolPermissionRequest`. Text such as `! allow` remains content in its existing
card and cannot grant permission. Output beginning with any card symbol is not
recursively interpreted.

## Effect on Related Issues

Native integration provides the clean architectural boundary requested by
#2687: a user can choose a session where marker options and traps do not exist,
instead of hiding their observable state. Enhanced v2 also removes the global
`DEBUG` trap and no longer forces `extdebug`, `functrace`, or `errtrace`. It uses
bounded prompt, command-not-found, and PTY integration points while preserving
the user's trap definitions and option state, which closes the observable
contract in #2687 without claiming that Enhanced is injection-free. Enhanced
Assisted remains the default to preserve existing product semantics.

The remaining Enhanced integration surface is explicit: `PS0`,
`PROMPT_COMMAND`, `_cosh_*` helpers, `command_not_found_handle`, and scoped
`COSH_*` state. Native remains the strict zero-injection choice. Xtrace output
tracked by #2683 and exit-status or signal correctness tracked by #2541 remain
separate contracts.

## Known Limits

- Changing `shell.integration` still requires a new `cosh-shell` session.
- `Shift+Tab` switches only the routing substate of an Enhanced session; it
  cannot add Enhanced hooks to a Native child Shell.
- Native integration intentionally has no implicit natural-language routing,
  slash interception, command-boundary ledger, marker handoff, or insights.
- The current Native session has no safe in-terminal Agent hotkey or panel
  because prompt ownership cannot be proven without additional integration.
- Direct Exec has no user entry or rendered input state.
- Enhanced integration is bounded rather than injection-free; users requiring
  no prompt, helper, environment, or command-not-found integration must start
  a Native session.
- `_COSH_*`, `_cosh_*`, and scoped `COSH_*` state are reserved implementation
  namespaces used by Enhanced integration. Modifying, unsetting, or marking
  these variables as `readonly` produces undefined behavior; use Native mode
  if strict isolation from these names is required.
