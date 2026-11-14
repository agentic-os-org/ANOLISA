# AW

[中文版](README_zh.md)

AW provides unified Hook orchestration, managed MCP integration and hot policy updates for Agents and SDK applications.
One configuration composes security checks before tool execution, output compression afterward and custom rules.
Supported resident entrypoints can update policy after tasks drain while keeping the host process running.
Agents retain their models, interfaces and permissions. AW runs on Linux.

<a id="install-and-use"></a>
<a id="installation-and-first-run"></a>

## Quickstart

Install AW core and optional Provider extensions using the [installation guide](../../docs/user-guide/en/aw/installation.md).
AW currently uses standalone packages; `anolisa install aw` and RPM installation are not available yet.
Install and sign in to your Agent first, then generate configuration with `aw config init`.
For changed or missing local Provider files, `aw uninstall --keep-changed` retains changed paths and clears package records; see [operations](../../docs/user-guide/en/aw/operations.md#upgrade-and-uninstall).

The example below uses an installed, signed-in Qoder. `AW_PREFIX` is the installation directory
selected in the guide; both `qodercli` and `aw` must be on `PATH`.

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
The local service remains reusable after the Agent exits. Inspect and stop it when finished.

```bash
aw service status --config "$AW_HOME/aw.yaml"
aw service stop --config "$AW_HOME/aw.yaml"
```

## Shared capabilities

| Capability | What you can do |
| --- | --- |
| Hook orchestration | Select rules by target and tool, compose sec-core, Tokenless and custom Hooks, and share step ordering, execution deadlines and audit. |
| Managed MCP | Apply the same policies at supported actual tool-call boundaries, checking invocation identity and approved arguments. |
| Policy updates | Explicitly apply a new policy to a supported idle instance so subsequent tasks use the new revision. |

Adapters connect native events and adopt supported effects; Providers implement policies.
The sec-core Provider can check locally resolved Skills before Qoder SDK and QwenPaw load their instructions; see [security setup](../../docs/user-guide/en/aw/security.md#check-skills-before-loading).

Core works independently. Install and enable the corresponding Providers for security checks and compression.
Tool result replacement is limited to authorized slots, and native Agent permission checks retain precedence.

Use `aw instance attach --agent TARGET --pid PID` to attach a supported native process, then `aw instance update --agent TARGET` to apply policy while idle. Both commands default to `aw.yaml` in the current directory; see the [command reference](../../docs/user-guide/en/aw/cli-reference.md#aw-instance-attach-and-aw-instance-update) for prerequisites and explicit configuration selection.

Use `aw instance setup` or an application-owned `prepare` call to retain native startup across the eight supported Adapters after one-time integration. Native readiness confirms registration, and `aw instance update` applies policy between completed tasks. SDK factories and QwenPaw use task Scopes; Qoder, OpenClaw and Hermes use native request boundaries. See [independent instances](../../docs/developer-guide/en/aw/deployment/independent-instances.md).

## Hook support matrix

✓ Declared with contract coverage; × unsupported. Generated from the [regression manifest](tests/regression-matrix.json). See [capability support](../../docs/user-guide/en/aw/capabilities.md) for native replacement limits, MCP and policy updates.

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.before` / `replace_input` | `tool.after` / `observe` | `tool.after` / `replace_result` | `input.submit` / `observe` | `input.submit` / `block` | `security.violation` / `observe` | `security.violation` / `block` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `deepseek-harness` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `openclaw` | ✓ | ✓ | × | ✓ | ✓ | × | × | × | × |
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
<!-- aw-regression-matrix:end -->

Qoder CLI also checks the native `UserPromptSubmit` text through `input.submit`; see [Qoder input checks](../../docs/developer-guide/en/aw/agents/qoder.md#check-user-submissions).

Pi `runWorker` factories also support literal user text checks before model dispatch; see the [Pi submission guide](../../docs/developer-guide/en/aw/agents/pi.md#check-user-submissions).

Ordinary resident Qoder SDK and QwenPaw can check submitted text before model
requests through `input.submit`. The sec-core Provider supports fast prompt
injection scanning with separate risk and failure policies; see
[user submission checks](../../docs/user-guide/en/aw/security.md#check-user-submissions).


<a id="hook-support"></a>
<a id="adapter-capability-coverage"></a>

## Supported entrypoints

AW currently integrates Qoder CLI, Qoder SDK, OpenClaw, Hermes, QwenPaw, Pi SDK,
Claude SDK and AgentScope SDK. CLI integrations use native Hooks or plugins;
SDK applications explicitly use managed factories. Runtime admission checks the required
native interfaces; [verified baselines](../../docs/developer-guide/en/aw/native-agents.md)
record the tested framework versions.
[Qoder SDK integration](../../docs/developer-guide/en/aw/agents/qoder-sdk.md) also covers prewarming, native configuration with unmodified SDK/Worker files, Pilot runtime launcher grants, pinned Bash consumers and declarative role assets.
The [capability matrix](../../docs/user-guide/en/aw/capabilities.md) compares result handling,
MCP and policy updates by Adapter and launch mode, with links to verified baselines and setup.


Resident tasks recheck service and owner validity during idle periods and before
covered dispatch. Status separates live workers from failed AW bridges; see
[operations](../../docs/user-guide/en/aw/operations.md) for recovery.

## Documentation

| Task | Guide |
| --- | --- |
| Install and complete a first run | [Quickstart](../../docs/user-guide/en/aw/index.md) |
| Look up every subcommand and option | [CLI reference](../../docs/user-guide/en/aw/cli-reference.md) |
| Select an Agent, configure rules and update policy | [Configuration and launch](../../docs/user-guide/en/aw/configuration.md) |
| Enable output compression or security checks | [Tokenless](../../docs/user-guide/en/aw/tokenless.md), [sec-core](../../docs/user-guide/en/aw/security.md) |
| Inspect runtime records | [AgentSight](../../docs/user-guide/en/aw/observability.md) |
| Check status, upgrade/uninstall and troubleshoot | [Operations](../../docs/user-guide/en/aw/operations.md) |
| Write Providers, integrate SDKs or understand the architecture | [Developer guide](../../docs/developer-guide/en/aw/index.md) |

<a id="code-architecture"></a>
<a id="development-and-validation"></a>

The [architecture overview](docs/design/en/architecture.md) describes framework and Provider ownership and call paths.
See [Contributing](CONTRIBUTING.md) for source builds, tests and CI entrypoints.

`aw config add-native-source` records native source references without opening or
rewriting their files. The shared loader API retains native configuration and
reports admission separately from activation; see [configuration](../../docs/user-guide/en/aw/configuration.md#record-native-configuration-sources).

Local audit consumers can use `aw audit-export` to read retained metadata after
service shutdown. See [audit export](../../docs/user-guide/en/aw/cli-reference.md#aw-audit-export)
for the owned state directory and bounded read interface.
