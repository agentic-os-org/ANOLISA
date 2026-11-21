# AW

[中文版](README_zh.md)

AW provides unified Hook orchestration, managed MCP integration and hot policy updates for Agents and SDK applications.
One configuration composes security checks before tool execution, output compression afterward and custom rules across CLI Agents, native framework adapters and custom Python/Node.js applications.
Supported resident entrypoints can update policy after tasks drain while keeping the host process running.
Agents retain their models, interfaces and permissions. AW runs on Linux.

<a id="install-and-use"></a>
<a id="installation-and-first-run"></a>

## 5-minute quickstart

Install AW core (and optional Provider extensions) from your applicable distribution channel—such as OS packages when available, release/CI archives, or a source build—and verify integrity following the [installation guide](../../docs/user-guide/en/aw/installation.md).
Install and sign in to your Agent first, then initialize, validate and launch with the AW CLI.

The example below uses a signed-in Qoder CLI (`AW_PREFIX` is your AW installation prefix; `aw` and `qodercli` must be on `PATH`):

```bash
export AW_HOME="$HOME/aw"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$(command -v qodercli)"
aw config validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

In Qoder, ask it to read the current directory with a tool and follow its native permission prompts.
After the Agent exits, the local service stays reusable until you inspect and stop it:

```bash
aw service status --config "$AW_HOME/aw.yaml"
aw service stop --config "$AW_HOME/aw.yaml"
```

## Shared capabilities

| Capability | What you can do |
| --- | --- |
| Hook orchestration | Select rules by target and tool, compose `sec-core`, `Tokenless` and custom Providers, and share step ordering, execution deadlines and audit. |
| Managed MCP | Apply the same policies at supported actual tool-call boundaries, checking invocation identity and approved arguments. |
| Policy updates | Explicitly apply a new policy (`aw update`) to a supported idle instance so subsequent tasks use the new revision. |

Adapters connect native events and adopt supported effects; Providers implement policies.
AW core works independently. Install and enable Providers when you need security scanning (`sec-core`) or tool output compression (`Tokenless`).
Tool result replacement is limited to authorized slots, and native Agent permission checks retain precedence.

## Hook support matrix

✓ Declared with contract coverage; × unsupported. Generated from the [regression manifest](tests/regression-matrix.json). See [capability support](../../docs/user-guide/en/aw/capabilities.md) for native replacement limits, Skill checks, MCP, policy updates and [verified baselines](../../docs/developer-guide/en/aw/native-agents.md).

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.before` / `replace_input` | `tool.after` / `observe` | `tool.after` / `replace_result` | `input.submit` / `observe` | `input.submit` / `block` | `security.violation` / `observe` | `security.violation` / `block` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `codex` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | × | × |
| `deepseek-harness` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `openclaw` | ✓ | ✓ | × | ✓ | ✓ | × | × | × | × |
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `sdk` | ✓ | ✓ | × | ✓ | × | ✓ | ✓ | × | × |
<!-- aw-regression-matrix:end -->

<a id="hook-support"></a>
<a id="adapter-capability-coverage"></a>

## Dual-language Runtime SDKs

AW ships 6 independent SDK packages (2 core packages + 4 optional framework adapter packages) so applications keep native startup, authentication and scheduling:

| Language | Core package | Optional framework adapters |
| --- | --- | --- |
| Python (`>=3.11`) | `anolisa-aw-sdk` (`aw_sdk`, `aw_sdk.sources`) | `anolisa-aw-sdk-claude`, `anolisa-aw-sdk-qwenpaw` |
| TypeScript / Node.js (`>=22`) | `@anolisa/aw-sdk` (`@anolisa/aw-sdk`, `@anolisa/aw-sdk/sources`) | `@anolisa/aw-sdk-qoder`, `@anolisa/aw-sdk-pi` |

- **Custom Python or Node.js Agents (zero Rust Adapter)**: Install only the core package and instrument input/tool methods with `LanguageSource` (`aw_sdk.sources` / `@anolisa/aw-sdk/sources`).
- **Resident framework applications**: Install the matching optional adapter package, compile config-bound startup hints with `aw config descriptor`, start the application natively, and attach with `aw connect --config aw.yaml --sdk-pid PID`.

See the [Runtime SDK guide](../../docs/developer-guide/en/aw/deployment/native-sdk.md) and [Python SDK guide](../../docs/developer-guide/en/aw/deployment/python-sdk.md) for runnable examples and deployment descriptors.

## Provider protocol

External policy, safety, compression or checkpointing tools connect to AW through the `aw-provider/v1alpha1` stdio JSON protocol without linking Rust libraries or checking out ANOLISA:

- Implement three one-shot JSON methods over stdin/stdout: `describe`, `validate_config` and `invoke`.
- Wire any installed executable directly in `aw.yaml` via `providers.<id>.transport.argv`, or package it as an optional local extension bundle.
- Reference first-party implementations live under `providers/agent-sec-core` and `providers/tokenless`.

See [Provider development](../../docs/developer-guide/en/aw/providers/index.md) for a minimal standalone Provider skeleton and `aw.yaml` wiring, and [Provider protocol](../../docs/developer-guide/en/aw/providers/protocol.md) for schemas and admission rules.

## Documentation

| Task | Guide |
| --- | --- |
| Install and complete a first run | [Quickstart](../../docs/user-guide/en/aw/index.md) · [Installation](../../docs/user-guide/en/aw/installation.md) |
| Look up every subcommand and option | [CLI reference](../../docs/user-guide/en/aw/cli-reference.md) |
| Select an Agent, configure rules and update policy | [Configuration and launch](../../docs/user-guide/en/aw/configuration.md) |
| Compare Hook, Skill, MCP and policy-update support | [Capability matrix](../../docs/user-guide/en/aw/capabilities.md) |
| Enable output compression or security checks | [Tokenless](../../docs/user-guide/en/aw/tokenless.md) · [sec-core](../../docs/user-guide/en/aw/security.md) |
| Inspect runtime and audit records | [AgentSight](../../docs/user-guide/en/aw/observability.md) · [Operations](../../docs/user-guide/en/aw/operations.md) |
| Integrate SDKs, independent instances or custom Providers | [Developer guide](../../docs/developer-guide/en/aw/index.md) |

<a id="code-architecture"></a>
<a id="development-and-validation"></a>

The [architecture overview](docs/design/en/architecture.md) describes framework and Provider ownership and call paths.
See [Contributing](CONTRIBUTING.md) for local verification commands, starter contribution areas and CI gates.
