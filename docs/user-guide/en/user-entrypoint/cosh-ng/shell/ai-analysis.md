# AI Analysis

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/ai-analysis.md)

Enhanced integration can review command failures and useful diagnostic output,
then suggest a next step or start an Agent analysis. Both Assisted and
Shell-only can provide post-command insights. Only Assisted performs
pre-command natural-language routing. Native has no command events, insights,
or Agent request routing.

## Choose a mode

Enhanced is the default integration. Set the analysis mode at runtime with
`/mode analysis <mode>` or persist it in `shell.analysis_mode`.

| Mode | Behavior |
|------|----------|
| `smart` | Default. Evaluate failures and diagnostic output, then show useful insights for review. |
| `auto` | Automatically start analysis only for a narrow set of high-confidence failures; other cases remain suggestions. |
| `manual` | Disable proactive suggestions, failure insights, automatic analysis, and personalized prompt recommendations. Request analysis explicitly when needed. |

Examples:

```text
/mode analysis smart
/mode analysis auto
/mode analysis manual
```

## What to expect

- A failed command does not always start an Agent request. cosh first checks whether the failure is actionable and the available evidence is reliable.
- A suggestion or action card lets you decide whether to analyze; choose **Skip** to leave the command result unchanged.
- Analysis uses the command, exit status, and a bounded output excerpt. The result is streamed in the terminal.
- Press `Ctrl+C` to cancel an analysis that is in progress.

Configure the default mode with:

```toml
[shell]
analysis_mode = "smart"
```

See [Interactive commands](interactive-mode.md) for the other slash commands and [Configuration](../configuration.md) for environment overrides.
