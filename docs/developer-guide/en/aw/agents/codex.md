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
aw validate --config aw.yaml
aw install --config aw.yaml --agent codex
codex
# In Codex, run /hooks and trust the three exact AW command definitions; then exit.
aw run --config aw.yaml --agent codex
aw status --config aw.yaml
aw stop --config aw.yaml
```

`aw install` adds stable definitions to `CODEX_HOME/hooks.json` (or
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
fails or the Hook result lacks a supported text projection. A bound PostToolUse
Hook also withholds if callback preparation fails, including inputs above the
1 MiB Hook limit, before Provider scanning. Admission rejects Code Mode and
other PostToolUse Hooks for this configuration. The example sets
`pii.output_mode: replace_result`; sec-core
uses one fixed withheld message for detected PII rather than a fieldwise edit.

This Hook behavior marks the native Hook as stopped. The reported CLI version
is evidence; launch admission checks the Hook configuration and trust without
a release allowlist. The replacement does not protect Code Mode, tool failures
without PostToolUse, prior result capture,
telemetry, terminal streaming, or a failed native Hook command. MCP structured
results become replacement text in the ordinary model request. Keep output PII
controls in the native tool or another enforcing boundary when full delivery
protection is required.

The admitted launch forms are a new local TUI or `codex exec`. AW rejects
resume, fork, remote modes, cwd/profile/config overrides and unknown arguments
because they can change the checked Hook inventory or session boundary. Put a command-line prompt after
Codex's `--` delimiter so it cannot be interpreted as a hidden or future subcommand.
For a TUI target use `aw run --config aw.yaml --agent codex -- -- "Check this project"`;
for an exec target use `argv: [/absolute/path/to/codex, exec]` and the same prompt
delimiter. Options such as `--ephemeral` go before Codex's delimiter. Native
Hooks may run concurrently with the AW Hook when `tool.before` is not required.
The native CLI's own permission
decision still applies. The [capability matrix](../../../../user-guide/en/aw/capabilities.md)
lists admitted effects; the [native matrix](../../../../../src/aw/tests/native-matrix.json)
records the local model acceptance scope.
