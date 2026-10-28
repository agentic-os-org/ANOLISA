# AW 能力支持矩阵

[English](../../en/aw/capabilities.md)

AW 为不同 Agent 共用工具检查、结果处理和自定义规则。以下能力由 Adapter 接入原生工具流程；具体版本和启动步骤见[Agent 接入索引](../../../developer-guide/zh/aw/native-agents.md)。

## Hook 支持矩阵

✓ 已声明且有契约测试覆盖，× 尚不支持。结果替换限于获准的文本槽，保留原生回执及失败状态。

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
`tool.before` 可观察或阻断，`tool.after` 可观察或通过结构化 Provider 替换结果。布尔脚本和原生命令不能请求结果替换。
配置可按[工具选择步骤](configuration.md#为-hook-步骤选择工具)，原生权限和拒绝优先。
Qoder CLI 尚未接入失败工具的 Post 回调；其他结果类型和接点限制见各 Agent 页。

## 16 个通用 Hook

下表列出公共 Provider Hook 的实现状态，各 Adapter 的效果支持见上表。

| Hook | 状态 |
| --- | --- |
| `session.start` | 规划中 |
| `input.submit` | 规划中 |
| `tool.before` | 已实现 |
| `tool.after` | 已实现 |
| `permission.request` | 规划中 |
| `compact.before` | 规划中 |
| `compact.after` | 规划中 |
| `subagent.start` | 规划中 |
| `subagent.stop` | 规划中 |
| `turn.stop` | 规划中 |
| `session.end` | 规划中 |
| `model.before_request` | 规划中 |
| `runtime.observed` | 规划中 |
| `runtime.exited` | 规划中 |
| `security.violation` | 规划中 |
| `coverage.changed` | 规划中 |

## MCP 与策略更新

| 入口 | 受管 MCP | 空闲策略更新 |
| --- | --- | --- |
| Qoder SDK | 进程内 handler，提供 Query 身份 | 普通常驻模式支持，全部 Query 关闭后更新 |
| QwenPaw App | 显式包装 MCP 客户端调用 | 普通常驻模式支持，全部 reply 排空后更新 |
| Claude SDK resident | 进程内 handler，可压缩成功单文本结果 | 修改需重启 |
| Pi SDK | 通过受管自定义工具接入 MCP 客户端 | 修改需重启 |
| Qoder CLI、OpenClaw、Hermes、AgentScope SDK | 覆盖已暴露的原生工具回调 | 普通模式修改需重启 |

Qoder TUI、Qoder SDK 和 QwenPaw 另有受限更新模式，版本与部署步骤见[策略更新](../../../developer-guide/zh/aw/deployment/policy-updates.md)。
MCP 工具需按实际输入语义配置安全检查，接线见 [Qoder SDK / QwenPaw MCP](../../../../src/aw/docs/design/zh/adapters/structured-mcp.md) 与 [Claude SDK MCP](../../../../src/aw/docs/design/zh/adapters/claude-mcp.md)。

Qoder SDK 应用还可接入固定摘要的 Bash 凭据消费者、Worker import 和声明式角色资产。
接线与权限要求见 [Qoder SDK 指南](../../../developer-guide/zh/aw/agents/qoder-sdk.md#原生-bash-命令消费者)。

## 使用边界

- 普通 Qoder SDK、QwenPaw 可配置后置故障 `withhold_result`，暂扣结果并停止后续派发；已发生的工具副作用保留。
- Pi、Claude SDK resident 的回调故障会停止任务。后置检查无法撤销已执行工具。
- 当前公共 Provider Hook 为上表两个事件。其余 14 个事件名、通用审批、输入替换、guard、Schema/BeforeModel 和 Retrieve 接入尚未开放。
- AW 通过原生 Hook 执行策略，OS 沙箱隔离需另行部署。

矩阵由[回归清单](../../../../src/aw/tests/regression-matrix.json)生成。[原生验收清单](../../../../src/aw/tests/native-matrix.json)记录固定版本及验证范围，操作方法见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。
