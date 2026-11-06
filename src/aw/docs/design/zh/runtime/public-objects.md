# 公共运行与审计对象

[English](../../en/runtime/public-objects.md)

`aw-contracts` 为运行事实生产者和 AgentSight 等消费者定义统一元数据合同。对象包含
apiVersion、kind、metadata 和 data；配置继续使用既有 spec 合同。库提供离线校验和
不可变引用校验。aw-service 在原生 Hook 和启动器边界生产这些事实，独立于可选的
AgentSight 查询；公共审计传输和 Sight 摄取通过各自集成消费已保留的元数据。

## 已注册结构与 API

| 注册名 | 对象身份 | 职责 |
| --- | --- | --- |
| object-meta-v1alpha1 | 无独立 kind 的值对象 | 公共元数据及引用、主体、关联、覆盖的 $defs |
| execution-event-v1alpha1 | runtime.aw/v1alpha1，ExecutionEvent | 执行边界上的元数据 |
| audit-record-v1alpha1 | audit.aw/v1alpha1，AuditRecord | 一个保留的源记录链位置对应的元数据投影 |

权威结构位于 [aw-contracts/schemas](../../../../crates/aw-contracts/schemas)。
Registry::object_schema 从这些资源解析精确的版本/kind 组合。Registry::validate 校验
具名 schema 的结构；Registry::validate_object 进一步校验关联、范围、来源身份与结果
不变量，日期格式校验包含实际日历日期。外部 JSON 首先经过 canonical::parse，拒绝
重复键、不安全整数、浮点数表示及大小/深度超限。

```rust
use aw_contracts::{canonical, objects, Error, Registry};
use serde_json::Value;

fn checked_reference(registry: &Registry, input: &[u8]) -> Result<Value, Error> {
    let object = canonical::parse(input)?;
    objects::reference(registry, &object)
}
```

objects::reference 校验目标并返回带内容摘要的版本化引用；objects::validate_reference
逐字段核对引用。objects::validate_audit_event 还核对审计中复制的事件名和主体。独立
审计对象通过校验，只说明自身一致；取得目标事件并完成引用校验后才能建立该关联。

## 公共字段

对象拒绝未知字段，新字段统一使用 lowerCamelCase。公共 schema 提供值对象词汇，
不会启动可配置的对象服务。

| 值对象 | 字段与含义 |
| --- | --- |
| Metadata | uid、createdAt、producer；事实不可变，无可变 status 或任意 labels |
| Producer | producerId、scopeId、generation；由实际认证的部署范围约束并保留来源身份 |
| Reference | apiVersion、kind、uid、完整 producer、digest；引用保留来源范围和代际 |
| Digest | algorithm 固定 sha256，value 为 64 位小写十六进制 |
| Execution subject | adapterId、bindingId、instanceId、configRevision；可空 sessionId、turnId、toolCallId |
| Native references | 可空 sessionId、turnId、toolCallId、modelCallId；保留原生值，与归一化身份分开 |
| Association | status、basis、verifiedFields、gaps；验证仅覆盖列出的归一化主体字段 |
| Event times | 可空 occurredAt、必填 receivedAt；metadata.createdAt 为保留的对象创建时间 |
| Observation coverage | status、scope、interval、observedAt、gaps、truncated；声明一个主体内指定事件集合的覆盖 |

元数据、生产者、binding 和链 ID 为 1–128 个可打印 ASCII 字符。归一化会话/轮次/调用
及原生标识允许 1–512 个不含控制字符的 Unicode 字符，禁止截断。Adapter ID 为
1–64 个小写 ASCII 字母、数字和连字符，以字母开头；格式合法不代表已注册或具备
原生能力，运行准入继续负责这一判断。

生产者随审计存储保留 producerId；scopeId 标识经过认证的部署范围。每个运行时写入
生命周期分配新的 generation，历史对象保留原值。ExecutionEvent 生产者捕获事实时
一次性分配不透明 uid，保证在完整生产者身份内唯一，持久保存后用于重放。读取者不
按到达时间或原生调用 ID 重新生成。binding/instance ID 由运行时管理；会话、轮次和
工具 ID 在所属绑定、实例及会话上下文内解释。原生 ID 相等不能单独确定公共对象身份。

时间固定为 UTC 的 YYYY-MM-DDTHH:MM:SS.sssZ。原生时间缺失、适用但未知的身份使用
null，不填空串或虚构 ID。配置版本取原始配置字节摘要。输入/输出摘要取捕获时所选
规范化值的 UTF-8 JSON 序列化字节，不保留原生 JSON 拼写，也不证明正文已保留。对象摘要复用现有整数域 AW 元数据
规范编码。元数据不包含原始提示词、工具参数/结果、环境或原生回调外壳。

verified 要求非空 verifiedFields 和明确 basis，列出的主体字段必须有值。原生 ID
不能在归一化对应字段缺失时出现。unassociated 不携带请求身份，basis 为 none，
verifiedFields 为空，并携带 gap；其他未验证关联也须说明 gap。生产者可以验证工具
身份，同时保留未知的原生轮次 ID。库校验一致性，认证与原生依据由生产者提供。

覆盖范围指定 instance、turn 或 tool 层级及一个以上事件种类。轮次/工具覆盖要求
对应主体身份。complete 必须有两个区间端点、无 gap、无截断；unknown、partial、
unavailable 必须有 gap，已知端点不得逆序。完整声明仅覆盖指定事件和区间，不证明
扫描质量、eBPF 就绪或来源真实。该类型与现有 common/v1 的扫描覆盖分开。

### 原因码归属与词汇

原因码使用 `<owner>.<code>`，两部分均以小写字母开头，允许小写字母、数字、下划线
和连字符，总长度不超过 128 个字符。`aw` 命名空间由 AW 保留；外部生产者使用约定
的归属命名空间，例如 `agentsight`。命名空间格式不认证生产者身份。AW 按字段用途
维护原因码：

| 用途 | 原因码 | 含义 |
| --- | --- | --- |
| `association.gaps` | `aw.native_identity_unverified` | 可用回调未证实请求身份 |
| `association.gaps` | `aw.native_turn_unavailable` | 原生轮次身份不可用 |
| `association.gaps` | `aw.instance_has_no_request` | 实例级事件没有请求身份 |
| `coverage.gaps` | `aw.coverage_unmeasured` | 尚未测量观测覆盖 |
| `coverage.gaps` | `aw.source_unavailable` | 当前无法访问观测来源 |
| `coverage.gaps` | `aw.journal_corrupt` | 保留的 journal 数据未通过解码或完整性校验 |
| `coverage.gaps` | `aw.history_incomplete` | 已有历史缺少预期片段或终态记录，原因尚未确认 |
| `coverage.gaps` | `aw.history_missing` | 无法定位请求的历史 |
| `coverage.gaps` | `aw.retention_gap` | 已知保留策略操作删除了部分请求历史 |
| `coverage.gaps` | `aw.consumer_lag` | 消费者相对来源存在已知积压 |
| 生命周期 `reasonCode` | `aw.native_adoption_unconfirmed` | Hook 处理结束，但缺少原生效果采用证据 |
| 生命周期 `reasonCode` | `aw.launch_failed` | 启动器启动或就绪失败 |
| 生命周期 `reasonCode` | `aw.worker_failed` | Hook 事件 Worker 失败 |
| 生命周期 `reasonCode` | `aw.expired` / `aw.stopped` | 事件期限或服务停止终止处理 |
| 生命周期 `reasonCode` | `aw.cancelled` | 轮次被取消 |
| 生命周期 `reasonCode` | `aw.owner_lost` | 确认终态前失去执行所有者 |
| 生命周期 `reasonCode` | `aw.cancel_cleanup_pending` | 取消清理尚未完成 |
| Provider `failureCode` | `aw.provider_timeout` | Provider 未在期限内完成 |
| Provider `failureCode` | `aw.transport` / `aw.exit` / `aw.incomplete_input` | 传输、进程退出或输入未完整交付失败 |
| Provider `failureCode` | `aw.native_environment` / `aw.provider` / `aw.protocol` | 原生环境不合法、Provider 结构化错误或协议失败 |

这些原因码规定生产者需要实现的语义。journal/历史和消费滞后原因码供后续公共审计
与 Sight 集成使用，当前合同库不检测这些状态。新增 AW 原因码须在对应用途的表中
评审登记；已定义语义保持稳定，变更须明确版本策略。

Schema 校验命名空间格式，并接受未知原因码以支持后续扩展。消费者保留未知码，将
未知 gap 展示为未分类，同时保留声明的关联/覆盖状态，不根据子串推测含义，也不将
无法识别的 gap 视为完整覆盖。关联缺口描述身份依据，覆盖缺口描述观测完整性；
消费者结合字段用途解释原因码。

### 时钟语义

occurredAt 使用原生来源的墙上时钟，receivedAt 使用 AW 生产者的墙上时钟，二者的
时钟关系未约定。两种先后顺序均合法，包括 occurredAt > receivedAt；两者相减不构成
已定义的延迟或新鲜度指标。metadata.createdAt 为生产者保留的对象创建时间，
coverage.observedAt 为生产者评估覆盖的时间。

生产者将 coverage.interval 的两个端点表达在同一时钟域中，已知端点满足 start <= end。
无法映射到该时钟域的端点保留 null，并说明覆盖缺口。将原生事件时间与区间端点比较，
需要生产者或消费者显式建立时钟映射。

语义内核桥接层须在自身约定的时钟域内确定证据捕获、判定和有效期限，并在该时钟域
执行判定预算。直接将 receivedAt 作为判定时间，或跨来源比较 UTC 字符串，不能建立
新鲜度保证或 captured_at <= decision < valid_until 关系。本合同携带描述性时间戳，
时钟映射与判定期限由接入的消费者负责。

## ExecutionEvent 分支

data 包含 name、subject、nativeRefs、association、times 及分支专属 details：

| 名称 | 关联形状 | details |
| --- | --- | --- |
| instance.started | 所有请求/原生引用为空 | 报告的 pid，仅作描述，不认证进程 |
| instance.ended | 所有请求/原生引用为空 | outcome、exitCode、signal、reasonCode |
| input.submit | 会话、轮次有值，工具调用为空 | inputDigest |
| turn.ended | 会话、轮次有值，工具调用为空 | outcome、reasonCode |
| tool.before | 工具调用有值，会话/轮次可未知 | name、nativeName、inputDigest |
| tool.after | 工具调用有值，会话/轮次可未知 | 前述三项及 resultDigest、resultStatus |

实例 exited 必须且只能有退出码或信号之一，无 reasonCode；failed/incomplete 实例
携带原因码，退出字段为空。轮次终态区分 completed、failed、cancelled、incomplete，
只有 completed 的原因码为空。工具结果为 success/error/interrupted/denied；前置事件
禁止结果字段。这些事实名称不向 Adapter 矩阵新增 Provider Hook、原生回调或能力。

## AuditRecord 身份与结果

data 包含 kind、eventRef、eventName、subject、chain、coverage 和 details。
eventRef 指向同一生产者的 ExecutionEvent，包括相同 scope 和 generation。chain
包含 id、安全整数 sequence、保留的私有 envelope 的 sourceDigest。

| 记录种类 | details | 含义 |
| --- | --- | --- |
| event.observed | 空对象 | 捕获的事件被记录 |
| provider.completed | providerId、stepId、requestId、status、policyBlock、failureAction、failureCode | 策略尝试完成，不声明原生采纳 |
| event.closed | outcome、reasonCode | 事件生命周期结束或仍不完整，不自动解释为工具/轮次成功 |

Provider status 为 ok 或 failed。调用成功没有 failureCode/action；policyBlock 独立
表示 true/false，未知时为 null。失败调用有原因码、report/block/withhold_result 动作，
策略判断为空。后置记录不得声明阻止工具执行的 block，前置不得声明扣留输出。
本版本 Provider 结果仅适用于 input.submit、tool.before、tool.after 边界。

objects::audit_uid 对下列规范编码数组计算 SHA-256：

```json
[
  "audit.aw/v1alpha1", "AuditRecord",
  {"producerId": "aw-1", "scopeId": "scope-1", "generation": "generation-1"},
  "chain-1", 1
]
```

读取者重启时保留原始来源代际；本版本每个源位置最多投影一条记录。同位置内容变化
保持 UID，改变对象摘要，
让消费者识别冲突。去重键使用生产者身份与 UID，再比较摘要；哈希不证明认证或持久化。

运行时持久化创建时间与确定性字段，重放保留原生产代际。新服务代际不能恢复旧运行的写权限。

## 运行事实生产与投影

aw-service 将通过校验的 ExecutionEvent 存入现有 Journal 初始预留记录的
plan.executionEvent，源记录链 key 同时作为对象 UID。Provider 完成和关闭记录在
持久化前保存创建时间。FileJournal format-1 封套、摘要链和既有私有字段的含义保持。
投影从已保留的封套计算 sourceDigest，源记录不包含引用自身的摘要。

aw_service::public_projection::execution_event 校验初始公共对象；project_record
逐条投影已校验封套。没有初始公共对象时明确表示私有或历史记录链；公共对象损坏
返回错误。步骤派发和原生 Hook 命令完成等中间记录没有公共 AuditRecord 投影。
重复读取保持 producer、UID、时间、序列和摘要。

审计目录中的私有 .aw-audit-source.json 只包含 apiVersion: audit.aw/v1alpha1、
kind: AuditSource 和随机的 64 位十六进制 sourceId。该存储描述独立于已注册公共
Schema。创建时原子发布并同步文件及目录，文件属于服务用户、单硬链接、模式 0600，
目录保持 0700。producerId 为 aw-service，scopeId 为保留的 sourceId，generation
为写入服务代际。显式读取拒绝缺失或损坏的描述；服务初始化为尚未初始化的存储创建
描述，历史记录保持原样。

| 运行边界 | 记录事实与范围 |
| --- | --- |
| aw run 子进程启动与等待 | evidence 关闭时仍记录 instance.started/ended；PID 仅为描述信息 |
| 已配置的输入/工具 Hook | 捕获元数据及 Provider 完成记录；没有原生采用证据的关闭为 incomplete |
| 无策略步骤的 Qoder SDK/QwenPaw 工具回调 | 默认元数据路由记录 before/after，不启动 Sight 工作线程，不保留正文 |
| 原生结果 | QwenPaw 使用明确的 ToolResponse state；SDK 使用成功/失败 Hook 类型及明确错误标记；未知状态记录 gap |
| 受管调用退休 | 既有 invocation ledger 阻断重放后，回收回调去重细节 |
| 未受管回调保留 | 每实例最多 256 个回调身份；溢出持久记录一次 aw.observation_callback_limit gap，后续新身份被拒绝 |

缺少必要公共元数据时，私有 gap 记录链保留 aw.native_metadata_unavailable。
既有 Scope 生命周期记录继续保留 parent/owner 信息；Scope ID 不替代原生 turn ID。
公共工具主体保留实际任务实例及已核实的 session/call 关联，nativeRefs 将原生 ID
与 AW 规范化 invocation ID 分开保留。缺少明确原生轮次身份时不生产 turn.ended。

覆盖保持 unknown，原因为 aw.coverage_unmeasured。观测链 completed 表示捕获完成，
Provider 完成记录表示策略输出；两者均不证明原生效果采用。公共元数据不包含输入/
结果正文、环境、凭据或回调封套。Journal 失败保持现有服务阻断行为，来源读取不重试执行。

## 兼容与集成

现有配置、Provider 请求/响应、Evidence、Core 合同及已注册 Schema 字节/ID 保持。
新增私有记录包含捕获元数据；旧读取器继续读取 format-1 Journal，旧记录链明确没有
公共投影。投影将私有 status:error 映射为公共 status:failed，并区分缺少的策略判断。

未知对象版本和字段被拒绝。发布定义保留资源 ID 与确切字节，结构或语义变化需要
明确版本管理。回退保留已有额外元数据和来源描述，不转换或重放历史记录链。

合同测试覆盖六个事件分支与不可变引用。服务测试覆盖 Sight 关闭、原生关联、去重与
退休、未知结果、持久化失败、来源身份并发初始化和跨服务代际重放。QwenPaw 原生
验收进一步验证同一 Agent 策略更新后的无策略事实。真实托管 Qoder Worker 采用与
Sight 存储按[集成验收](../integrations/agentsight/agentsight-integration.md)分别验证。
