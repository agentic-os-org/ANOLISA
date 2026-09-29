# Code Scanner Hook Configuration

Code Scanner hooks inspect shell or code tool calls before execution and reuse each Agent host's existing hook interaction model. Environment variables select existing behavior; they do not add approval or blocking responses that the host plugin did not already use.

## Installation

```bash
# Recommended (system mode required)
sudo anolisa --install-mode system install sec-core

# Alternative for Alinux systems with the YUM repository configured
sudo yum install agent-sec-core

# Source build for developers
cd src/agent-sec-core
make build-cli
```

Install or deploy the adapter for the Agent you use as described in the [AgentSecCore quick start](QUICKSTART.md).

## V2 CLI Interface

In the V2 RPM, `agent-sec-cli scan-code` is a Rust client for an already-running
`agent-sec-daemon`. The deployment supplies the daemon endpoint through
`AGENT_SEC_DAEMON_SOCKET`; an absolute `--socket` value overrides that
variable. The CLI never starts a daemon and does not fall back to the V1 Python
implementation.

```bash
export AGENT_SEC_DAEMON_SOCKET=/run/agent-sec-core/daemon.sock

# Scan Bash (default language) or Python source.
agent-sec-cli scan-code --code 'rm -rf /tmp/test'
agent-sec-cli --socket /run/agent-sec-core/daemon.sock \
  scan-code --code 'import os; os.system("rm -rf /")' --language python
```

The V2 scanner currently supports the embedded regex rules only. `--mode llm`
remains accepted for compatibility but returns the parseable error result
`LLM model not available`; it does not contact Ollama. V2 does not yet accept
`--trace-context` or write code-scan telemetry, so Agent hooks that depend on
those features remain deferred until their migration is complete.

## Environment Variables

| Agent plugin | `CODE_SCANNER_HOOK_ENABLED` | `CODE_SCANNER_MODE` | `CODE_SCANNER_TIMEOUT` |
|---|---|---|---|
| Qoder | `true` / `false` | `observe`, `ask`, `block` | Supported; default 10 seconds |
| Qwen Code | `true` / `false` | `observe`, `ask`, `block` | Supported; default 10 seconds |
| Codex | `true` / `false` | `observe`, `block` | Supported; default 10 seconds |
| Cosh | `true` / `false` | `ask` only | Not supported; fixed at 10 seconds |
| Hermes | `true` / `false` | `observe`, `block` | Not supported; uses capability `timeout` |
| OpenClaw | `true` / `false` | `observe`, `ask`, `block` | Not supported; fixed at 10 seconds |

`CODE_SCANNER_HOOK_ENABLED=false` skips hook input processing and CLI invocation. On Hermes and OpenClaw, a valid boolean environment value overrides capability `enabled`; an invalid value is treated as unset and falls back to capability configuration.

`CODE_SCANNER_MODE` controls how a plugin handles scanner `warn` and `deny` verdicts with findings:

- `observe` scans and audits while allowing the tool call.
- `ask` uses the host's existing approval interaction.
- `block` uses the host's existing deny or block interaction.

Compatibility aliases are normalized before host capability checks: `debug` maps to `observe`, and `deny` maps to `block`. `warn`, invalid values, and modes unsupported by that host are treated as unset; these configuration diagnostics never enter stdout, system messages, or other HookOutput. Standalone scripts write bounded diagnostics to stderr, while Hermes/OpenClaw capabilities write them to the host logger.

Consequently, Cosh keeps its fixed `ask` response when given `observe` or `block`; Codex and Hermes ignore `ask`; OpenClaw supports `observe`, `ask`, and `block`, with `deny` normalized to `block`. Unsupported modes use the same default or native configuration the plugin would use if `CODE_SCANNER_MODE` were absent.

## Native Configuration Precedence

Hermes preserves `[capabilities.code-scan]` configuration:

```toml
[capabilities.code-scan]
enabled = true
timeout = 10
enable_block = false
```

A supported `CODE_SCANNER_MODE` overrides `enable_block`; otherwise `enable_block=true` selects block and `false` selects observe.

OpenClaw preserves `capabilities["scan-code"].enabled` and `codeScanRequireApproval`. A supported `CODE_SCANNER_MODE` overrides `codeScanRequireApproval`; otherwise `true` selects ask and `false` selects observe. In `ask` mode, ordinary findings return `requireApproval`; in `block` mode, ordinary findings return `{ block: true, blockReason }`.

## Examples

```bash
# Qoder or Qwen Code: request approval
CODE_SCANNER_MODE=ask qoder
CODE_SCANNER_MODE=ask qwen

# Codex: block scanner warn and deny findings
CODE_SCANNER_MODE=block codex

# Disable the hook completely
CODE_SCANNER_HOOK_ENABLED=false codex
```

For managed services, inject these variables into the Agent process environment and restart the service. Do not add `CODE_SCANNER_TIMEOUT` for Cosh, Hermes, or OpenClaw because those adapters do not consume it.

## Failure and Safety Semantics

CLI startup failures, timeouts, nonzero exits, invalid JSON, and unknown verdicts fail open. Invalid or unsupported configuration is equivalent to an unset variable.

Hermes and OpenClaw retain their existing self-protect findings, which force block when a tool call attempts to disable the security plugin. This is a fixed safety exception, not an additional configurable MODE. Disabling the entire hook skips scanning, including self-protect checks.

## Hook MODE vs Scanner Engine

`CODE_SCANNER_MODE` controls the host hook response. It does not select the scanning engine. V2 currently runs embedded regex rules only; `--mode llm` is accepted for compatibility and returns `LLM model not available`.
