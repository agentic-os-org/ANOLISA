# Tool Approval

[中文版](../../../../zh/user-entrypoint/cosh-ng/shell/approval.md)

cosh may show an approval card before an Agent uses a guarded tool. Review the tool, its input, the risk, and any Hook warning before allowing the action.

## Choose an approval mode

Switch with `/mode approval <mode>` or set `shell.approval_mode`.

| Mode | Behavior |
|------|----------|
| `recommend` | Explain and suggest only; no tool calls are emitted. |
| `auto` | Default. Eligible read-only or low-risk tools can run automatically; risky, guarded, or external work asks first. |
| `trust` | Provider tool requests run automatically for this session after explicit confirmation. |

In `auto` mode, eligible simple read-only commands can suppress stderr with
`2>/dev/null` or `2>>/dev/null` without an approval card. Spaces after the
operator and quotes around `/dev/null` are supported, for example:

```sh
find /tmp -maxdepth 3 -name '*cosh*' 2>/dev/null
```

Stdout suppression (`>/dev/null`, `1>/dev/null`), mixed output routing,
and redirection to regular files still require approval. The command must
still pass the read-only and risk checks; stderr suppression does not make
a modifying command eligible. Stdout and the exit status remain available
in the execution result, including when the command fails.

These automatic commands run in the requesting directory with a controlled
environment. `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, and `TZ` come from the cosh
process; subsequent `export` changes in the interactive shell are not copied,
and `GIT_*` variables are not passed through. Time-zone, locale, and Git
results can therefore differ from the same command in that shell. Run commands
that need the shell's current environment directly in the foreground shell.

Enable trust mode with a second confirmation:

```text
/mode approval trust confirm
```

Trust mode is not a blanket bypass. Irrecoverable system-control commands such as `reboot`, `shutdown`, and `halt` still require an approval card, and high-risk requests cannot create a persistent trust key.

## Read and answer a card

Check the tool name, input preview, risk, and Hook warnings. Choose **Approve** or **Deny**; use **Details** when the preview is shortened. If requests are queued, the card shows the queue position.

When you approve a `shell` tool, cosh runs the command in the foreground bash or zsh. Its output and interactive prompts stay visible, and `Ctrl+C` can interrupt it. Approved foreground commands run one at a time.

If an approved command waits for password input, a pager, or plain terminal input, cosh can show a hint and interrupt it after 120 seconds by default. Set `shell.input_wait_timeout_secs = 0` to disable this timeout. Fullscreen TUIs and pipeline reads are exempt.

Approval decisions are kept in the runtime journal. When audit logging is enabled, a redacted copy is also available in the audit timeline; see the [audit guide](../cli/audit.md).

## Configuration

```toml
[shell]
approval_mode = "auto"
trusted_commands = ["ls", "cat", "echo"]
input_wait_timeout_secs = 120
```

`trusted_commands` matches exact trust keys, not arbitrary command substrings, and does not override the irrecoverable-command gate. See [Configuration](../configuration.md) for environment overrides.

Configuration and environment overrides also accept the legacy values
`balanced`, `suggest`, and `strict` as `recommend`. Invalid values fail closed
to `recommend`; `/mode` accepts only the three canonical names.
