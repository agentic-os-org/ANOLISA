# 本地 Provider Host

[English](provider-host.md)

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

准备阶段先解析配置，并对全部启用需求调用 `admission::preflight`，之后才启动
Provider。每个被引用的结构化 Provider 分别启动进程执行 `describe` 和
`validate_config`；原生命令按显式原生步骤合同准入，不执行这两项调用。仅被禁用项
引用的 Provider 会跳过。已检查的响应成为
`admission::admit` 的证据，最终只保留全部通过准入的步骤。失败时不会返回部分
准备好的 Host，也不会回滚此前完成的调用，因此准备方法应避免副作用。

准备成功表示本地协议交互通过，不代表原生 Hook 已安装、绑定进入 `Ready` 或
Agent 已采用策略。准入范围仍为：

| 事件 | 候选效果 | 失败动作 |
| --- | --- | --- |
| `tool.before` | `observe`、`block` | `report`，或在 Adapter 支持时使用 `block` |
| `tool.after` | `observe` | `report` |

启用但不受支持的事件、精确工具选择器、guard、`ask`、替换效果及超出
`native_hook` 的保证均被拒绝。完整准入合同见[协议设计](provider-protocol_zh.md)。

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

用 `Event::steps()` 选择保留的步骤，再调用 `Event::invoke(step_id)`。调用方可以
取得配置顺序，按原生回调合同决定串行或并行调度。同一个 Event 内的并发调用仍
共享截止时间。每个步骤在每个 Event 中只能被占用一次，失败尝试也不例外；不会
自动重试。创建另一个 Event 不会对原生回调去重。执行工具前，Adapter 必须收集
其合同要求的结果。

结构化交互采用一个方法一个进程、字面 argv、单条 JSON 请求和单条 JSON 响应。
原生步骤使用 `Host::hook_event`，同时提供原始回调字节和归一化事件。原始输入在
该 Event 内不可变，原生命令直接接收它，不包裹 AW 协议。
传输层保留独立的一秒清理预算，因此返回时间可能晚于事件截止时间。无法验证清理
完成仍属于执行失败。进程组清理不提供 sandbox 或 OS 执行约束；详见
[有界命令执行](bounded-execution_zh.md)。

## 解释报告

`Invocation.result` 保留 `StepOutput` 或原始 `Failure`。`StepOutput::Provider`
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

[Host 示例](../../crates/aw-host/examples/host.rs)读取
[专用配置](../../crates/aw-host/examples/aw.yaml)，将当前目录的绝对路径作为 `cwd`，
并调用 `./target/debug/examples/policy`。示例提供合成 Adapter 证据和工具事件。
工具前示例为 `DeleteFile` 返回候选阻断，工具后示例返回空效果列表。两者均不启动
Agent 或执行原生工具。样例策略不是 sec-core，也不是安全规则集。

测试与 workspace 门禁见[参与 AW 开发](../../CONTRIBUTING_zh.md)。本地进程测试
验证传输与协议行为；原生验收仍需实际 Agent 已安装回调并采用返回效果的证据。
