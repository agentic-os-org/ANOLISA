# AW

[中文版](README_zh.md)

AW provides unified Hook orchestration, managed MCP integration and hot policy updates for Agents and SDK applications.
One configuration composes security checks before tool execution, output compression afterward and custom rules.
Supported resident entrypoints can update policy after tasks drain while keeping the host process running.
Agents retain their models, interfaces and permissions. AW is available as a Linux Preview.

## Shared capabilities

| Capability | What you can do |
| --- | --- |
| Hook orchestration | Select rules by target and tool, compose sec-core, Tokenless and custom Hooks, and share step ordering, execution deadlines and audit. |
| Managed MCP | Apply the same policies at supported actual tool-call boundaries, checking invocation identity and approved arguments. |
| Policy updates | Explicitly apply a new policy to a supported idle instance so subsequent tasks use the new revision. |

Adapters connect native events and adopt supported effects; Providers implement policies.
Core works independently. Install and enable the corresponding Providers for security checks and compression.
Tool result replacement is limited to authorized slots, and native Agent permission checks retain precedence.

## Hook support matrix

✓ Declared with contract coverage; × unsupported. Generated from the [regression manifest](tests/regression-matrix.json). See [capability support](../../docs/user-guide/en/aw/capabilities.md) for native replacement limits, MCP and policy updates.

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.after` / `observe` | `tool.after` / `replace_result` |
| --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ |
| `openclaw` | ✓ | ✓ | ✓ | ✓ |
| `pi` | ✓ | ✓ | ✓ | ✓ |
| `qoder` | ✓ | ✓ | ✓ | ✓ |
| `qoder-sdk` | ✓ | ✓ | ✓ | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->

<a id="install-and-use"></a>

## Installation and first run

Get and install a Preview package using the [installation guide](../../docs/user-guide/en/aw/installation.md).
AW currently uses standalone packages; `anolisa install aw` and RPM installation are not available yet.
Install and sign in to your Agent first, then generate configuration with `aw config init`.

The example below uses an installed, signed-in Qoder. `AW_PREFIX` is the installation directory
selected in the guide; both `qodercli` and `aw` must be on `PATH`.

```bash
export AW_HOME="$HOME/aw"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$(command -v qodercli)"
aw validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

In Qoder, ask it to read the current directory with a tool and follow its native permission prompts.
The local service remains reusable after the Agent exits. Inspect and stop it when finished.

```bash
aw status --config "$AW_HOME/aw.yaml"
aw stop --config "$AW_HOME/aw.yaml"
```

<a id="hook-support"></a>
<a id="adapter-capability-coverage"></a>

## Supported entrypoints

AW currently integrates Qoder CLI, Qoder SDK, OpenClaw, Hermes, QwenPaw, Pi SDK,
Claude SDK and AgentScope SDK. CLI integrations use native Hooks or plugins;
SDK applications explicitly use managed factories.
[Qoder SDK integration](../../docs/developer-guide/en/aw/agents/qoder-sdk.md) also covers pinned Bash consumers, Worker imports and declarative role assets.
The [capability matrix](../../docs/user-guide/en/aw/capabilities.md) compares result handling,
MCP and policy updates by Adapter and launch mode, with links to supported versions and setup.

## Documentation

| Task | Guide |
| --- | --- |
| Install and complete a first run | [AW user manual](../../docs/user-guide/en/aw/index.md) |
| Select an Agent, configure rules and update policy | [Configuration and launch](../../docs/user-guide/en/aw/configuration.md) |
| Enable output compression or security checks | [Tokenless](../../docs/user-guide/en/aw/tokenless.md), [sec-core](../../docs/user-guide/en/aw/security.md) |
| Inspect runtime records | [AgentSight](../../docs/user-guide/en/aw/observability.md) |
| Check status, upgrade/uninstall and troubleshoot | [Operations](../../docs/user-guide/en/aw/operations.md) |
| Write Providers, integrate SDKs or understand the architecture | [Developer guide](../../docs/developer-guide/en/aw/index.md) |

<a id="code-architecture"></a>
<a id="development-and-validation"></a>

The [architecture overview](docs/design/en/architecture.md) describes framework and Provider ownership and call paths.
See [Contributing](CONTRIBUTING.md) for source builds, tests and CI entrypoints.
