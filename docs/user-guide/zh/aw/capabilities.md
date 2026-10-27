# AW 能力支持矩阵

[English](../../en/aw/capabilities.md)

AW 用一份配置组合 Hook、受管 MCP 和策略更新，通过
`aw run --config aw.yaml --agent <target>` 选择目标。
本页按 Adapter 和启动模式说明可用范围；各行适用于所链接的版本和接线方式。

## Hook 与 MCP

八个 Adapter 均支持 `tool.before` 的观察与阻断，以及 `tool.after` 的观察。
一份配置可按目标和工具选择步骤，组合布尔命令、结构化 Provider 与原生 Hook 命令。
步骤选择方法见[配置指南](configuration.md#为-hook-步骤选择工具)。

| Adapter / 接入入口 | 后置结果替换 | 受管 MCP 接点 | 可配置 `withhold_result` |
| --- | --- | --- | --- |
| [Qoder CLI](../../../developer-guide/zh/aw/native-agents.md#构建并启动-qoder) | 支持的成功 PostToolUse 结果；失败回调未接入 | 无独立受管接点 | 不支持 |
| [Qoder SDK](../../../developer-guide/zh/aw/native-agents.md#启动-qoder-sdk-worker) | 不支持 | 受管 `createSdkMcpServer` 的进程内 handler | 普通 resident 支持 |
| [OpenClaw](../../../developer-guide/zh/aw/native-agents.md#启动-openclaw) | 运行时作用域内的原生结果槽位 | 无独立受管接点 | 不支持 |
| [Hermes](../../../developer-guide/zh/aw/native-agents.md#启动-hermes) | 原生工具结果槽位 | 无独立受管接点 | 不支持 |
| [QwenPaw](../../../developer-guide/zh/aw/native-agents.md#启动-qwenpaw) | 支持的原生结果槽位 | `managed_mcp_tools(client)` 包装实际 `ClientSession.call_tool` | 普通 resident 支持 |
| [Pi SDK](../../../developer-guide/zh/aw/native-agents.md#启动-pi-sdk-worker) | 成功的单文本结果 | 通过受管自定义工具接入 MCP 客户端 | 不支持配置 |
| [Claude SDK](../../../developer-guide/zh/aw/native-agents.md#启动-claude-sdk-worker) | 获准的原生文本或受管 MCP 结果槽位 | resident 客户端配合 `aw_claude.create_sdk_mcp_server` 的进程内 handler | 不支持配置 |
| [AgentScope SDK](../../../developer-guide/zh/aw/native-agents.md#启动-agentscope-sdk) | 支持的 middleware 结果 | 无独立受管接点 | 不支持 |

结果替换仅供结构化 Provider 修改获准的结果槽位，并保留原生回执和失败状态。
`withhold_result` 在后置步骤失败时阻止结果交付，适用于普通 Qoder SDK、QwenPaw 的
结构化 Provider 和布尔 Hook。Pi、Claude SDK resident 的回调故障会停止任务。
工具已产生的副作用继续生效，原生权限与拒绝保持优先。

受管 MCP 在实际 handler 或远端调用处检查身份、server、tool 和获准参数。
表中没有独立受管接点的入口，仅覆盖框架已暴露的原生工具回调。
选择入口时还需检查以下差异。

- Qoder SDK 提供 Query 身份，缺少来源模型工具 ID；`parentIdentity: "required"` 会被拒绝。
- QwenPaw helper 解析固定工具列表；动态 `Toolkit(mcps=[client])` 发现需另行接线。
- Claude 受管 MCP 仅替换成功、单文本且不含结构化内容的结果。Pi 沿用受管自定义工具的策略链。

MCP 接线、身份与取消规则见[原生接入参考](../../../developer-guide/zh/aw/native-agents.md)，
协议细节见[结构化 MCP](../../../../src/aw/docs/design/zh/adapters/structured-mcp.md)和
[Claude MCP](../../../../src/aw/docs/design/zh/adapters/claude-mcp.md)。
按工具的实际语义配置[安全检查](security.md)和[输出压缩](tokenless.md)，保留结构化及多模态结果。

## 策略更新模式

策略更新可修改 Provider、事件和 `execution.default_event_budget_ms`。
AW 校验完整候选配置，在确认实例空闲后显式激活；配置文件变更不会自动触发更新。
每个任务的工具前后检查和受管 MCP 使用同一策略版本。

| Adapter / 启动模式 | 空闲策略更新 | 就绪与切换条件 |
| --- | --- | --- |
| 普通 Qoder SDK 1.0.56 / Worker 1.1.78 resident | 支持，保留严格 Post、受管 MCP 和私有执行租约 | 受管 `startDaemon()` 确认就绪；全部 Query 完整关闭且清理结束 |
| 普通 QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App resident | 支持，保留严格 Post、受管 MCP 和私有执行租约 | 受管 App startup 确认就绪；全部 reply 及清理结束 |
| Qoder 1.1.73 TUI、上述 Qoder SDK / QwenPaw 的 `--reloadable` 模式 | 受限支持 | 使用原生空闲桥接；SDK 要求显式 daemon 和已初始化 Query；不提供 resident 严格 Post 和私有执行租约 |
| 外部启动的 Qoder 1.1.73 TUI / QwenPaw App | 受限支持，经 PID 接管 | 首请求前接管；满足受限桥接的原生空闲与清理条件 |
| 普通 Qoder CLI、Pi SDK、Claude SDK、OpenClaw、Hermes、AgentScope SDK | 不支持 | 策略随宿主启动，修改需重启 |

暂停任务派发后执行 `aw update --config aw.yaml --agent TARGET`，成功后恢复派发。
同一目标有多个实例时用 `--instance ID` 选择。普通 Qoder SDK 预热时应使用受管
`startDaemon()`；未使用显式 daemon 时，须在 30 秒启动期限内初始化 `createQuery()`，
该 Query 完整关闭前持续占用实例。

活跃任务或清理未完成时返回 `busy`，更新不排队。候选准备失败或版本冲突保留旧策略。
激活后审计失败会停止后续派发，需停止受影响的 worker，再按期望策略重启 service 和 worker。
模型凭据、SDK / Worker 版本、启动配置及 AW 二进制的变更需重启或重新部署。
完整操作和恢复步骤见[更新预热实例](configuration.md#更新预热实例)，
外部 PID 接入见[接入已启动的原生进程](configuration.md#接入已启动的原生进程)。

## 尚未开放的能力

其余 14 个配置事件名尚无公共 Provider 派发，完整词汇见
[开发配置参考](../../../developer-guide/zh/aw/configuration.md#事件效果与工具选择)。
`replace_input`、通用 `ask` 和 guard 尚不支持；sec-core 使用选中的 `tool.before` 步骤。
活动任务中的策略切换、通用 MCP 网关，以及 Tokenless Schema/BeforeModel、Retrieve 接入仍待实现。

## 验收依据

支持范围由 [Adapter catalog](../../../../src/aw/crates/aw-adapter/src/adapter/catalog.rs)、
[回归矩阵](../../../../src/aw/tests/regression-matrix.json)和
[原生验收清单](../../../../src/aw/tests/native-matrix.json)共同维护。
原生清单记录固定版本、验收命令和自动化 / 手工范围；Qoder SDK 托管模型受管 MCP
用例仍使用手工容器证据。完整验收说明见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。

<details>
<summary>Hook 事件与效果契约表</summary>

下表从回归矩阵生成，由 CI 检查声明与契约用例的一致性。
`✓` 表示已声明且有契约覆盖，`×` 表示尚未支持；原生结果范围与启动模式以上文为准。

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
