# Tokenless Quick Start

[中文版](../../../zh/token-saving/tokenless/QUICKSTART.md)

Install Tokenless, connect it to Claude Code, run one real task, and verify a
before/after Token record in about three minutes. Tokenless works in the
background, so your prompts and normal Agent workflow do not change.

Savings vary by workload. Tool-heavy tasks usually show the clearest result;
short or conversation-only tasks may show little change.

## 1. Install Tokenless and connect Claude Code

This quick path uses Claude Code as the example Agent:

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

If the anolisa CLI is already installed, start with `anolisa install
tokenless`. The PATH line is needed only when a fresh installation reports
that `~/.local/bin` is not available in the current Shell.

Using another Agent? Follow the matching setup path in
[Use another Agent](#use-another-agent); the remaining steps stay the same.
Most Agents use an `anolisa adapter enable` command, while OpenCode uses its
linked lifecycle script.

## 2. Run one real task

Restart Claude Code so it loads the adapter, then start a new session and run a
tool-heavy task. For example:

> Run the full test suite for this repository and summarize only the failures.

You do not need to mention Tokenless in the prompt.

## 3. Verify the saving

After Claude Code uses a Shell, API, or another supported tool, run:

```bash
tokenless stats list --limit 5
tokenless stats summary
```

Example output (values vary by workload):

```text
Showing 1 record(s):
================================================================================
[ID:42] 2026-08-12 10:20:30 | claude-code | Session:- | Tool:- | Chars:5120→2880(-2240) | Tokens:1280→720(-44%)

Tokenless Statistics Summary
============================================================
Total Records: 1

Character Savings:
  Before: 5120 chars
  After:  2880 chars
  Saved:  2240 chars (43.8%)

Token Savings:
  Before: 1280 tokens
  After:  720 tokens
  Saved:  560 tokens (43.8%)

Breakdown by Operation:
----------------------------------------
  compress-response: 1 records
    Chars: 5120 -> 2880 (-43.8%)
    Tokens: 1280 -> 720 (-43.8%)
```

You are done when `stats list` contains a record whose estimated Token count
decreases from before to after. To inspect exactly what changed, copy its ID:

```bash
tokenless stats diff <record-id>
```

For a visual view of savings over time, follow the
[AgentSight guide](../../agent-observability/agentsight/integrations.md#tokenless-token-savings).
When Tokenless and AgentSight run as the same user, the Dashboard reads local
Tokenless statistics without requiring SLS.

If no record appears, the content may not have passed through Tokenless or may
not have become shorter. Check the adapter and component health:

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

Then see
[No statistics appear after setup](troubleshooting.md#no-statistics-appear-after-enabling-the-adapter).

Token counts are estimates for content processed by Tokenless, not a direct
measurement of the model bill. Statistics and diffs may contain original tool
content; avoid sharing their output when it contains sensitive data. See
[Measuring savings](measuring-savings.md) and
[Configuration and data privacy](configuration-and-privacy.md) for details.

## Use another Agent

Scan the machine, then enable only the Agent you use:

```bash
anolisa adapter scan
```

| Agent | Setup |
|-------|-------|
| cosh / Copilot Shell | `anolisa adapter enable tokenless cosh` |
| OpenClaw | `anolisa adapter enable tokenless openclaw` |
| Hermes | `anolisa adapter enable tokenless hermes` |
| Qoder | `anolisa adapter enable tokenless qoder` |
| Claude Code | `anolisa adapter enable tokenless claude-code` |
| Codex | `anolisa adapter enable tokenless codex` |
| DeepSeek Harness (dsh) | `anolisa adapter enable tokenless dsh --profile <profile>` |
| OpenCode | `anolisa adapter enable tokenless opencode` |
| Qwen Code | `anolisa adapter enable tokenless qwencode` |
| QwenPaw | `anolisa adapter enable tokenless qwenpaw` |
| WorkBuddy | Lifecycle script (see below) |

Restart the Agent CLI or IDE after setting it up. OpenClaw also requires
`openclaw gateway restart`; if its security check rejects the plugin, follow
the [OpenClaw integration instructions](framework-integration.md#2-enable-one-adapter).
For DeepSeek Harness, `<profile>` is required and must match the name used by
`dsh --profile <profile>`; restart that profile after enabling the bundle.
To enable more than one profile, repeat `--profile` in the same command:

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

Every later enable or re-enable replaces the entire recorded profile set.
Include every profile that should retain Tokenless each time.

OpenCode is available through the same adapter command. The bundled lifecycle script remains an
alternative for npm installations that have no ANOLISA component record. WorkBuddy is not
registered with `anolisa adapter enable` in this release; use the bundled lifecycle script
described in the [WorkBuddy integration instructions](framework-integration.md#workbuddy).

## Optional: test compression without an Agent

Use this deterministic check when you want to confirm the standalone CLI
before enabling an adapter:

```bash
printf '%s\n' \
  '{"status":"ok","data":{"name":"demo","items":[1,2,3]},"debug":{"trace":"verbose"},"metadata":null}' \
  | tokenless compress-response

tokenless stats list --limit 1
```

The command returns valid JSON with `debug` and `metadata` omitted. Content
without removable fields is returned unchanged and is not recorded.

## Platform support

| Platform | anolisa CLI installation |
|----------|--------------------------|
| Linux x86_64/aarch64 | Supported |
| macOS Apple Silicon | Supported |
| macOS x86_64 | Not currently supported |
| Windows or Linux with musl, such as Alpine | Not currently supported |

This page covers installation with the anolisa CLI only. To build the
standalone CLI from source, see
[User manual · Build the standalone CLI from source](user-manual.md#build-the-standalone-cli-from-source).

## Next steps

- [Python SDK](sdk.md): framework-neutral and AgentScope layers with runnable examples
- [AgentScope SDK integration](sdk/agentscope.md): AgentScope 1.x, 2.x, and App attachment
- [Agent integration](framework-integration.md): product adapter activation
- [User manual](user-manual.md): behavior boundaries and documentation map
- [CLI reference](cli-reference.md): all subcommands and options
- [Measuring savings](measuring-savings.md): statistics, dual runs, and AgentSight/SLS
- [Configuration and data privacy](configuration-and-privacy.md): toggles, storage, and sensitive data
- [Troubleshooting](troubleshooting.md): common errors, upgrades, and uninstall
