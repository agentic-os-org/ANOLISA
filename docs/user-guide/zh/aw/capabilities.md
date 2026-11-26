# AW 能力支持矩阵

[English](../../en/aw/capabilities.md)

两语言 SDK 支持应用确认的 attach／detach，操作见[协作式控制](../../../developer-guide/zh/aw/deployment/native-sdk.md#协作式-attach-与-detach)。

应用自行持有的 QwenPaw App 可使用 [Python SDK](../../../developer-guide/zh/aw/deployment/python-sdk.md)，公开 middleware 的覆盖范围在接入指南中单独说明。

应用自行持有的 Qoder Query 可通过 [TypeScript SDK](../../../developer-guide/zh/aw/deployment/native-sdk.md) 显式选择原生回退。公开 Hook 接线的覆盖与验收范围单独列出。

常驻 Claude 和 Pi worker 可以原生启动并发布 SDK 监听。控制器按 worker PID 接入，准入后再放行受保护的任务。这两个入口始终要求 AW 可用，覆盖已配置的 `input.submit`、`tool.before`（含 `replace_input`）和 `tool.after`（含 `replace_result`）。生命周期 Hook 的适用入口见下表。Claude worker 原生清理无法确认时，连同其进程组被终止。Pi 严格结果保护沿用托管入口的 0.84.2–1.5.2 版本区间。 [Claude Python 工厂](../../../developer-guide/zh/aw/agents/claude-sdk.md) · [Pi TypeScript 工厂](../../../developer-guide/zh/aw/deployment/native-sdk.md#pi-worker-接入)

AW 为不同 Agent 共用工具检查、结果处理和自定义规则。以下能力由 Adapter 接入原生工具流程；具体版本和启动步骤见[Agent 接入索引](../../../developer-guide/zh/aw/native-agents.md)。

Python 和 TypeScript 应用均可通过 `sdk/application` profile 接入通用输入、工具和生命周期
Hook，支持显式启用的 `replace_input`、`replace_result` 与 `withhold_result`（流式严格替换或
扣留要求先完成有界缓冲）。创建 `LanguageSource` 时声明需要的生命周期事件，再从应用的原生边界
调用对应的 `SourceSession` 方法。应用在结束轮次前采用 `turn.stop` 返回的决策。
[通用 Runtime SDK 接入](../../../developer-guide/zh/aw/deployment/native-sdk.md#通用应用接入)

## Hook 支持矩阵

✓ 已声明且有契约测试覆盖，× 尚不支持。多数 Adapter 在获准文本槽中替换结果并保留原生回执与失败状态；Codex 的普通模型文本替换范围见下文。

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.before` / `replace_input` | `tool.after` / `observe` | `tool.after` / `replace_result` | `input.submit` / `observe` | `input.submit` / `block` | `security.violation` / `observe` | `security.violation` / `block` | `session.end` / `observe` | `session.start` / `observe` | `turn.stop` / `observe` | `turn.stop` / `block` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | × | × | ✓ | × |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | ✓ | ✓ |
| `codex` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | × | × | × | ✓ | ✓ | × |
| `deepseek-harness` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | ✓ | ✓ | × |
| `hermes` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | ✓ | × |
| `openclaw` | ✓ | ✓ | × | ✓ | ✓ | × | × | × | × | ✓ | ✓ | ✓ | × |
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | ✓ | × |
| `qoder` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | × | × |
| `qoder-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | × | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | × | × | ✓ | × |
| `sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->
`tool.before` 可观察或阻断，`tool.after` 可观察或通过结构化 Provider 替换结果。布尔脚本和原生命令不能请求结果替换。
配置可按[工具选择步骤](configuration.md#为-hook-步骤选择工具)，原生权限和拒绝优先。
Qoder CLI 尚未接入失败工具的 Post 回调。Codex CLI 0.163.0 和 0.166.0
已验收提交与工具执行前阻断，
并可在关闭 Code Mode 且没有其他 PostToolUse Hook 时替换普通模型请求中的文本；
策略前捕获、遥测和 Hook 故障仍不受保护。本地 app-server stdio 在受信客户端保持线程与轮次
工作目录和配置不变时通过同等效果验收。0.166.0 网关入口限制线程为本进程新建且 cwd 固定，
可在模型出口接入独立的凭证检查。Shell 和 MCP 原生通知仍可能携带原文。见 [Codex 指南](../../../developer-guide/zh/aw/agents/codex.md)。
其他结果类型和接点限制见各 Agent 页。
Codex 的 `tool.before` 标为必需时，AW 拒绝其他可在检查后改写参数的原生 PreToolUse Hook；
非必需检查仅覆盖原始输入。

普通 resident Qoder SDK、QwenPaw、Claude SDK 前台/常驻工厂、Pi worker、AgentScope 工厂回复
、DeepSeek Harness headless 轮次及本地 Codex CLI在模型请求前支持 `input.submit` 观察与阻断，检查文本并关联会话和轮次。
Claude 支持字符串或单条异步文本消息，拒绝或取消后完成清理再复用。DeepSeek 配置该检查后拒绝含非文本
内容的提交；Web、SDK 和 ACP 输入路径仍需原生验收。受限 reloadable 模式及未声明此能力的 Adapter
拒绝该事件。
配置与验收方法见[用户提交检查](security.md#检查用户提交)。

AgentScope 2.0.6 在 `reply` / `reply_stream` 检查原生用户 `Msg` 文本，消息与清理要求见[输入合同](../../../developer-guide/zh/aw/agents/agentscope.md#在回复前检查用户输入)。

工具 PII 已在 Qoder SDK 1.0.56 / Worker 1.1.78 普通常驻 Query，以及 QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App 回复中验收。输入检查先于工具执行；输出检查扫描完整结果，检查保留字段后替换一个获准文本槽，必需检查失败时停止交付。QwenPaw 对选中工具的流在 SSE/模型交付前缓冲。Qoder SDK 的受限 `--reloadable` 模式拒绝结果替换。多文本块、媒体、托管 MCP 与后台 PII 结果交付需分别验收。参见 [PII 配置](security.md#在-qoder-sdk-或-qwenpaw-中检查工具-pii)和[原生验收用例](../../../../contribute/zh/aw/testing.md#工具-pii-验收)。

Qoder SDK 前台 `Read` 的文本结果支持 PII 内容替换，并保留文件元数据；图像及其他非文本 Read 结果需分别验收。

Pi 输入检查要求 `runWorker` 工厂和 interactive 字面量文本。启用后拒绝媒体、`input`/`context`/`before_provider_request` 扩展处理器、展开的斜杠命令及队列/其他输入入口。输入拒绝或 block 检查故障后，同一会话可在清理完成后继续 prompt；取消和清理失败会终止任务。直接回调改写用户消息或序列化 Provider 请求也会被拒绝。

Claude SDK 对精确匹配的 Python／TypeScript SDK／bundled CLI 构建开放最终
`security.violation` 检查。私有运行时 Gate 在 Post 回调完成后检查序列化工具
结果；未知构建拒绝必需策略。范围见
[Claude 最终结果限制](../../../developer-guide/zh/aw/agents/claude-sdk.md#最终工具结果安全边界)。

Qoder SDK 1.0.57 / Worker 1.1.81 在 Linux arm64/x64 上支持单次前台 Bash Query 的必需最终结果检查，在原生请求编码前检查 Post Hook 后的文本；未覆盖的传输、额外上下文轮次和运行时构建均拒绝放行。参见[配置与范围](../../../developer-guide/zh/aw/agents/qoder-sdk.md#最终工具结果检查)。

## 生命周期边界

会话 Hook 观察原生的开始和结束。根据 Adapter 的接点，`turn.stop` 可以观察已经结束的轮次，
也可以检查原生即将结束的一次请求。审计记录保留接点提供的停止或结束原因，并关联会话、
轮次及已知的任务和进程身份。Service 分别记录策略步骤结果与原生停止原因。

| 入口 | 生命周期接点 |
| --- | --- |
| Qoder CLI | 普通托管启动使用原生 `SessionStart` 和 `SessionEnd`。当前固定版本的 Stop 回调缺少轮次 ID。启动挂接与 reloadable 模式沿用原有就绪和清理协议。 |
| Qoder SDK | 普通托管 Query 观察原生会话开始和结束。应用自行安装的公开 Hook bridge 观察会话开始。受限 reloadable 模式及精确最终结果 profile 暂不覆盖这些接点。 |
| Claude SDK | Python 前台及常驻工厂在 SDK 活跃轮次的原生 `Stop` 接点执行检查。阻断时将 Provider 原因交给 Claude 请求继续。消费端收到带原生会话 ID 的 SDK `init` 消息时记录会话开始，确认断开后记录会话结束。这些接点适用于 Python SDK profile。同一原生控制请求 ID 的 Stop 重送复用当前轮次决策。阻断后收到新请求 ID 时重新执行策略。 |
| QwenPaw、AgentScope | 普通回复观察原生 `ReplyEndEvent`，保留中断、迭代耗尽等原因。继续已挂起的回复沿用原生回复 ID。此处覆盖回复轮次。 |
| Hermes | 普通本地 chat 使用 `on_session_start`、含轮次标识的 `on_session_end` 以及 `on_session_finalize`。Gateway 和 reloadable 模式另有原生边界。 |
| Pi | 工厂创建和释放时报告会话，原生 Agent 的 `turn_end` 记录模型停止原因，包含错误和中断。 |
| OpenClaw | 已验证构建的普通 Gateway 使用原生会话 Hook。首次 `before_agent_finalize` 以原生轮次或运行标识观察停止请求。 |
| Codex | 经原生信任确认的命令观察 `SessionStart` 和首次 `Stop`。启用相应事件后用 `aw connect` 补装 Codex Hook，并在原生端确认信任。 |
| DeepSeek Harness | 观察 Agent 创建及已提交的 `turn/end`，保留原生轮次结束原因。固定版本的 headless 入口在退出时缺少可确认的会话结束回执。 |

## 16 个通用 Hook

下表列出公共 Provider Hook 的实现状态，各 Adapter 的效果支持见上表。

| Hook | 状态 |
| --- | --- |
| `session.start` | 观察能力见生命周期接点 |
| `input.submit` | 普通 resident Qoder SDK / QwenPaw 文本、Claude SDK 前台/常驻文本工厂、Pi worker 文本输入、AgentScope 工厂文本回复、DeepSeek Harness headless 文本轮次及本地 Codex CLI已实现 |
| `tool.before` | 已实现 |
| `tool.after` | 已实现 |
| `permission.request` | 规划中 |
| `compact.before` | 规划中 |
| `compact.after` | 规划中 |
| `subagent.start` | 规划中 |
| `subagent.stop` | 规划中 |
| `turn.stop` | 观察能力见生命周期接点。Claude Python 和通用 SDK 支持阻断 |
| `session.end` | 观察能力见生命周期接点 |
| `model.before_request` | 规划中 |
| `runtime.observed` | 规划中 |
| `runtime.exited` | 规划中 |
| `security.violation` | 已实现于固定版本的 DeepSeek Harness 0.2.1-rc.5；OpenClaw 2026.9.5–2026.9.7 已验证构建的显式 OpenAI-completions Gateway 路径提供实验性支持；支持观察和阻断 |
| `coverage.changed` | 规划中 |

OpenClaw 最终检查要求精确构建、前台 Gateway 和非 reloadable 模式。上方静态矩阵描述普通启动能力。最终检查在原生结果和请求改写完成后检查模型请求；未知构建或路径拒绝必需策略。接线限制见 [OpenClaw 指南](../../../developer-guide/zh/aw/agents/openclaw.md#工具结果最终检查)。

Qoder SDK 1.0.55／1.0.56 仍只有后置 Post 改写反例和普通 Hook 覆盖。已验证的
1.0.57／Worker 1.1.81 前台 legacy Profile 在 Linux arm64/x64 提供上文所述最终检查。

## Skill 校验

Qoder SDK 1.0.56 / Worker 1.1.78 与 QwenPaw 2.2.2.post1 / AgentScope 2.0.10
在普通常驻模式下通过 `tool.before` 校验本地 Skill。sec-core Provider 的
`check_skill` 检查实际解析来源，拒绝时阻止指令进入模型。该操作尚不支持
`--reloadable` 和交互 `ask`，配置与来源限制见[安全指南](security.md#加载前校验-skill)。
两端均在[原生矩阵](../../../../src/aw/tests/native-matrix.json)注册 `skill-ledger`，
QwenPaw 的 `skill-objects` 另覆盖实际同名选择、来源固定与取消。

独立 AgentScope 2.0.6 复用相同的 Toolkit 解析器。原生 `skill-objects`
与 `skill-provider` 验收选择、对象固定、取消，以及受控 Ledger CLI/模型端点下的指令阻断。

Hermes 本地 chat 通过同一 `tool.before` 检查支持目录型 `skill_view`。
原生选择与来源范围见 [Hermes Skill 接入](../../../developer-guide/zh/aw/agents/hermes.md#skill-信任检查)。

Pi Coding Agent/Core 0.84.2–1.5.2 的已发布同版配对还可通过 `input.submit` 检查本地
`/skill:name` 输入。先固定选中目录的正文，再执行 Ledger 校验，prompt 与 PII
检查针对最终展开文本。接线与限制见 [Pi 指南](../../../developer-guide/zh/aw/agents/pi.md#校验-skill-输入)。
原生 `skill-objects` 与 `skill-input` 验收覆盖选中对象、来源变化、信任拒绝、取消及真实模型交付。

## 任务私有凭据

AgentScope SDK 2.0.6 工厂回复支持应用自有私有租约。工具显式消费执行环境，公开 Hook
和模型结果接收脱敏视图。申请、工具排空及撤销期间保持策略更新 busy。
见[工厂接入与清理限制](../../../developer-guide/zh/aw/agents/agentscope.md#任务私有工具凭据)。
原生验收在[原生矩阵](../../../../src/aw/tests/native-matrix.json)登记为
`private-lease` 和 `private-native`。

## MCP 与策略更新

| 入口 | 受管 MCP | 空闲策略更新 |
| --- | --- | --- |
| Qoder SDK | 进程内 handler，提供 Query 身份 | 普通或已准备的原生工厂；全部 Query 关闭后更新 |
| QwenPaw App | 显式包装 MCP 客户端调用 | 普通或已准备的原生 App；全部 reply 和清理排空后更新 |
| AgentScope SDK | 覆盖已暴露的原生工具回调 | 普通或已准备的原生工厂；reply、工具和 Scope 清理结束后更新 |
| Claude SDK resident | 进程内 handler，可压缩成功单文本结果，`report` 故障保留原始结果 | 已准备的原生工厂；全部客户端断开并完成清理后更新 |
| Pi SDK | 通过受管自定义工具接入 MCP 客户端 | 已准备的原生 worker 工厂；全部 Session 关闭后更新 |
| OpenClaw Gateway | 覆盖已暴露的原生工具回调 | 已准备的原生 Gateway；成功完成 run 并排空工具回调后更新 |
| Hermes 本机 chat | 覆盖已暴露的原生工具回调 | 已准备的原生 chat；确认轮次完成并排空回调后更新 |
| [Hermes Gateway](../../../developer-guide/zh/aw/agents/hermes.md#原生-gateway-启动与后台挂接) | 进程内原生工具回调 | 受控轮次和对应工作区绑定排空后更新 |
| Qoder CLI | 原生工具回调 | 已准备的原生 TUI／print；TUI 也支持首请求前 attach 或 `--reloadable`；须等待请求与清理结束 |

原生启动需要先按框架文档完成准备。SDK 应用保留 AW 工厂接入；OpenClaw、Hermes 和
QwenPaw 安装一次启动组件并重启。Qoder TUI 沿用独占 profile、首请求前 attach 的路径。
尚未接入的运行中进程没有就绪的 AW 实例。接点就绪并完成表中的清理后，执行
`aw update --agent TARGET` 即可加载默认 `aw.yaml`。普通 `aw run` 启动的
Pi、Claude SDK、OpenClaw 和 Hermes 仍沿用原有重启要求。Qoder SDK 和 QwenPaw
保留受限 `--reloadable` 模式，QwenPaw 另有受限的外部 attach 路径。具体入口与清理限制见
[策略更新](../../../developer-guide/zh/aw/deployment/policy-updates.md)。
MCP 工具需按实际输入语义配置安全检查，接线见 [Qoder SDK / QwenPaw MCP](../../../../src/aw/docs/design/zh/adapters/structured-mcp.md) 与 [Claude SDK MCP](../../../../src/aw/docs/design/zh/adapters/claude-mcp.md)。

Qoder SDK 应用还可接入固定摘要的 Bash 凭据消费者、Worker import 和声明式角色资产。
接线与权限要求见 [Qoder SDK 指南](../../../developer-guide/zh/aw/agents/qoder-sdk.md#原生-bash-命令消费者)。

Qoder SDK 保留原生配置并支持一次性预热，SDK/Worker 文件保持原样。
检查覆盖公开 Hook 可见的参数，后续原生改写没有最终执行前再次检查。
MCP、Pilot 接入与覆盖边界见[原生配置](../../../developer-guide/zh/aw/agents/qoder-sdk.md#原生配置)。

## 使用边界

- 普通 Qoder SDK、QwenPaw 可配置后置故障 `withhold_result`，暂扣结果并停止后续派发；已发生的工具副作用保留。
- AgentScope 2.0.6+ 的 tool.after 可配置 `withhold_result`：严格检查失败时在中间件链末端以错误态占位结果替换未检查原文，其余 after Hook 仍观察原始结果；仅在桥支持结果替换时准入。
- Pi 受管 tool.after 配置 `on_error: report` 时，Provider 或传输故障保留已检查的原始结果，同一会话可继续下一次 prompt；before 故障、tool.after 的 `block`/`withhold_result` 故障、取消、身份与清理失败仍停止任务。输入拒绝及 block 输入检查故障只拒绝该次 prompt，清理后允许继续提交。
- Claude SDK resident 的 `tool.after` 配置 `on_error: report` 时，已审计且可恢复的 Provider 故障保留原始结果并继续模型调用。范围包括原生 HTTP MCP 的文本及 `structuredContent` 结果、Bash 等内置工具；受管 SDK MCP 保留既有恢复行为。回调排空后，同一客户端可继续下一轮任务。身份/scope 失效、取消、生命周期/清理失败、before-Hook 故障、非法原生响应、未知传输详情及审计失败仍停止任务。`on_error: block` 与原生安全拒绝保留阻断语义。可恢复类别见[常驻部署](../../../developer-guide/zh/aw/deployment/resident.md)。后置检查无法撤销已执行工具。
- 上方矩阵列出各 Adapter 支持的公共 Provider 事件和效果。配置会话与轮次事件时，按生命周期接点选择受支持的启动模式。
- AW 通过原生 Hook 执行策略，OS 沙箱隔离需另行部署。

矩阵由[回归清单](../../../../src/aw/tests/regression-matrix.json)生成。[原生验收清单](../../../../src/aw/tests/native-matrix.json)记录固定版本及验证范围，操作方法见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。

框架版本沿用[原生接入检查](../../../developer-guide/zh/aw/native-agents.md)。记录的测试基线用于复现验收，其他版本可通过相同原生合同接入。


配置式 QwenPaw SDK 插件从 AW 文件选择输入或首次工具准入。
配置输入检查时，在模型派发前等待；仅工具策略时，模型推理与 AW 接入可重叠，
原生工具进入前固定 Scope。required resident 路径保留结果扣留和原生清理。
AfterOnlyPlugin 保持范围较窄的兼容合同。固定原生套件覆盖
QwenPaw 2.2.2.post1／AgentScope 2.0.10 的 after-only、before-only 和 input-only 配置。
见 [Python SDK 部署](../../../developer-guide/zh/aw/deployment/python-sdk.md#安装与接入)。

## 原生配置加载

通用 native-loader API 和离线来源引用编辑器保留原文件，区分声明、加载、准入和激活。
必跑的确定性消费者测试覆盖未知原生字段、来源顺序、凭据引用、更新和覆盖内调用失败。
内置 Adapter 的来源选择及原生配置限制仍以各 Agent 指南为准；通用 API 本身不解除
这些限制，也不代表业务 SDK 已验收。用法见[配置说明](configuration.md#记录原生配置来源)。

### Claude SDK 原生配置

| 原生能力 | 兼容情况 | AW 边界 |
| --- | --- | --- |
| 默认发现、user/project/local 来源及内联或文件 settings | 沿用 SDK 加载 | 原文件及来源优先级保持原生行为 |
| 常驻模型地址位于原生 settings | Anthropic Messages 上游沿用合并配置 | 接受 query 前确认会话中继路由，模型响应经过 AW |
| 项目 Skills 和本地插件 | 保留，并验证原生 CLI 发现 | 内部命令需要单独核对覆盖证据 |
| CLI 加载的 settings、项目或插件 Hook 改参 | 保留原生行为 | AW 检查公共 Hook 输入；改写后的普通工具命令可能执行，缺少最终参数的 AW 终检 |
| SDK、HTTP 和 stdio MCP 配置 | 原样保留；HTTP 租户请求头已覆盖验收用例 | 原生工具回调支持观察和前置策略；HTTP MCP 的 tool.after 压缩替换模型可见字符串（structuredContent 序列化或单文本块）并在下一模型请求生效；report 故障保留原投影 |

日常工厂回归覆盖原始选项和未知 settings 字段；固定版本原生验收覆盖 settings/Skills、
插件命令和 HTTP MCP。Bash 文本替换及显式常驻 SDK MCP 单文本替换沿用既有范围。
运行时与 callback 组合限制见 [Claude SDK 指南](../../../developer-guide/zh/aw/agents/claude-sdk.md)。

已验收的 SDK 0.2.88 / CLI 2.1.161 中，原生 `disableAllHooks: true` 关闭 settings
Hook，SDK 注册的 AW callbacks 仍执行。原生改参用例记录执行前终检缺口，并验证 AW
明确拒绝时工具不会执行；这些用例不构成最终参数强制检查验收。

配置式 Qoder SDK 接入使用原生公开 Hook 与协作 PID attach。关闭 `session.start` 和
`input.submit` 时，模型推理可先于挂接开始，首次工具准入固定 Scope。`qoder-public-hooks` 支持输入和工具前的
观察与阻断，也支持工具后观察和替换 `PostToolUse` 的受支持文本槽。`PostToolUseFailure` 提供
错误详情供观察。故障扣留和最终结果检查使用受管入口。见 [SDK 部署](../../../developer-guide/zh/aw/deployment/native-sdk.md#按配置接入-qoder)。
