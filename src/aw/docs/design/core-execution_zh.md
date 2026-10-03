# Core 执行与存储

[English](core-execution.md)

`aw-core` 通过可信接入端口执行已由策略确定的能力计划。它复用既有注册合同
Schema 和校验器；`aw-contracts` 保持无副作用，也不依赖 Core。

## 准备与执行

调用方创建 `Core`，实现 `ProviderHost` 和 `Clock`，并提供包含计划、原生边界、
运行时绑定和每步一个 `StepInput` 的 `PrepareRequest`。`prepare` 在任何 Host 调用
之前准入所有选定 descriptor 和 invocation。指定的 Provider 不可用会拒绝整个
请求；显式空路由作为 gap，按计划指定的失败策略处理。

准备结果为不可变的 `PreparedPlan`。事件键绑定完整 scope 和 event ID；更换计划
不能使同一事件重复运行。调用方必须为每次原生边界触发分配独立 event ID
（包括分别触发的 pre/post），并在重试时保留该 ID。

`execute` 消费准备好的计划，在 `Journal` 中预留事件，然后串行运行各步。分发前
再次检查 Provider descriptor 和绝对截止时间，包括 Journal 写入之后。每步所选
Provider 都被计入结果后才归并决策。拒绝、保留原文或取消会停止后续步骤。生成
的终态记录必须通过计划及实际调用证据校验，才能返回。

Host 负责认证 Provider、实现其协议并限制执行和输出。Core 校验返回证据和本地
观测的调用时间，但无法抢占同步 Host。取消在调用之间及调用返回后采样。接入方
提供可信时钟，确保运行时与边界权限持续有效，并处理进程退出或重启的竞态。

## Journal 回执

Core 在分发前写入 claim 和 invocation start，继续执行前写入 receipt 和步骤结果。
每个回执立即通过 `Registry::validate_evidence` 按既有 common/v1 evidence 定义
校验。中间写入返回形状错误的成功结果同样会报错。这仅证明引用的形状正确；
持久性和记录真实性仍由 Journal 实现保证。终态执行记录最后一次 append 获得
确认后，才向调用方返回。

`FileJournal` 在 Linux 上使用可信、服务所有的本地目录，通过原子创建文件及
同步写入保存记录。新建目录和文件使用私有权限。claim 或写入失败仍保留事件
预留；append 失败后该 writer 不可继续写入。其他对象或进程不能接管已有 claim
的写入权。`Journal::release` 关闭本地写入权并移除内存记账，持久预留和记录保持
不变。Core 在返回或栈展开时释放每次成功 claim 的 writer，包括错误回执、取消及
Host 或存储错误。claim 失败时，必须清理本次尝试取得的资源，不能释放已有 writer。
直接使用 Journal 的调用方必须 release 自己的 claim 或丢弃 backend。release 不得
panic 或执行可能失败的存储操作；释放后 append 会被拒绝。
当前没有重试、回滚、删除 claim 或恢复 API。

Format-1 envelope 包含 sequence、previous digest、record 和 digest，这是私有
存储格式，不是另一套 AW wire schema。读取拒绝半条记录和损坏的链；完整前缀
本身无法揭示尾部记录被删除，`read_verified` 需要与独立保留的回执比较链尾。
哈希无法防止攻击者同时重写两份数据。其他操作系统需要另行实现 `Journal`。

Journal 记录计划元数据、ID、摘要、回执和决策，不保存原始能力输入或输出。
`Execution::calls` 在内存中保留这些载荷；日志、保留策略和制品访问由接入应用控制。

## 原生所有权与验证

`Execution::record`、`calls` 和 `journal_ack` 暴露关联的执行事实。`proceed` 结果
不代表原生分发许可或采用证据。实际工具动作前，原生 owner 必须取得最新 intent
和 OS 证据，并让 `Registry::validate_dispatch` 与动作原子衔接。结果采用需要
独立的原生回读及 `Registry::validate_plan_adoption` 校验。

在仓库根目录运行 `python3 src/aw/scripts/check.py`。入口检查两个 crate，要求
执行和 Journal 测试有非 ignored 用例，并强制已评审的依赖边界与 Rust 源文件
大小限制。Core 测试使用合成 Host、受控时钟和隔离的临时 Journal，覆盖错误回执、
失败、截止时间、取消、重复事件、重启、并发 claim、有界 writer 生命周期和损坏或截断的存储。
这些测试不认证掉电行为、生产 Provider 接入或原生 Agent 采用。
