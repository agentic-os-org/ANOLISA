# 本地 Provider Host

[English](../../en/runtime/provider-host.md)

[设计索引](../README.md) · [架构总览](../architecture.md)

`aw-host` 将已校验的配置和 `aw-provider/v1alpha1` 协议接到 Linux 命令执行层。
它执行真实的 Provider 进程，返回已检查的候选效果与可关联的调用报告。
Agent 启动、原生 Hook 安装、效果采用和审计持久化仍由接入方负责。

## 组合边界

| 层 | 职责 |
| --- | --- |
| `aw-config` | 解析期望配置并校验静态引用 |
| `aw-provider` | 检查协议消息、离线预检与能力准入 |
| `aw-exec` | 按字面参数执行命令，提供有界字节传输与所属进程清理 |
| `aw-host` | 保留执行上下文，准备 Provider 并调用已准入的事件步骤 |
| 接入 Adapter | 提供可信能力、归一化事件、调度步骤并应用原生效果 |

Host 复用这些库，离线准入仍不执行命令，原始传输层仍不解释 Provider JSON。
原生步骤显式选择 `native-hook/v1alpha1`；Host 使用同一执行器保留原始字节和
原生退出状态，不执行 Provider 握手。
`aw-host` 不实现独立的 `aw-core::Host` 合同，不生成 Core receipt，也不改变
Core 的最终执行约束。

## 准备固定上下文

调用 `Host::prepare(bytes, target, capabilities, context, deadline, cancelled)`。
`bytes` 是完整配置文档；每次调用保留的 SHA-256 修订摘要由这些原始字节确定。
`target` 选择配置中的 Agent。调用方提供实际版本及入口对应的
`AdapterCapabilities`，Provider 声明不能提供或扩展这份可信证据。

`ProcessContext` 提供绝对路径 `cwd`、完整的子进程 `environment` 和独立的
`stderr_bytes` 上限，不会隐式继承父进程环境。需要搜索可执行文件时显式传入
`PATH`。Host 保留的命令、私有配置、Adapter 证据和进程上下文不可变；任一项变化
都需要创建新的 Host。这固定的是值，不是可执行文件或依赖内容；调用方仍需负责
它们的可信性与稳定性。

准备阶段先解析完整配置，再对事件/步骤选择器交集可能匹配所选 Adapter 的启用
需求调用 `admission::preflight`，之后才启动 Provider。确定不适用于当前目标的
步骤不进入计划；无法确定的标准名/原生名交集继续严格准入。Host 准备和原生注册
使用同一组步骤，revision 身份仍保留完整文档。每个被引用的结构化 Provider 分别启动进程执行 `describe` 和
`validate_config`；原生命令按显式原生步骤合同准入，不执行这两项调用。布尔 Hook
（省略 `protocol` 或填写 `aw-hook/v1alpha1`）同样跳过这两项调用，preflight 在本地
准入配置的命中效果，不执行脚本。仅被禁用项
引用的 Provider 会跳过。已检查的响应成为
`admission::admit` 的证据，最终只保留全部通过准入的步骤。失败时不会返回部分
准备好的 Host，也不会回滚此前完成的调用，因此准备方法应避免副作用。

准备成功表示本地协议交互通过，不代表原生 Hook 已安装、绑定进入 `Ready` 或
Agent 已采用策略。准入范围仍为：

| 事件 | 候选效果 | 失败动作 |
| --- | --- | --- |
| `tool.before` | `observe`、`block` | `report`，或在 Adapter 支持时使用 `block` |
| `tool.after` | `observe`, `replace_result` | `report`，或 Adapter 显式提供失败动作能力时使用 `withhold_result` |

启用但不受支持的事件、guard、`ask`、输入替换效果及超出
`native_hook` 的保证均被拒绝。完整准入合同见[协议设计](../../../../../../docs/developer-guide/zh/aw/providers/protocol.md)。

## Adapter 执行语义

对于已准入且包含一个末尾替换步骤的 `tool.after` 链，下表区分原生调度与 AW 配置
步骤顺序。AW 链之外的 Hook 保留原生调度。

| 框架 | 原生顺序 | 替换点位 | AW 观察步骤看到的结果 | AW 是否在服务内部排序 | 完成/关联点位 |
| --- | --- | --- | --- | --- | --- |
| Qoder | 回调并行，或显式配置原生 `sequential` | `PostToolUse` 返回 `hookSpecificOutput.updatedToolOutput` | 原始回调结果 | 是：此前 AW 步骤尝试结束后执行下一步，不串行化外部 Hook | 独立回调共享不可变原生输入和事件句柄 |
| OpenClaw | 结果 middleware 先于完成事件，原生 after handler 并发执行 | `registerAgentToolResultMiddleware` | 压缩前结果的脱敏视图，以及原生完成元数据 | 否：插件在替换后的完成阶段串行执行 AW 观察步骤 | 原始 middleware 快照绑定替换和观察阶段，首次完成视图被固定 |
| QwenPaw | Acting middleware 按注册顺序逆序返回 | `on_acting` 返回的最终 `ToolResponse` | 原始完成结果 | 否：反向注册使 AW 观察步骤按配置顺序返回，再执行替换 | 回调共享完成响应和工具调用身份，中间块不投影 |
| AgentScope SDK | Acting middleware 按注册顺序逆序返回 | `on_acting` 返回的最终 `ToolResponse` | 原始完成结果 | 否：反向注册使 AW 观察步骤按配置顺序返回，再执行替换 | 回调共享完成响应和每次调用的新身份，中间块不投影 |
| Hermes | 完成回调先于最终结果转换 | `transform_tool_result` 返回最终字符串 | 原始完成结果 | 否：原生回调注册保留 AW 观察步骤顺序 | `post_tool_call` 和 `transform_tool_result` 共享绑定，仅归一化点位名称和观察阶段独有的 middleware trace |

每种框架均共享一个事件截止时间和步骤单次执行声明。已报告的失败不会延长预算；
Qoder 在失败尝试结束后释放后续步骤，缺失回调则让事件过期。服务中存在显式的 Qoder
排序及 OpenClaw/Hermes 关联分支；这是当前受支持 Adapter 的合同，不是与框架无关的
调度保证。绑定和取消细节见[共享事件生命周期](local-service.md#共享事件生命周期)。

可执行合同位于
[`posttool.rs`](../../../../crates/aw-cli/tests/posttool.rs)、
[`openclaw.mjs`](../../../../crates/aw-cli/tests/openclaw.mjs)，以及原生
[QwenPaw](../../../../crates/aw-cli/tests/fixtures/qwenpaw/native_middleware.py) 和
[Hermes](../../../../crates/aw-cli/tests/fixtures/hermes/native_contract.py) fixture。
[AgentScope 生命周期验收](../../../../crates/aw-cli/tests/agentscope_lifecycle.py)
另通过公共 SDK、真实 Tokenless CLI 和离线模型验证接入。
Fixture 通过本身不证明真实模型采用。

## 缺省只调用 invoke 的 Hook

布尔 Hook 只使用 `invoke`，步骤中也可省略。Host 在内部绑定并校验调用，只将
标准化事件交给命令，并要求退出码 0、stdin 完整交付以及一个 JSON 布尔值。
Hook 的大小和深度限制只作用于裸事件，内部封装不占用事件额度；结构化 Provider
请求仍遵循完整消息限制。
true 投影步骤准入的全部 effects 和可选 reason code，false 不请求效果，AW
自行构造并校验关联 outcome。用户脚本无需协议字段或 effect 响应对象。布尔
stdout 同时受 32 字节和配置上限约束，使用普通执行器立即清理进程组。共享
deadline、取消、一次调用声明和失败处置沿用 Host 规则。显式结构化 Provider
保留握手和响应校验；失败的握手不会当作缺省方法或重试为布尔 Hook。

## 调用一个事件

用归一化的 JSON 事件创建 `Host::event(value, original_deadline, cancelled)`。
事件中的 Adapter 和绑定必须与已准备的 Host 一致。原生实例、会话与工具调用标识
仍是事件数据，不构成已认证的身份。启动步骤进程前会校验调用 Schema。

返回的 `Event` 只有一个截止时间，取原始回调截止时间与从 `Host::event` 入口开始
计量的配置事件预算中的较早者。传入原始截止时间，才能让归一化等调用前工作也计入
回调预算。每次交互另受 Provider 超时限制。编码、执行和响应校验都计入截止时间；
迟到或已取消的结果不能成为成功的策略结果。准备阶段同样在发现调用间共享调用方
传入的截止时间。
协议中的整数 `budget_ms` 将正的剩余时间向上取整到毫秒，不足一毫秒的余量也如此。
实际限制仍使用精确的 `Instant`，取整不会延长截止时间。

原生适配器可调用 `Event::invoke_with_native_environment(step_id, snapshot)`，
为原生命令提供实际回调环境。只有原生传输替换绑定环境，结构化 Provider 仍使用
准备阶段上下文。原有 `Event::invoke` 沿用绑定环境。快照最多 4096 项、1 MiB，
拒绝非法键和 NUL，不改变事件身份或期限，也不进入 stdin 或执行报告。非法快照
占用步骤执行权并返回明确故障。

用 `Event::steps()` 选择保留的步骤，再调用 `Event::invoke(step_id)`。
迭代器按配置顺序保留全部已准入步骤，包括工具选择器未匹配的步骤。
`invoke` 在回调归一化、绑定检查和步骤单次占用之后，对事件与步骤的 `match.tools`
取交集。任一层省略或使用 `["*"]` 时选择全部工具。标准名称匹配规范化的
`tool.name`；`native:<adapter>:<name>` 精确匹配 `agent.adapter` 和保留的
`tool.native_name`。匹配区分大小写，不提供 glob 或正则模式。

未匹配调用先校验规范化事件、检查取消和共享截止时间，再返回
`StepOutput::Skipped`；不启动 Provider 或 Hook 调用进程，也不产生候选效果。
启用的结构化 Provider 仍须完成准备阶段的发现和配置校验。跳过步骤保留单次占用、
关联记录及正常完成路径，使后续步骤和终态清理继续执行。
服务在完成元数据审计中记录 `kind: skipped`。非法、过期或已取消事件仍按配置的
`on_error` 返回失败。

调用方取得配置顺序，按原生回调合同决定串行或并行调度。同一个 Event 内的并发调用仍
共享截止时间。每个步骤在每个 Event 中只能被占用一次，失败尝试也不例外；不会
自动重试。创建另一个 Event 不会对原生回调去重。执行工具前，Adapter 必须收集
其合同要求的结果。

结构化交互采用一个方法一个进程、字面 argv、单条 JSON 请求和单条 JSON 响应。
原生步骤使用 `Host::hook_event`，同时提供原始回调字节和归一化事件。原始输入在
该 Event 内不可变，原生命令直接接收它，不包裹 AW 协议。
传输层保留独立的一秒清理预算，因此返回时间可能晚于事件截止时间。无法验证清理
完成仍属于执行失败。进程组清理不提供 sandbox 或 OS 执行约束；详见
[有界命令执行](bounded-execution.md)。

## 解释报告

`Invocation.result` 保留 `StepOutput` 或原始 `Failure`。
`StepOutput::Skipped` 记录选择器未匹配，没有命令进程或效果；Adapter 返回中立的
原生回复，保留待确认权限请求和拒绝。不受支持的输入改写仍被拒绝。Tokenless
PreTool 建议须由新调用重新经过选中的安全步骤和原生权限检查；PostTool 替换仍只
作用于末尾获准的文本槽，保留原生 envelope 与失败状态。
`StepOutput::Provider`
包含已校验的结构化结果，`StepOutput::Native` 包含 stdout、stderr 和原生进程状态。
原生非零退出是交给 Adapter 解释的结果，不是结构化 Provider 错误。以下效果与
响应校验规则适用于结构化 Provider 结果。成功的 `block` 效果属于
策略结果；非零退出、stdin 未完整写入、传输失败、Provider 错误或无效协议响应
属于执行失败。`failure_action` 另行给出配置要求的 `Report` 或 `Block` 失败
动作，不会把失败结果替换成成功的策略阻断。空效果列表不增加限制，也不授予原生
权限。实际工具决策和效果采用由 Adapter 负责。

`CallRecord` 关联配置修订、绑定、Provider、请求、方法、事件和步骤，并在可用时
保留耗时和原生进程事实。`Host::preparation()` 暴露成功的准备调用，
准备过程后续失败时，`Error::Preparation(Box<PreparationFailure>)` 在 `completed`
中保留此前成功的调用，并通过 `cause` 保留原始错误。`Error::Call` 类型的原因携带
失败调用；最终准入、截止时间或取消检查失败也保留此前调用历史。早期配置、预检和上下文
错误仍直接返回，此时没有启动进程。调用失败保留在返回报告中，步骤选择无效或重复
则返回 `Error`。报告是本地数据，不是已持久化的审计记录或 Core receipt。原始
stderr 单独保留，不会自动写入日志；审计写入方必须脱敏或省略诊断与敏感载荷。

## 本地示例

在 Linux 上从仓库根目录运行：

```bash
cd src/aw
cargo build --locked -p aw-provider --example policy
cargo run --locked -p aw-host --example host -- crates/aw-host/examples/aw.yaml tool.before DeleteFile
cargo run --locked -p aw-host --example host -- crates/aw-host/examples/aw.yaml tool.after ReadFile
```

[Host 示例](../../../../crates/aw-host/examples/host.rs)读取
[专用配置](../../../../crates/aw-host/examples/aw.yaml)，将当前目录的绝对路径作为 `cwd`，
并调用 `./target/debug/examples/policy`。示例提供合成 Adapter 证据和工具事件。
工具前示例为 `DeleteFile` 返回候选阻断，工具后示例返回空效果列表。两者均不启动
Agent 或执行原生工具。样例策略不是 sec-core，也不是安全规则集。

测试与 workspace 门禁见[参与 AW 开发](../../../../CONTRIBUTING_zh.md)。本地进程测试
验证传输与协议行为；原生验收仍需实际 Agent 已安装回调并采用返回效果的证据。
