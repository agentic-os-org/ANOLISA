# AW capability support

[中文版](../../zh/aw/capabilities.md)

AW composes Hooks, managed MCP and policy updates in one configuration, selecting
a target through `aw run --config aw.yaml --agent <target>`.
This page compares Adapters and launch modes. Each row applies to the versions
and integration steps in its linked reference.

## Hooks and MCP

All eight Adapters support observation and blocking at `tool.before`, and
observation at `tool.after`. One configuration selects steps by target and tool,
composing boolean commands, structured Providers and native Hook commands.
See [tool selection](configuration.md#select-tools-for-a-hook-step).

| Adapter / entrypoint | After result replacement | Managed MCP boundary | Configurable `withhold_result` |
| --- | --- | --- | --- |
| [Qoder CLI](../../../developer-guide/en/aw/native-agents.md#build-and-start-qoder) | Supported successful PostToolUse results; failure callback not connected | No separate managed boundary | Unsupported |
| [Qoder SDK](../../../developer-guide/en/aw/native-agents.md#start-a-qoder-sdk-worker) | Unsupported | In-process handlers through managed `createSdkMcpServer` | Ordinary resident supported |
| [OpenClaw](../../../developer-guide/en/aw/native-agents.md#start-openclaw) | Runtime-scoped native result slots | No separate managed boundary | Unsupported |
| [Hermes](../../../developer-guide/en/aw/native-agents.md#start-hermes) | Native tool result slots | No separate managed boundary | Unsupported |
| [QwenPaw](../../../developer-guide/en/aw/native-agents.md#start-qwenpaw) | Supported native result slots | `managed_mcp_tools(client)` around actual `ClientSession.call_tool` | Ordinary resident supported |
| [Pi SDK](../../../developer-guide/en/aw/native-agents.md#start-a-pi-sdk-worker) | Successful single text results | MCP clients exposed through managed custom tools | Not configurable |
| [Claude SDK](../../../developer-guide/en/aw/native-agents.md#start-a-claude-sdk-worker) | Authorized native text or managed MCP result slots | In-process `aw_claude.create_sdk_mcp_server` handlers with a resident client | Not configurable |
| [AgentScope SDK](../../../developer-guide/en/aw/native-agents.md#start-agentscope-sdk) | Supported middleware results | No separate managed boundary | Unsupported |

Only structured Providers can replace authorized result slots, preserving native
envelopes and failure status. `withhold_result` prevents result delivery after an
after-step failure; ordinary Qoder SDK and QwenPaw support it for structured
Providers and boolean Hooks. Pi and Claude SDK resident callback failures stop
the task. Existing tool side effects remain, and native permissions and denials
retain precedence.

Managed MCP checks identity, server, tool and approved arguments at the actual
handler or remote call. Entrypoints without a separate managed boundary cover
the native tool callbacks exposed by their framework. Check these differences
when choosing an entrypoint.

- Qoder SDK provides Query identity without the originating model tool ID; `parentIdentity: "required"` is rejected.
- QwenPaw's helper resolves a fixed tool list; dynamic `Toolkit(mcps=[client])` discovery needs separate integration.
- Claude managed MCP replaces only successful single text results without structured content. Pi uses its managed custom-tool policy path.

See [native integration](../../../developer-guide/en/aw/native-agents.md) for
MCP setup, identity and cancellation rules, and the
[structured MCP](../../../../src/aw/docs/design/en/adapters/structured-mcp.md) and
[Claude MCP](../../../../src/aw/docs/design/en/adapters/claude-mcp.md) designs for protocol details.
Configure [security checks](security.md) and [output compression](tokenless.md)
for the tool's actual semantics, preserving structured and multimodal results.

## Policy update modes

Policy updates can change Providers, events and `execution.default_event_budget_ms`.
AW validates the complete candidate and explicitly activates it after checking
that the instance is idle. Editing a file does not trigger an update.
A task's before/after checks and managed MCP use the same policy revision.

| Adapter / launch mode | Idle policy update | Readiness and switching conditions |
| --- | --- | --- |
| Ordinary Qoder SDK 1.0.56 / Worker 1.1.78 resident | Supported; retains strict Post, managed MCP and private execution leases | Managed `startDaemon()` confirms readiness; all Queries must fully close and cleanup must finish |
| Ordinary QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App resident | Supported; retains strict Post, managed MCP and private execution leases | Managed App startup confirms readiness; all replies and cleanup must finish |
| Qoder 1.1.73 TUI, or the above Qoder SDK / QwenPaw in `--reloadable` mode | Restricted | Native idle bridge; SDK requires explicit daemon and initialized Query; resident strict Post and private execution leases unavailable |
| Externally started Qoder 1.1.73 TUI / QwenPaw App | Restricted, through PID attachment | Attach before the first request; satisfy the restricted bridge's native idle and cleanup conditions |
| Ordinary Qoder CLI, Pi SDK, Claude SDK, OpenClaw, Hermes, AgentScope SDK | Unsupported | Policy is fixed at host launch; restart to change it |

Pause task dispatch, run `aw update --config aw.yaml --agent TARGET`, and resume
dispatch after success. Use `--instance ID` when a target has multiple instances.
Prewarm ordinary Qoder SDK with managed `startDaemon()`. Without an explicit
daemon, initialize `createQuery()` within the 30-second startup deadline; that
Query occupies the instance until fully closed.

Active tasks or unfinished cleanup cause `busy`; updates are not queued.
Candidate preparation failures and revision conflicts retain the old policy.
Audit failure after activation stops further dispatch; stop affected workers,
then restart the service and workers with the intended policy.
Changes to model credentials, SDK/Worker versions, launch configuration or AW
binaries require restart or deployment.
See [update a prewarmed instance](configuration.md#update-a-prewarmed-instance)
for operations and recovery, and
[attach an already-running native process](configuration.md#attach-to-an-already-started-native-process)
for external PID integration.

## Capabilities awaiting implementation

The other fourteen configuration event names have no public Provider dispatch.
See the [developer configuration reference](../../../developer-guide/en/aw/configuration.md#events-effects-and-tool-selection)
for the full vocabulary. `replace_input`, portable `ask` and guards are unsupported;
sec-core uses selected `tool.before` steps. Active-task policy switching, a general
MCP gateway, and Tokenless Schema/BeforeModel and Retrieve integration remain unimplemented.

## Acceptance evidence

The [Adapter catalog](../../../../src/aw/crates/aw-adapter/src/adapter/catalog.rs),
[regression matrix](../../../../src/aw/tests/regression-matrix.json) and
[native inventory](../../../../src/aw/tests/native-matrix.json) maintain support claims.
The native inventory records pinned versions, commands and automated/manual
boundaries. Qoder SDK hosted managed-MCP cases still use manual container evidence.
See [Contributing](../../../../src/aw/CONTRIBUTING.md) for the full acceptance scope.

<details>
<summary>Hook event and effect contract catalog</summary>

This table is generated from the regression matrix. CI checks declarations
against contract cases. `✓` means declared with contract coverage; `×` means
unsupported. Native result limits and launch modes follow the sections above.

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

</details>
