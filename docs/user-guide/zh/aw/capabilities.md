# AW 能力支持矩阵

[English](../../en/aw/capabilities.md)

通过本页选择 Agent 入口，并确认 AW 在该入口可以执行哪些策略控制。
统一入口是 `aw run --config aw.yaml --agent <target>`；实际能力取决于所选
Adapter、原生版本和启动模式。

本矩阵描述 #8840 合入后的能力合同，依据其候选 `1b48f237d183` 和 ANOLISA
`main` 的 `32925dc0a` 核对，包含 #9108 的常驻安全和受管 MCP 改动。
相关合同变更时应同步维护本页。原生版本和准确验收范围维护在
[原生验收清单](../../../../src/aw/tests/native-matrix.json)中；配置语法见
[配置参考](../../../developer-guide/zh/aw/configuration.md)。

## 如何理解支持状态

- **可用**：指定入口准入该能力，并存在原生采纳路径。原生清单区分自动化和手工验收范围。
- **受限**：需要表中指定的模式、结果形态或显式工厂。
- **规划**：仍需实现或完成原生验收。
- 配置识别的名字、内部生命周期信号和公开 Provider Hook 分别有自己的合同。
  每个公开 Hook 都需要派发、取消、关联、原生效果采纳和验收覆盖。

公共策略链支持布尔用户命令、结构化 Provider 和原生 Hook 命令。可移植策略使用
归一化事件；原生命令输入输出保留框架方言。准入会拒绝不支持的活动控制。
原生权限和拒绝保持优先。一份配置可以包含按 target 区分的步骤选择器，AW 在准备
目标前投影适用步骤。参见[工具选择](configuration.md#为-hook-步骤选择工具)。

## 16 个通用 Hook 名称

目前 8 个已注册 Adapter 都只将 `tool.before` 和 `tool.after` 派发到公共 Provider
链。成功、错误、被拒绝尝试的事件覆盖遵循各自原生回调合同。其余 14 个名称属于
配置词汇，公开派发仍待实现。

以下为建议的开发顺序，尚未承诺可用时间：**A** 维护现有工具控制并将常驻热更新
公共底座扩展到更多 Adapter；**B** 开放可靠的生命周期观察；**C** 补模型、上下文和子 Agent 接点；
**D** 扩展权限及专用安全合同。不同框架可以在原生边界验证后分批进入各阶段。

| Hook | 已有公开 Provider 派发 | 规划范围 / 顺序 |
| --- | --- | --- |
| `tool.before` | 全部 8 个 Adapter：`observe`、`block` | A：保持拒绝、已批准参数不可变、重试重新授权 |
| `tool.after` | 全部 8 个：`observe`；除 Qoder SDK 外的 7 个准入受限 `replace_result` | A：保持原生回执与失败处理；Qoder SDK 结果替换单独跟进 |
| `session.start` | 规划 | B：验证会话创建、加载、恢复的观察点 |
| `input.submit` | 规划 | B：验证输入提交观察点，区分提交与执行 |
| `turn.stop` | 规划 | B：原生停止观察及明确结果，不能推断任务成功 |
| `session.end` | 规划 | B：区分会话结束、进程退出和已确认清理 |
| `runtime.observed` | 规划 | B：将可信运行时登记开放为 Provider 事件 |
| `runtime.exited` | 规划 | B：将可信根进程退出开放为 Provider 事件 |
| `coverage.changed` | 规划 | B：公开已验证接入覆盖的变化 |
| `model.before_request` | 规划 | C：验证模型发送边界；Schema 转换需要独立效果和采纳实现 |
| `compact.before` | 规划 | C：验证上下文压缩前边界 |
| `compact.after` | 规划 | C：观察压缩结果及上下文身份 |
| `subagent.start` | 规划 | C：可信父子身份和原生启动边界 |
| `subagent.stop` | 规划 | C：子任务终止和回调排空语义 |
| `permission.request` | 规划 | D：观察原生权限请求；通用 `ask` / 审批需要独立合同 |
| `security.violation` | 规划；配置的 guard 在运行时准入阶段被拒绝 | D：明确最终候选检查与实际执行边界 |

现有 sec-core 检查运行在选中的 `tool.before` 步骤，无需 `security.violation`。
内部模型身份登记、请求计数、清理通知和证据采集尚未开放这些预留的公开 Hook。
结构化 `replace_input` 和通用 `ask` 仍不支持。

## Adapter 与效果矩阵

每行都准入前置观察 / 阻断和后置观察。结果替换只修改获准的结果槽位；原生命令和
布尔 Hook 不能请求替换。`withhold_result` 是后置步骤的**失败动作**，与替换效果
分别定义，需要已验证的结果交付边界。

| Adapter | 原生验收入口 / 版本 | 后置结果替换 | 可配置 `on_error: withhold_result` |
| --- | --- | --- | --- |
| `qoder` | CLI >=1.1.64；普通交互 / print，热更新版本见下表 | 限于支持的成功 PostToolUse；失败回调尚未接入 | 不支持 |
| `qoder-sdk` | SDK 1.0.56 / Worker 1.1.78；受管 daemon / Query 工厂 | 不支持 | 仅普通 resident；结构化 Provider 和布尔 Hook |
| `openclaw` | 2026.9.6 Gateway plugin | 限于运行时作用域内的原生结果槽位 | 不支持 |
| `hermes` | Chat plugin；版本提交见原生清单 | 限于原生工具结果路径 | 不支持 |
| `qwenpaw` | 2.2.2.post1 / AgentScope 2.0.10 App/API | 限于支持的原生结果槽位 | 仅普通 resident；结构化 Provider 和布尔 Hook |
| `pi` | SDK 0.84.2 task / resident 工厂；不启动原生 Pi CLI | 限于成功的单文本结果 | 不支持配置该动作；resident 回调失败会停止任务 |
| `claude-sdk` | SDK 0.2.88 / bundled CLI 2.1.161；task / resident 客户端工厂 | 限于获准的原生文本或受管 MCP 结果槽位 | 不支持配置该动作；resident 回调失败会停止任务 |
| `agentscope` | SDK 2.0.6 显式工厂 | 限于支持的 middleware 结果 | 不支持 |

普通 resident Pi、Claude SDK、Qoder SDK 和 QwenPaw 会回收已完成任务状态，并限制
活动资源。独立的受限热更新模式有各自生命周期合同；尤其是 Qoder SDK reloadable
实例会保留 Hook 关联历史。选择模式时应阅读
[原生接入参考](../../../developer-guide/zh/aw/native-agents.md)，确认原生后置事件
覆盖、清理要求和错误行为。仅原生接点测试或确定性本地模型验收会按其实际边界记录；
任意托管模型部署仍需相应验收。

## MCP 覆盖

受管 MCP 在实际调用处，结合可信身份、server、tool 和已批准参数执行授权。
原生工具回调只覆盖框架公开的事件，实际 handler / 远端调用边界需要单独接入。
AW 目前没有覆盖任意外部客户端的通用 MCP 网关。

| Adapter | 受管实际调用边界 | 身份与限制 |
| --- | --- | --- |
| `claude-sdk` | resident 客户端工厂配合 `aw_claude.create_sdk_mcp_server` 进程内 handler | 原生派发身份绑定模型调用；Pre/Post 各一次；仅成功的单文本且无结构化内容的结果可替换 |
| `qoder-sdk` | 受管 `createSdkMcpServer` 进程内 handler | Query 身份及精确 server / tool / input；缺少来源模型工具 ID，`parentIdentity: "required"` 会拒绝；普通 resident 支持严格 Post；不支持替换 |
| `qwenpaw` | 显式 `managed_mcp_tools(client)` 包装实际 `ClientSession.call_tool` | 模型父调用身份；保留 schema、annotations 和完整回执；原生远端客户端生命周期归应用 |
| `pi` | 经受管公共自定义工具暴露的 MCP 客户端 | 复用自定义工具 Pre/Post 链；没有独立通用 MCP 连接包装 |
| `qoder` | 无独立受管 MCP 工厂 | 仅覆盖已暴露的原生工具回调；结构化派发接入待实现 |
| `openclaw` | 无独立受管 MCP 工厂 | 仅覆盖已暴露的原生工具回调；结构化派发接入待实现 |
| `hermes` | 无独立受管 MCP 工厂 | 仅覆盖已暴露的原生工具回调；结构化派发接入待实现 |
| `agentscope` | 无独立受管 MCP 工厂 | 仅覆盖已暴露的原生工具 middleware；QwenPaw 的受管 helper 未验收此 Adapter |

QwenPaw helper 解析固定工具列表；动态 `Toolkit(mcps=[client])` 发现不会安装该接点。
Qoder SDK 托管模型受管 MCP 用例已有手工容器证据，自动化仍待补齐。准确的身份、取消
与验收边界见[结构化 MCP 合同](../../../../src/aw/docs/design/zh/adapters/structured-mcp.md)
和 [Claude MCP 合同](../../../../src/aw/docs/design/zh/adapters/claude-mcp.md)。

按已声明的工具语义选择安全 Hook。工具名或 `command` 参数本身无法证明 shell 执行、
工作目录和授权；不透明工具需要显式映射 / 授权策略。保留结构化和多模态回执，文本块
也可能包含业务要解析的数据。Tokenless Schema/BeforeModel 和 Retrieve 的待实现依赖
见 [Tokenless 交付合同](../../../../src/aw/docs/design/zh/adapters/resident-sdk.md)。

## 策略热更新

现有公共更新事务会验证完整候选配置、准备 Provider、检查 revision 与空闲状态、写入
持久化审计并激活策略。允许修改 Provider、事件和 `execution.default_event_budget_ms`。
模型凭据、SDK / Worker 版本、启动配置和 AW 二进制继续通过重启 / 部署更新。
配置文件不会被自动监听，需显式执行
[`aw update`](configuration.md#更新预热实例)。

| Adapter / 模式 | 已有策略热更新 | 就绪和切换边界 |
| --- | --- | --- |
| 普通 Qoder SDK 1.0.56 / Worker 1.1.78 | 显式空闲更新，保留严格 Post、受管 MCP 和私有租约 | 受管 `startDaemon()` 在首个 Query 前确认就绪；全部 Query 关闭且清理完成后更新 |
| 普通 QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | 显式空闲更新，保留严格 Post、受管 MCP 和私有租约 | 受管 App startup 在首个 reply 前确认就绪；全部 reply 和清理结束后更新 |
| 受限 Qoder 1.1.73 TUI / Qoder SDK / QwenPaw App | 既有显式 `--reloadable` 模式 | 原生空闲桥接；SDK 要求显式 daemon 和已初始化受管 Query；继续不提供严格 Post 和私有执行租约 |
| 外部启动的 Qoder TUI / QwenPaw App | 既有支持的 PID attach 路径 | 首请求前接管，保留受限原生空闲和清理合同 |
| 普通 Qoder CLI（交互 / print / headless） | 不支持 | 需验证原生交接 |
| Pi SDK / Claude SDK | 不支持 | resident 任务、快照和路由交接待接入；重启前保持启动策略 |
| OpenClaw / Hermes / AgentScope SDK | 不支持 | 需验证原生路由 / 空闲交接；重启前保持启动策略 |

预热时先启动受管 Qoder daemon 或 QwenPaw App，准备 Hook 文件和完整配置，暂停
任务派发，执行 `aw update --config aw.yaml --agent TARGET`，成功后恢复派发。
普通 Qoder 未使用显式 daemon 时，仍须在 30 秒启动期限内初始化 `createQuery()`；
工厂预留持续占用实例，直到 Query 关闭。同一 target 有多个实例时用 `--instance ID` 选择。

PID 保留两类用途：通过 `aw agent connect --pid PID`（及旧 install/remove 入口）
接管外部原生进程，以及内部进程身份 / 存活检查。便捷选择器 `aw update --pid` 延后；
所有支持的更新模式使用 `--agent` / `--instance`。Adapter 的更新边界需要单独验证。

公共事务协调激活、任务预留、不可变策略快照、原生路由校验 / 确认与最终清理。
一个任务的 PreTool、MCP 和 PostTool 使用同代策略；准备中、未确认交接及未完成清理
都阻止激活。Adapter 提供就绪、任务边界和路由采纳，框架负责候选校验、revision 检查、
审计和激活。

忙碌时拒绝更新，不排队、不自动取消或重试。准备失败、revision 冲突保留旧策略；
清理状态不明继续占用。Host 激活后审计失败会关闭实例并停止 service 的后续派发，
需停止受影响的 worker，再按期望策略重启 service 和 worker 恢复。传输响应不确定时，派发前检查实例状态及生效 revision。
每次更新作用于一个实例；控制面需串行化更新和任务派发，包括预热后的首个任务。

其他 Adapter 复用此底座，并为自身任务与路由边界补原生验收。公开生命周期 Hook、
活动任务中切换、Tokenless Schema/Retrieve 和 Qoder 结果替换继续单独推进。

## 证据与维护

[编译后的 Adapter catalog](../../../../src/aw/crates/aw-adapter/src/adapter/catalog.rs)、
[回归矩阵](../../../../src/aw/tests/regression-matrix.json)和
[原生验收清单](../../../../src/aw/tests/native-matrix.json)提供可执行的支持证据。
README 自动生成的事件 / 效果表汇总 catalog；本页补充模式、MCP、更新和待办边界。
相关合同变更必须同步更新两种语言及验收引用。允许框架交付顺序不同；尚未支持的能力
必须在准入和本矩阵中明确呈现。
