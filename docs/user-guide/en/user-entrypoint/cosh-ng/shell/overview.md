# Interactive Terminal

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/overview.md)

`cosh` starts in Enhanced Assisted mode and keeps the shell's native prompt
appearance. Cosh may route natural-language input before bash or zsh executes
it. Press `Shift+Tab`
at an empty prompt for Enhanced Shell-only, or select Native at startup
when the session must have no Cosh hooks, observation, or insights.

Status symbols are off by default. Set `shell.status_symbols = true` in
`config.toml` (or `COSH_SHELL_STATUS_SYMBOLS=1` for one session) to publish a
`◇` (Assisted) or `◌` (Shell-only) status line when a prompt opens, control
returns from a card, or routing changes. It scrolls with terminal output.
Shell prompts start on the next line, keeping normal wrapping and cursor
movement; editing redraws do not add status lines. Native sessions have no
Cosh status line.

## A typical workflow

1. Change to the target directory and run `cosh`.
2. Run familiar commands normally.
3. Press `Shift+Tab` when ordinary input should remain Shell-only.
4. Describe a task in Assisted mode and review cards before side effects.
5. Use `/session status` before leaving a long-running investigation.

Useful starts:

```bash
cosh
cosh --shell zsh
cosh --resume
COSH_SHELL_INTEGRATION=native cosh
```

## How input is routed

| Input | Native | Enhanced Shell-only (`◌` if enabled) | Enhanced Assisted (`◇` if enabled) |
|---|---|---|---|
| `git status` | Runs in the Shell. | Runs in the Shell; an execution insight may follow. | Runs in the Shell; an execution insight may follow. |
| `hello` | The Shell normally reports a missing command. | The Shell normally reports a missing command. | The classifier evaluates it and currently leaves this ambiguous single word to the Shell. |
| `why did the last command fail?` | The Shell handles the text. | The Shell handles the text. | Starts an Agent request with recent terminal evidence. |
| `/session list` | The Shell handles the text. | The Shell handles the text. | Runs a Cosh control command. |
| Agent tool request | Unavailable. | Available after explicitly accepting an insight or Agent entry. | Runs or shows an approval card according to the approval mode. |

Native integration does not install Cosh `DEBUG`, `RETURN`, or `ERR` traps and
does not enable `extdebug`, `functrace`, or `errtrace`. Enhanced is the default;
select Native with `shell.integration = "native"` or
`COSH_SHELL_INTEGRATION=native`. Restart `cosh` to switch integrations.
`Shift+Tab` changes only the Enhanced routing substate, without restarting.

Approved Shell commands in enhanced integration stay in the foreground Shell,
so prompts, output, job control, and `Ctrl+C` remain usable. See
[Tool approval](approval.md) for the safety rules.

## Bash prompt compatibility

Enhanced combines user `PROMPT_COMMAND` hooks with Cosh's prompt hooks. User
hooks configured before integration keep their execution order under the
selected Bash version. Both Assisted and Shell-only use this integration.

The variable's representation and child environment have these limits:

- Bash 5.1 and newer use an array. An existing export attribute is retained,
  but Bash does not export array values. A previously exported scalar
  `PROMPT_COMMAND` therefore no longer reaches child processes, including
  when its original value was an empty string.
- Bash 4.3–5.0 use a combined scalar without the export attribute. This keeps
  Cosh's internal hook text out of child processes; the original user scalar
  is not exported either.

Configure prompt hooks in each interactive shell's startup files when child
shells should initialize them independently. Choose Native at startup when
the session must preserve Bash's own prompt-variable representation and
environment behavior. Native provides no Cosh hooks, observation, or insights;
switching Enhanced to Shell-only does not remove the limits above.

`--resume` always selects Enhanced, even with `COSH_SHELL_INTEGRATION=native`
or `shell.integration = "native"`. Omit `--resume` when Native behavior is required.

These limits apply to Bash `PROMPT_COMMAND`, not ordinary environment variables
or the interactive continuation prompt `PS2`.

## Sessions and proactive help

- Enhanced sessions are persisted by cosh-core and scoped to the workspace
  where cosh started. Recovery restores model-visible conversation context,
  not terminal processes or old terminal output. See
  [Session recovery](session-recovery.md).
- `smart` is the default analysis mode inside enhanced integration. Use
  [AI analysis](ai-analysis.md) to choose how much proactive failure help appears.
- `/help` is the source of truth for enhanced-mode commands; use
  [Interactive commands](interactive-mode.md) for a concise reference.

## Next steps

- [Tool approval](approval.md)
- [AI analysis](ai-analysis.md)
- [Session recovery](session-recovery.md)
- [Session compaction](session-compaction.md)
- [Skills](../core/skills.md)
- [MCP](../mcp.md)
- [Extensions](../core/extensions.md)
