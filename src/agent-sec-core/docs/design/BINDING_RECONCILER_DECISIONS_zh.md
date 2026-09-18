# Binding Reconciler 设计讨论记录

文档类型：`[TARGET V2]` 设计决定与取舍记录；实施进度和运行结果由 PR、CI 与验收报告记录。

[调度、存储与恢复详细设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)
记录 CR-010～CR-015 的目标约束。Runtime 集成使用 ready + entries、
Queued/Running/WaitingRetry/Exhausted 和 dirty；deployment 独立局部更新，无全局 resourceVersion；
每次 reconcile 都重新读取最新 Binding 并从头执行，plan/prepared/返回结果仅在本次调用内使用，
不跨调用缓存或落库。dirty 仅负责再次排队，不区分续做与重新开始。本文早期整体写入、
保存 prepared、跨调用补写结果和调度能力仅留 TODO 的目标在相应范围内被替代；
已有实现及历史验收说明不因此变为新设计证据。

PR 边界：Runtime/daemon 接线与单元、组件及调度竞争测试一起交付；完整端到端
测试单独开 PR；系统性 error injection 和进程崩溃/恢复测试待 persistent Repository
就绪后实施。完整 E2E 和系统性故障注入不作为 Runtime 集成 PR 门禁，既有相关回归继续维护。

共享边界补充：Adapter/Client trait 已抽取到 `asc-policy-target-contracts`，
参数和结果数据位于 `asc-policy-types::target`。Reconciler 与具体 Client 均依赖
该共享层；Client 不再依赖 `asc-pcp`，核心只重导出契约以保留本地消费者兼容。
共享运行记录和数据库读/CAS 接口位于 `asc-policy-repository`；认领、登记、结果合并、
重试决策留在核心，同 Binding 执行串行由 Runtime/WorkQueue 保证。memory repository 不依赖 `asc-pcp`；真实组件组合测试归核心，
Client 单独可测试。
本次仅调整依赖归属，不改变序列化、PEP 请求或状态机语义。

本文记录当前讨论中的已确认方向、已知限制及待细化事项，属于
`[TARGET V2]` 设计输入，不代表功能已实现或验收通过。
整体架构遵循 [Rust 迁移架构](AGENT_SEC_RUST_MIGRATION_zh.md)。

完整的目标状态表、API contract 修正清单、数据与事务、Client 接口、恢复流程、
实施工作包和验收矩阵见
[Binding Reconciler 设计与实现方案](BINDING_RECONCILER_DESIGN_AND_IMPLEMENTATION_zh.md)。
本文保留讨论依据；详细方案中的实现提案不代表已经实现或通过验收。

首版 Reconciler 自身的必过范围、完整 fixture/trace 要求、22 项核心场景和完成条件
已单独记录在 [Reconciler 验收标准](../../v2/fixtures/reconciliation/ACCEPTANCE.md)。
核心、Client 组合、daemon 和持久化各层的具体执行结果见
[验收报告](../../v2/fixtures/reconciliation/RESULTS.md)，不得跨层替代验证。

## 已确认方向

- PAP 同步保存 Binding；翻译和 PEP 下发由事件驱动的 Reconciler 异步执行。
- `bindingRevision` 只标识 spec：创建为 1，spec 改变时递增；Delete、同 spec
  失败重试和生命周期推进均不增版。
- 删除意图不可撤销：`PENDING_DELETE`、`DELETING`、`DELETE_FAILED` 拒绝所有 UPDATE。
  DeleteFailed 只能重试删除；所有目标确认 Absent 后移除 Binding 及运行数据。
  旧 ID GET/UPDATE/DELETE 返回 NotFound，LIST 不再包含它。重新部署 CREATE 新 ID、
  revision 1；AgentSight 的 UUIDv5 因 Binding ID 不同自然产生新目标身份。
- `APPLY_FAILED` 同 spec UPDATE 重置重试控制，保留 revision 和部署记录；每次执行都
  重新读取并准备。spec 改变才增版；重复 pending/running 请求不重置预算。
- `APPLYING` 期间允许用户 Delete：新删除意图将当前状态置为
  `PENDING_DELETE`，交给 Reconciler 处理；Delete 不增加 revision。
- 本服务按 Binding ID 互斥执行目标操作，Delete 任务不能与前一个 Apply/Update
  任务同时执行。旧任务的状态写回还需 revision/status CAS，不能覆盖新删除意图。
- PEP Client 封装 create/update/delete 的目标请求及必要的调用顺序；不读写
  repository，不通过回调修改 repository。请求前后的持久化均由 Reconciler 完成。
  Reconciler 不编排 PEP 特有的旧策略删除和新策略创建步骤。
- 当前讨论以更新时先清理旧策略、再创建新策略为通常流程；具体实现由 Client
  封装。这种流程可能在两个步骤之间暂时没有该 Binding 的生效策略。
- 使用 exponential backoff 重试；达到配置的次数上限后停止自动重试，并将
  当前意图对应的 Binding 更新为 `APPLY_FAILED` 或 `DELETE_FAILED`，保存错误。
  已被新意图覆盖的旧任务不能写入当前 Binding 的失败状态。
- Policy、Scope、Binding 继续只保留最新 revision 的完整副本；不能因恢复设计
  默认引入历史对象全量存储。

[请求状态表](../../v2/README.md)、PAP、Repository、协议与 fixtures 必须保持一致。
PAP 区分创建和条件更新，防止旧写入复活已删除记录；最终删除按事务条件完成，
再回收已确认无待写结果的执行槽位。请求准入、后台执行和 SQL 耐久性分别提供证据。

## 已确认：首阶段使用内存 repository

首阶段使用内存实现，不把 SQL 或跨重启恢复作为功能
实现阻碍。Binding、部署记录、稳定请求及重试预算仍统一交由 Reconciler/PAP 通过
repository 原子保存；Client 不访问 repository。

内存记录只在进程存活期间有效，重启后丢失，PEP 对象可能遗留。本记录所述持久化
顺序先作为逻辑保存顺序实现；磁盘耐久性、重启接续、迁移及 close/reopen 验收
保留到后续 SQL 阶段。不能把首阶段内存流程的通过结果描述为跨重启恢复已验证。

## 已知限制：跨服务的操作时序

问题示例：本服务发出 Apply 后超时，但 PEP 仍在处理；本服务随后发出 Delete，
若 PEP 先完成 Delete、后完成先前的 Apply，就可能重新产生生效策略。

该设计范围仅解决本服务内的任务互斥和状态写入竞态，不增加跨服务协作机制。
本地锁、CAS 和超时不能证明 PEP 已停止执行原请求，也不能保证上述远端时序。
跨服务的操作查询、版本约束或顺序保证作为后续协作问题保留；不得宣称当前
本地互斥方案已经解决该限制。

## 已确认：部署记录与清理责任

### 当前意图与目标记录分开保存

Binding 当前 spec 表达用户最新期望；Binding 的运行状态还需保存该逻辑 Binding
在 PEP 上待管理的部署记录。记录包含已确认存在及可能存在的目标，不只是
“已经部署成功”的列表。

下例是概念模型，不是已经实现或冻结的序列化 schema：

```text
BindingState
  status: PENDING_DELETE
  deployments:
    - pepBindingId: A
      bindingRevision: 1
      presence: PRESENT
    - pepBindingId: B
      bindingRevision: 2
      presence: UNKNOWN
```

- `pepBindingId` 是 PEP 上的目标身份；`bindingRevision` 关联产生目标的本地 revision，
  Delete 不改变这个来源版本；清理时不能统一换成 Binding 当前 spec 的 revision。
- `PRESENT` 表示最近一次明确确认存在，不承诺持续观测远端实时状态。
- `UNKNOWN` 表示可能存在，必须保留确认或清理责任；不能当作未部署。
- 还需能定位目标 PEP；固定单目标部署下可以由配置提供，具体字段待接口设计确定。
- 记录逻辑上属于 Binding 的运行状态，可内嵌保存或使用关联表；物理存储尚未选定。
- PAP 替换当前 spec/status 时不能覆盖或清空这份记录。部署记录由 Reconciler 管理，
  与请求准入对 Binding 当前意图的修改协调。
- 只有确认目标已删除或不存在，才能移除记录；也可以先记录 `ABSENT` 再回收。
  Apply/Update/Delete 请求、失败、超时、重试耗尽都不是回收依据。

### A 更新为 B 后立即 Delete

| 时刻 | 当前期望 | 部署记录及处理 |
|---|---|---|
| A 已生效 | Apply A | 保留 A |
| 用户提交 B，worker 尚未执行 | Apply B | 仍保留 A；尚不必有 B 的记录 |
| 用户立即 Delete | Delete | 删除意图不能清空 A |
| Delete worker 执行 | Delete | 根据部署记录调用 Client 清理 A |
| 确认清理完成 | Binding 不存在 | 原子移除 Binding 及全部运行记录 |

Delete 的语义是撤销整个逻辑 Binding 的部署，不是仅删除当前 revision 推导的
PEP 对象。如果 B 的执行已经开始，A、B 都可能需要清理；worker 必须读取实际
保留的目标记录。这样允许过期用户意图被合并，同时不丢失已产生或可能产生的副作用。

### 持久化时机

下表中的 repository 更新全部由 Reconciler 执行：

| 时点 | 部署记录处理 |
|---|---|
| 创建 B 或调用内部可能创建 B 的 update 前 | 保存 B 的目标身份为 `UNKNOWN`，保留 A；持久化成功后才允许调用 Client |
| Client 明确确认 B 生效 | 将 B 记为 `PRESENT` |
| 创建失败、超时、返回结果无法确认 | 保留 B；不能因调用失败推断目标不存在 |
| 删除 A 之前 | 保留 A，不提前移除 |
| Client 明确确认 A 已删除或不存在 | 移除 A，或先记为 `ABSENT` 再回收 |
| 删除失败、超时或无明确成功信号 | 保留 A 的清理责任，下次重试删除；不能将过去的 `PRESENT` 当作新的实时确认 |
| 目标结果已完成持久化 | 再以认领的 revision/status CAS 推进 Binding 完成状态 |

请求前保存 B 后，即使进程尚未发请求就退出，恢复时仍保守处理 B；多一次幂等
确认或删除是可接受的。请求前保存失败则不发请求。远端成功而结果记账失败时，
不能报告 Binding 完成，保留已有记录用于恢复。

在当前“先删除 A、再创建 B”的封装下，调用顺序为：

```text
Reconciler：保存 B 为 UNKNOWN，保留 A
  → Client.update：执行 PEP 请求，返回确认结果或错误
  → Reconciler：保存已确认的目标结果，保留不确定目标
  → Reconciler：CAS 更新当前 Binding 生命周期
```

Client 不需要每执行一步就回调 repository。它可以在部分失败时返回已经确认的
操作结果；如果进程退出而没有返回，Reconciler 保守保留 A、B 并通过重试恢复。
部分结果的具体接口尚待定义。

### 任务互斥与新删除意图

同 Binding 的目标请求及随后记账由 Runtime/WorkQueue 的 Running entry 保证串行；
不同 Binding 可以并发。核心不维护第二把执行锁。Running 不阻止 PAP 接受 Delete 并保存
`PENDING_DELETE`，新通知只设置 dirty，待前一个 Apply/Update 调用退出后再领取。

旧任务即使被新删除意图覆盖，仍可按原目标身份保存部署结果，但不能把旧任务的
完整 Binding 快照写回。旧任务的成功、失败和重试状态写入必须使用认领的
revision/status CAS，不能将 `PENDING_DELETE` 改回 `READY`、`PENDING_APPLY`
或 `APPLY_FAILED`。Delete worker 获得锁后重新读库并清理所有未确认不存在的目标。
部署记录更新与生命周期 CAS 如何形成明确的 repository 原子接口，仍需细化。

Delete 不增加 revision，因此旧 Update 完成时即使 revision 仍匹配，也必须检查
`expected_status == APPLYING`。若当前已是 `PENDING_DELETE`，状态更新应失败。
revision 与 expected status 的检查及状态写入必须在 repository 中原子执行；
同 Binding 单 worker 不能替代这个检查，因为 PAP 仍可并发接受删除意图。

## 已确认方向：Client 部分完成与恢复

### 目标身份和请求准备由具体 PEP Client 负责

- AgentSight plan 的 `source` 提供 Binding、Policy、Scope 的身份和版本信息；
  `scopeRevision` 从完整 Binding 的 Scope 快照复制，是否用于请求由 Client 决定。
  当前 Client 校验其类型但不用于 UUID、清理参数或 HTTP 请求；缺失字段的旧 v1
  plan 仍可读取，新 Adapter 固定输出该字段。由于旧 Client 严格拒绝未知字段，
  应先更新 Client reader，再启用新 Adapter；prepared 格式与字节不变。
  Policy ID/revision 对应 AgentSight 的产品策略归属字段，目标部署身份独立使用
  Binding ID/revision。Adapter/Client 的完整 golden 与 Scope revision 变体测试
  验证来源版本保留以及请求内容不受该信息字段影响。
- Reconciler 不统一计算 PEP bindingId。AgentSight Client 用 SecCore Binding ID +
  revision 计算 UUIDv5；其他 PEP 若能直接使用 SecCore Binding ID，其 Client 可以
  原样使用。Reconciler 只保存、比较并回传 Client 提供的不透明目标身份，不复制
  UUIDv5 算法或强制所有 PEP 使用 UUID。
- Adapter 负责翻译策略及范围；Client 的 `prepare_apply` 负责本地校验、目标身份
  生成/映射、必要的进程身份解析和请求编码，不执行目标创建，也不更新 repository。
- `apply_prepared` 表示执行创建请求，可在通用能力中命名为 `create(prepared)`；
  不是创建前的逻辑。具体 Rust 名称尚未冻结，不要求两个同义接口同时存在。
- prepared 是 Client 定义和解释的目标产物。Reconciler 只在本次调用中原样
  传递它，重试重新准备；不解析或重写其中的 DSL、HTTP 字段和 process start time。
- AgentSight 新请求在没有产品 Agent 身份时明确发送 `agent_id: ""`，不得用
  `scope_id` 或其他资源身份代填。当前通用 `/api/enforcement/bindings` 接口允许
  空字符串。此修正同时用于直接 Apply 和 prepared 创建；本次已有 prepared 保留原始
  请求字节及归属字段，发送时不改写。对应 Client wire/prepared fixtures 与
  `scope_identity_is_never_used_as_agent_attribution`、
  `existing_prepared_attribution_is_replayed_without_rewriting` 测试锁定此边界。
- `update` 封装 PEP 的更新机制；AgentSight 的先删后建属于具体 Client，其他 PEP
  可以原地更新。通用 Reconciler 不编排 PEP 内部步骤。
- 通用顺序是 Adapter 翻译 → Client 准备 → Reconciler 登记 UNKNOWN 目标 → Client
  创建或更新 → Reconciler 结果记账及状态 CAS。Client 始终不访问或回调 repository。

这些是已确认的职责边界，不代表接口已经实现；首阶段仍使用内存 repository。

### 部分成功与重放

以旧目标 A 更新为新目标 B 为例：

1. Client 成功删除 A。
2. 创建 B 明确失败，或 daemon 在调用创建前退出。
3. 本次 update 未完成，但删除 A 的副作用不会随函数失败或进程退出回滚。

如果 B 已创建成功、daemon 在记录成功前退出，则重试还必须识别或幂等复用 B。
这些是 Client 契约要覆盖的故障情形，不是要求 Reconciler 理解所有内部步骤。

已选方向是保守保留目标记录并使用可幂等重放的 Client 操作：旧对象已不存在时，
删除可以确认完成；同一新目标已存在且内容一致时，创建重试需能确认原操作结果。
固定请求和 mock HTTP 组合应有独立验证，不能仅因目标 ID 稳定就宣称真实远端幂等成立。
update 重试应区分旧目标集合与本次新目标，不能因为新目标已登记就把它误作旧目标
先删除，也不能把目标存在但内容不一致直接当作成功。

仅在幂等重放不足以支持恢复时再评估额外执行进度；当前不要求复杂 checkpoint
协议。Client 接收多个待清理目标、返回部分确认、固定请求重放的可执行契约已实现；
repository 跨重启恢复仍依赖后续 SQL，不由请求可序列化直接证明。

## 已确认：有界重试与失败后保留记录

- 核心可重试故障使用 exponential backoff；达到配置的次数上限后停止自动重试。
- Apply/Update 最终失败写 `APPLY_FAILED`；Delete 最终失败写 `DELETE_FAILED`。
- 失败状态和错误写入仍受当前意图 revision/status 约束。
- 重试次数需要持久化，避免重启后重新计数而变成无限重试。次数口径、退避参数、
  下次执行时间与中断尝试计数见 Runtime 设计；跨重启的耐久性由后续 SQL 工作包验证。
- `DELETE_FAILED` 仍保留所有未确认不存在的部署记录，供后续用户重新发起删除。
  `APPLY_FAILED` 同样不能清空仍可能存在的目标。
- 只有所有待清理目标均已明确确认不存在且结果已持久化，才允许条件删除整个 Binding 聚合。
- 错误是否可重试与目标是否存在是两个判断；不可重试错误也不能作为删除记录的依据。

Runtime 另对自动重调度设进程内预算：默认首次调用后最多 4 次，RetryAt、Superseded 和
仓储错误共用计数。Superseded/仓储错误固定等待 1 秒，RetryAt 按期限等待；耗尽后保留
Exhausted 条目并输出诊断，补扫不再激活，不假定失败已写入数据库。这个队列预算不跨
Runtime 重建，不替代核心已提交的业务预算；持久化重试恢复仍由 SQL 阶段验收。

## 用户错误表达：方向明确，字段待定

用户不需要理解 Reconciler 内部阶段。建议 Binding 展示操作结果、简明原因、
自动重试是否停止，以及有证据支持的策略生效情况；内部步骤、目标 ID 和调用
细节留在脱敏诊断日志中。字段和错误码尚待定义。

示例：所有关联旧目标及新目标均明确确认不存在，且已停止自动重试时，可说明
“策略更新失败，当前该 Binding 的策略未生效；自动重试已停止”。新策略被拒绝
本身不足以证明目标不存在，尤其同一操作曾超时的情况下。若创建请求超时而结果
不明，只能说明“策略更新失败，当前生效状态无法确认”，不能断言策略不存在。

失败状态不自动意味着远端无策略，也不代表已回滚；本记录未约定自动回滚能力。

## Runtime 调度与负载控制

通知去重、dirty、有界并发和补扫属于 Runtime 集成范围；PEP 请求速率限制仍延后。

### 按 Binding ID 合并通知

Runtime 队列只保存 Binding ID 和调度元数据，worker 重读当前意图。
Queued 重复通知合并；Running 通知设置 dirty；WaitingRetry/Exhausted 通知取消旧等待或停止标记并立即排队。
新通知重置队列自动重试计数，dirty 在当前调用退出后优先处理；核心业务预算仍由 Repository 决定。
通知与 finish 原子协调，确保当前调用实际退出后才开始下一次调用。

### 负载控制与首版边界

首版提供 4 个 worker、有界队列、FIFO、退避和稳定 ID 分页补扫。队列满不回滚已提交意图，
由补扫重新发现；等待退避释放 worker。耗尽条目仍占容量，需要新通知触发后完成才能释放。
PEP 速率限制、跨进程恢复与持久化队列不在本次范围。
具体默认值、组件接口和验收以[Runtime 设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)为准。

## 问题复核与开发前剩余工作

“已解决”在本节表示讨论中的设计选择已明确，不表示代码或测试已经实现。

| 前述问题 | 结论 | 剩余工作 |
|---|---|---|
| revision 与 PEP 身份 | 已实现：严格 spec-only；删除不可撤销，完成后移除；重新 CREATE 新 ID | 按详细方案第 3 节和 CR-001 至 CR-008 对齐代码、契约及 fixture |
| 相同 spec 失败后重新 Apply | 区分自动重试、新请求与删除后的新部署 | APPLY_FAILED 显式重试不增版但重新准备；自动重试也从头读取和准备；旧部署记录保留至明确 Absent |
| API 文案笼统承诺 UPDATE/DELETE 总是返回 PENDING | 幂等调用实际返回已有状态，例如 READY/DELETING；删除完成后 NotFound | 明确返回原子准入时的当前 BindingView，验收重复请求及请求后 GET/LIST |
| ING 期间 Update/Delete 准入 | 已明确：保持 Update 约束，允许 APPLYING 期间接受 Delete | 更新 PAP、状态定义、repository 原子准入及请求状态表，增加竞争测试 |
| 等待期间连续 Update、重复或乱序事件 | Runtime 重读当前意图并保留部署记录，合并通知并保留 dirty | 以 Runtime 竞争测试验证通知可靠性 |
| A 更新 B 后立即删除导致 A 清理丢失 | 已解决：部署记录独立于当前 spec 保留，Delete 清理全部关联目标 | 定义持久化 schema 及原子更新接口 |
| 请求前还是请求后更新记录 | 已解决：请求前保存身份，返回后记录确认结果 | 验证各崩溃窗口和 repository 写失败路径 |
| Client 是否更新 repository | 已解决：仅 Reconciler 更新，Client 只执行目标请求并返回结果 | 定义 Client 输入、输出及部分失败类型 |
| 谁计算目标 ID、解释 prepared 和执行 PEP 更新步骤 | 已明确：具体 PEP Client；Reconciler 使用不透明身份与请求产物 | 接口及 fake Client 验收不得强制 UUIDv5 或泄露 AgentSight 专有字段到通用流程 |
| Client 先删后建部分完成 | 原则明确：保留不确定目标，幂等重放 | 验证重放所需完整输入、目标分类及 PEP 幂等行为 |
| 无明确删除成功信号时如何处理 | 已解决：保留记录，下次重试删除 | 明确 Client 认可的成功/不存在响应并锁定测试 |
| 重试耗尽后怎么办 | 已解决：停止自动重试，写失败状态及错误，保留部署记录 | 冻结次数口径、退避配置及持久化时序 |
| 用户是否需要理解内部步骤 | 方向明确：展示结果、简明原因、重试是否停止及有依据的生效情况 | 定义公开字段、错误码及日志脱敏边界 |
| 跨服务 Apply 晚于 Delete 完成 | 明确延期，不属于该设计范围 | 保留限制，不将本地锁或 CAS 当作远端执行顺序保证 |
| 提交成功但通知丢失、重启后 ING 卡住 | 进程内事件交接和补扫归 Runtime；跨重启恢复延后 SQL 阶段 | Runtime 验证补扫和 running 恢复交接；持久化工作包验证重启恢复 |
| 大量任务、队列去重和 throttle | Runtime 包含有界队列、并发池、去重和退避；PEP 速率限制延后 | 队列竞争和容量验收在本阶段；需要时另增 PEP 速率限制 |
| 如何证明实现符合设计 | 核心 fixtures、真实 Adapter/Client 的 mock HTTP 组合及分阶段集成验收 | 运行结果见 RESULTS.md；完整 E2E 独立 PR，恢复与系统性注入随持久化阶段完成 |

实施需持续覆盖四组契约（核心内存及 Client 部分已落地，PAP/调度/SQL 分阶段推进）：

1. **数据与事务**：部署记录与当前意图的独立更新，目标记账与最终状态的一致性，
   Binding 与意图的原子保存，首阶段由内存 repository 同一临界区实现；后续 SQL
   再实现耐久性。不能仅依赖 worker 串行执行弥补 repository 更新丢失。
2. **Client 接口与幂等**：请求前可获得目标身份，结果表达部分确认及不确定。本次 prepared
   原样交给 Client；下一次重新读取、翻译和准备。AgentSight 只校验本次准备的进程身份，
   不保留跨次进程连续性依据。删除后重新 CREATE 使用新 Binding ID、revision 1。
3. **调度、状态与重试恢复**：WorkQueue 的 Running 边界、dirty、容量、并发、补扫、退避和
   shutdown 在 Runtime 集成验收。完成或耗尽预算的 Binding 不因补扫自动重试。
   跨重启恢复依赖后续 SQL；PEP 请求速率限制另行实施。
4. **接口投影与验收**：公开错误字段，以及对竞争、部分成功、崩溃恢复的可执行
   验收。fixture 应比较完整输入输出和有序调用；首阶段内存结果不能代替后续 SQL
   耐久性或真实 PEP 生效证据；跨服务时序限制需保持显式标注。

上述是详细设计和实现验证工作，目前无需重新确认已选定的产品方向。
持续漂移修复、多 PEP 聚合、分布式 worker、原子替换和自动回滚不因本次讨论
自动进入 P1 范围。

## 源码与契约入口

- [PAP repository](../../v2/crates/asc-pap/src/repository.rs)：请求准入与条件更新端口。
- [BindingView](../../v2/crates/asc-policy-types/src/binding.rs)：spec/status 与生命周期类型。
- [AgentSight Client](../../v2/crates/asc-agentsight-client/src/client.rs)：目标准备、执行和结果分类。
- [Runtime 设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)：局部存储写、每次从头执行、调度及恢复边界。

相关文件、变更记录编号与后续验收用例详见
[详细方案的 API contract 清单](BINDING_RECONCILER_DESIGN_AND_IMPLEMENTATION_zh.md#4-api-contract-修正与后续清单)。
