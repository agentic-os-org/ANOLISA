# AW capability support matrix

[中文版](../../zh/aw/capabilities.md)

AW shares tool checks, result processing and custom rules across Agents. Adapters connect these capabilities to native tool execution. See the [Agent index](../../../developer-guide/en/aw/native-agents.md) for versions and setup.

## Hook support matrix

✓ Declared with contract coverage; × unsupported. Result replacement is limited to authorized text slots and preserves native envelopes and failure status.

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
`tool.before` can observe or block; `tool.after` can observe or replace results through a structured Provider. Boolean scripts and native commands cannot request result replacement.
Configuration can [select steps by tool](configuration.md#select-tools-for-a-hook-step); native permissions and denial retain precedence.
Qoder CLI has no failed-tool Post callback integration. See each Agent page for result types and callback limits.

## 16 common Hooks

Public Provider Hook status; see the matrix above for effects supported by each Adapter.

| Hook | Status |
| --- | --- |
| `session.start` | Planned |
| `input.submit` | Planned |
| `tool.before` | Implemented |
| `tool.after` | Implemented |
| `permission.request` | Planned |
| `compact.before` | Planned |
| `compact.after` | Planned |
| `subagent.start` | Planned |
| `subagent.stop` | Planned |
| `turn.stop` | Planned |
| `session.end` | Planned |
| `model.before_request` | Planned |
| `runtime.observed` | Planned |
| `runtime.exited` | Planned |
| `security.violation` | Planned |
| `coverage.changed` | Planned |

## MCP and policy updates

| Entrypoint | Managed MCP | Idle policy updates |
| --- | --- | --- |
| Qoder SDK | In-process handlers with Query identity | Ordinary resident mode, after all Queries close |
| QwenPaw App | Explicitly wrapped MCP client calls | Ordinary resident mode, after all replies drain |
| Claude SDK resident | In-process handlers; successful single-text result compression | Restart required |
| Pi SDK | MCP clients exposed as managed custom tools | Restart required |
| Qoder CLI, OpenClaw, Hermes, AgentScope SDK | Exposed native tool callbacks | Ordinary mode requires restart |

Qoder TUI, Qoder SDK and QwenPaw also offer restricted update modes. See [policy updates](../../../developer-guide/en/aw/deployment/policy-updates.md) for versions and deployment.
Configure MCP security checks for actual input semantics; see [Qoder SDK / QwenPaw MCP wiring](../../../../src/aw/docs/design/en/adapters/structured-mcp.md) and [Claude SDK MCP wiring](../../../../src/aw/docs/design/en/adapters/claude-mcp.md).

Qoder SDK applications can also integrate pinned Bash credential consumers, Worker
imports and declarative role assets. Setup and permission requirements are in the
[Qoder SDK guide](../../../developer-guide/en/aw/agents/qoder-sdk.md#native-bash-command-consumers).

## Boundaries

- Ordinary Qoder SDK and QwenPaw accept Post failure action `withhold_result`, withholding delivery and stopping subsequent dispatch. Existing tool side effects remain.
- Pi and Claude SDK resident callback failures stop the task. Post checks cannot undo an executed tool.
- Public Provider Hooks currently cover the two events above. The other 14 event names, common approval, input replacement, guard, Schema/BeforeModel and Retrieve integration are not available.
- AW enforces policy through native Hooks. Deploy OS sandbox isolation separately.

The matrix is generated from the [regression manifest](../../../../src/aw/tests/regression-matrix.json). The [native manifest](../../../../src/aw/tests/native-matrix.json) records pinned versions and coverage; see [contributing](../../../../src/aw/CONTRIBUTING.md) for validation.
