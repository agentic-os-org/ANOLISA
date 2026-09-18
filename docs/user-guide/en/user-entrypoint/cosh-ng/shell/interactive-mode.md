# Interactive Commands

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/interactive-mode.md)

Use this page to start `cosh` and control a running session. Run `/help` to see the exact commands supported by the installed version.

## Start `cosh`

| Command | Use |
|---|---|
| `cosh` | Start Enhanced Assisted (`◇ `) with Agent and slash-command routing. |
| `COSH_SHELL_INTEGRATION=native cosh` | Start Native without Cosh hooks, observation, or insights. |
| `cosh --shell zsh` | Select zsh explicitly. |
| `cosh --isolated` | Skip user rcfiles. |
| `cosh --login` | Start a login shell. |
| `cosh --resume [id]` | Open the session picker or resume the given session. |
| `cosh -c '<command>'` | Run one command through the shell and exit. |
| `cosh -- <program> [args...]` | Run a program directly and exit. |

If no shell is selected, `cosh` uses its configured or detected bash/zsh and falls back to bash.
Integration is selected at startup. Enhanced is the default. Set
`shell.integration = "native"` in the user config for persistent hook-free
sessions, or use the environment variable for one launch.

For command or redirected-stdin execution, place `--isolated` before shell-owned
options: `cosh --isolated -c '<command>'`. Bash skips startup files and removes
`BASH_ENV` and `ENV` from its environment. Bash login invocations (`--login`,
`-l`/`+l`, combined forms such as `-lc`, or login argv[0] such as `-cosh`) are
rejected before shell startup with status 2: Bash cannot disable `.bash_logout`
independently. Use a non-login invocation for isolated commands. Command and
script arguments that happen to contain login flags are preserved.

On this exec path, isolated Zsh accepts no shell-owned arguments; otherwise
Cosh returns status 2 before starting Zsh. Interactive TUI startup continues
to use its existing isolation handling.

## Input and editing

- Native integration sends every input byte to the foreground bash or zsh.
- Enhanced Assisted (`◇ `) routes Shell syntax to the foreground Shell and can
  turn a natural-language request into an Agent request.
- At an empty Enhanced prompt, `Shift+Tab` switches to Shell-only (`◌ `).
  Ordinary input, including a leading `/`, then goes to the Shell while
  post-command insights remain available. Press it again to restore Assisted.
- A leading `/` runs a Cosh control command only in Enhanced Assisted. In
  Native and Enhanced Shell-only it remains Shell input.
- `Shift+Enter` inserts a newline when supported. Multiline paste remains one submission.
- Up-arrow history includes shell input and slash commands. `Ctrl+C` cancels the active command or Agent request.

## Enhanced integration slash commands

| Command | Purpose |
|---|---|
| `/help` | Show the installed command set. |
| `/agent` | Compose a one-shot cosh-core request with optional Skill and workspace references. |
| `/health` | Run local health checks. |
| `/status` (`/about`) | Show runtime, provider, and session status. |
| `/stats [model\|tools]` | Show model identity or tool activity. |
| `/auth` | Choose or update provider authentication. |
| `/config language [auto\|en-US\|zh-CN]` | Inspect or set the UI language. |
| `/mode approval [recommend\|auto\|trust]` | Inspect or change tool approval. |
| `/mode analysis [smart\|auto\|manual]` | Inspect or change proactive analysis. |
| `/session ...` | Create, list, resume, clear, or compact sessions. |
| `/recommendations [on\|off\|status\|privacy\|clear]` | Manage local prompt recommendations. |
| `/hooks <command>` | Inspect Hook findings and trust state. |
| `/extensions <command>` | Manage extension packages and settings. |
| `/skills [list\|detail\|enable\|disable]` | Manage Skills. |
| `/mcp [list\|connect\|inspect\|refresh\|disconnect\|login\|logout]` | Manage MCP servers. |

Commands such as `/details`, `/audit`, and `/send-to-shell` appear only when the current card or run provides their required context. `/mcp login` requires the shell-based OAuth flow described by the MCP guide.

## Compose a one-shot Agent request

Run `/agent` when the configured runtime is cosh-core. The Agent Composer opens
as a multiline editor without changing how later shell input is routed. Enter
sends the request, `Shift+Enter` adds a line, and `Esc` cancels it and restores
the shell prompt.

Type `/` as the first token to browse public slash commands. Continue typing to
filter by prefix (`/ho` suggests `/hooks`); Up/Down selects a candidate and
scrolls through the six-row list. Tab replaces the command token and adds a
space for arguments. When a single-line draft contains only the command token,
Enter accepts and executes the selected candidate (the first candidate by
default). Drafts with arguments or multiple lines are submitted as written.
Esc cancels the Composer, including when the list is open.
Slash commands use the same local command handlers and confirmation cards as
at the shell prompt. A command submission ends the Composer: its interactive
card takes over input, or the shell prompt returns when the command finishes.
Unknown command names display a local error instead of starting an Agent turn.

`/skill:<name>` still selects a Skill for an Agent request; `/skills` manages
Skills. Existing absolute paths such as `/tmp` and `/etc`, and paths containing
another slash such as `/tmp/file`, remain Agent text. Bare `/` opens the command
menu; exact registered command names take precedence over same-named paths.
Prefix a request with `??` to discuss a slash command literally,
for example `?? /help explain this command`. Commands in later tokens do not
activate the menu. Tabs and newlines inside bracketed paste remain text.
Ordinary shell prompts retain their native path completion.

The first token may select one Skill, and any later whitespace-separated token
that starts with `@` requests a file or directory from the current workspace:

```text
/skill:repo-review inspect @Cargo.toml @src
```

- `/skill:<name>` must be the first token. The selected Skill is invoked before
  other Agent tools.
- `@path` must name an existing file or directory inside the workspace. Absolute
  paths, parent traversal, workspace-escaping symlinks, and unsupported paths
  are rejected.
- A submission accepts at most 16 valid references. Directory references are
  non-recursive unless the request explicitly asks for traversal.
- cosh-ng sends only validated path metadata. It does not read or embed file
  contents while building the request.
- Rejected references are shown with their path and reason before the Agent
  turn starts, and are omitted from its structured reference context.

Plain text without a Skill or references is also valid. Other provider runtimes
leave `/agent` unavailable; an enhanced session can still use an ordinary
natural-language request or multiline input instead.

For approval behavior, see [Tool approval](approval.md). For proactive failure help, see [AI analysis](ai-analysis.md).
