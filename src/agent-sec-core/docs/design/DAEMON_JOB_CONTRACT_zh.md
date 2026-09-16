# AgentSec daemon Job 生命周期与执行契约

| 属性 | 值 |
| --- | --- |
| 状态 | V1 Python production Job 行为基线及最小化 Rust JobSupervisor 目标 |
| 实现核对日期 | 2026-08-21 |
| 实现核对提交 | `fe58ed4b23b8` |
| OTel V2 目标决策日期 | 2026-08-25 |
| 适用实现 | 当前 Python daemon oracle；目标 Rust daemon |

## 1. 文档地位

本文是 AgentSec daemon 后台 job 注册、调度、生命周期、状态、日志、trace、失败、取消、
恢复和健康投影的语言无关权威规范。V2 产品和进程架构以仓库内迁移总计划为准；本文记录
V1 oracle，并定义 asc-daemon-core 首版 JobSupervisor 必须承接和明确延后的
语义。权威关系见
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)。
daemon transport、权限和启停见
[`DAEMON_CURRENT_BEHAVIOR_zh.md`](DAEMON_CURRENT_BEHAVIOR_zh.md)，job 相关 wire method 和
`daemon.health` envelope 见 [`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md)。

本文不规定 Python 类层次、Rust crate、async runtime、线程模型或 channel 实现。Python
和 Rust 可以采用不同内部结构，但必须满足相同的可观察行为和 contract fixtures。

本文使用以下范围标记：

- **[CURRENT]**：当前 Python 实现已经具备的行为；
- **[PRESERVE V1]**：supported V1 兼容接口在兼容期必须保持的外部行为；
- **[TARGET V2]**：与仓库内迁移总计划一致的 Rust 新增或统一能力，不能作为 Python golden；
- **[HISTORICAL]**：只作为历史依据，不构成当前或目标要求。

本文未显式标记的当前可观察行为只属于 **[CURRENT]**；是否 PRESERVE V1 由 per-crate
compatibility inventory 和 fixture 决定。Python worker、class 和 per-user 进程形态不是
V2 要求。

## 2. 概念与职责边界

### 2.1 Preparation、Background service 与 Run

Rust 首版必须先区分 readiness-critical preparation 和 daemon ready 后的后台工作，不能把
所有启动期动作都抽象成 Job：

| 概念 | Ready 关系 | 当前示例 | Rust 首版承接位置 |
| --- | --- | --- | --- |
| 安装/升级 preparation | daemon 启动前，由部署流程完成 | 文件布局、密钥 provision、V1 到 V2 状态迁移 | installer、init container 或显式 `asc-state-migrator`，不属于 daemon Job |
| Daemon bootstrap | 完成后才可以 Ready | 配置、权限、认证材料、runtime、数据库打开和 schema compatibility 校验 | composition root 中的有序初始化与 rollback，不属于后台 Job |
| Background service | 由 daemon 启动并长期存活；不等待首个 run 完成即可 Ready | `skill-ledger-activation` actor | 最小化 JobSupervisor |
| Background run | service 的一次具体处理 | startup reconcile、一次 debounced Skill 变更批次 | service 内的一次有界执行 |

V1 Skill Ledger 的 startup reconcile 虽然由 daemon 启动触发，但它不阻止当前 socket bind，
语义上属于 background run，而不是 readiness-critical preparation。V1 production registry
没有注册具体 periodic Job；此陈述不涵盖 V2。V2 Binding reconciliation 的通知、重试定时器、
补偿扫描及 DJOB 验收映射见第 11.4 节，不要求引入通用 periodic scheduler。

长期 background service 的运行状态不能代替最近一次 run 的结果。Rust 首版只需在 health
中区分 `running/degraded/stopped` 和最近一次 outcome，不需要预先实现两套可扩展状态机。

### 2.2 daemon、Supervisor 与领域逻辑

Rust 首版 JobSupervisor 只负责：

- 静态注册、确定性启动和停止；
- 持有全部长期 task、取消信号和 terminal join；
- 轻量 health snapshot 与安全的结构化诊断；
- 为实际执行批次建立 OTel span；
- 在 shutdown 时停止接收 trigger 并完成 drain 或 cancel。

trigger、debounce、合并和当前 retry 语义由第一个具体 service adapter 承担；只有出现第二个
具有相同需求的 production Job 后，才评审是否上移为通用 scheduler/policy。

领域 handler 负责：

- 解释类型化 payload；
- 调用所属领域 application service；security action 经 asc-action-runtime 和
  CapabilityExecutor；
- 返回领域结果或分类后的错误；
- 保持领域持久化、锁和幂等语义。

Job adapter 不得复制 capability、middleware lifecycle、SecurityEvent、telemetry 或
redaction 业务逻辑。V2 daemon 直接调用 Rust application service，不保留用于复用 Python
领域逻辑的永久 worker。

## 3. **[CURRENT]** 当前 Python Job framework

### 3.1 注册、启动与停止

当前 Job manager 按注册顺序保存 Job，并提供 `register/get/start_all/stop_all/status` 的逻辑
能力。默认 registry 恰好注册一个 production Job：`skill-ledger-activation`。仓库中的通用
one-shot/periodic 类及其测试证明 Python 原型具备这些调度能力，但没有对应的默认 production
Job，因此它们属于 **[CURRENT][HISTORICAL]** framework evidence，不是 Rust 首版功能需求。

daemon 启动时的可观察顺序为：

1. 完成认证 key、runtime directory、lock 和 stale socket 的前置校验；
2. 按注册顺序启动全部 Job；
3. bind socket；
4. 若 Job 启动或 bind 失败，停止已经进入启动流程的 Job，并完成 socket、lock 和 umask
   回滚；
5. socket 可接受请求后记录 `daemon_started`。

daemon 正常停止时先停止接收并 drain 在途请求，再按注册逆序停止 Job，最后清理 socket、
lock 和 umask。SIGTERM、SIGINT 和 daemon task cancellation 必须进入同一清理路径。

当前 Python `JobManager` 不拒绝重复名称；这是 **[CURRENT]** 内部缺口，不属于
**[PRESERVE V1]**。兼容测试不得依赖重复名称或 `get()` 返回第一个同名对象。

### 3.2 当前 Job 接口

当前通用逻辑接口只有：

| 操作 | 语义 |
| --- | --- |
| `start` | 启动长期任务或安排一次运行；不得阻塞 daemon 的整个服务周期 |
| `stop` | 请求停止并等待该 Job 拥有的任务、worker 和必要清理结束 |
| `status` | 返回不初始化重型能力的轻量、可序列化快照 |

Python 的 `BackgroundJob`、`OneShotBackgroundJob` 和 `PeriodicBackgroundJob` 类名不是
跨语言 API；Rust 不需要复制继承结构。

### 3.3 当前 health schema

`daemon.health.data.jobs` 是按注册顺序排列的数组。每项至少包含：

| 字段 | 类型 | 当前语义 |
| --- | --- | --- |
| `name` | non-empty string | 稳定 Job 名称 |
| `state` | string | 当前 Job 状态，值域因 Job kind 而异 |
| `last_error` | string/null | 最近一次未清除的错误文本 |
| `last_tick_at` | ISO-8601 string/null | 最近一次开始处理工作的时间 |

通用 periodic/one-shot 原型可以增加以下字段；值未设置时当前序列化会省略，而不是写
`null`。这些字段当前不会由默认 production registry 产生，属于
**[CURRENT][HISTORICAL]**：

| 字段 | 类型 | 适用范围 |
| --- | --- | --- |
| `interval_seconds` | positive number | periodic |
| `last_started_at` | ISO-8601 string | periodic/one-shot |
| `next_run_at` | ISO-8601 string | periodic |

当前观察到的 `state` 值如下：

| Job kind | 值域 | 说明 |
| --- | --- | --- |
| one-shot | `stopped/running/completed/error` | 成功后进入 `completed`，异常后进入 `error` |
| periodic | `stopped/running/error` | 单次失败暂时为 `error`，后续 tick 仍会继续 |
| Skill Ledger activation | `stopped/running/error` | 单次处理失败不会终止循环，后续成功可恢复 `running` |

当前 schema 将长期 Job 状态和最后一次 run 结果部分混合。Rust 首版只需为实际注册的
`skill-ledger-activation` 保留 `name/state/last_error/last_tick_at` 兼容投影；不得因为未注册的
通用原型提前固化 periodic/one-shot optional 字段。

## 4. **[CURRENT]** 当前调度语义

### 4.1 **[HISTORICAL]** One-shot 原型

- `start()` 安排一次后台执行并立即返回，不等待 Job body 完成；
- Job 已在运行时重复 `start()` 不会启动第二份并发执行；
- 成功、失败或取消分别形成 completed、failed 或 cancelled lifecycle；
- `stop()` 取消尚未完成的 task 并等待取消清理。

### 4.2 **[HISTORICAL]** Periodic 原型

- `start()` 后第一次 run 立即到期；
- 每个周期以该次 run 的开始时间为锚点；
- run 超过一个或多个周期时跳过已经错过的边界，不进行补偿性连续执行；
- 同一 periodic Job 不重叠执行多个 run；
- 一次普通异常被记录为失败，但 loop 继续等待后续 tick；
- 每个 tick 使用独立的 daemon-owned trace ID。

上述两节只用于解释和测试当前 Python 通用框架，不是 supported 外部能力，也不是 Rust
首版 no-regression gate。

### 4.3 **[PRESERVE V1]** Skill Ledger activation

当前默认 Job 是一个自定义 event-driven loop，不通过通用 one-shot/periodic lifecycle
wrapper：

1. 启动后状态为 `running`；
2. 枚举显式 managed Skill 目录；
3. 为每个目录 enqueue 一个 `eventKind=reconcile`、空 paths 的启动事件；
4. 收到 SkillFS notify 后以 canonical Skill directory 为 key 暂存；
5. 默认 debounce 500 ms；
6. 逐个处理合并后的目录变更。

同一 canonical directory 的合并规则为：

- `eventKinds` 和 `paths` 取并集；
- 后到且非空的 reported Skill ID 覆盖旧值；
- 第一次加入返回 newly queued，已经存在返回 coalesced；
- drain 过程中到达的新事件进入下一批并重新等待 debounce；
- task cancellation 时，当前批中尚未处理的变更重新放回 pending。

默认目录发现项不等于 managed 目录；startup reconcile 只处理 resolver 明确返回的 managed
Skill 目录。

## 5. **[CURRENT]** Worker、错误、重试与取消

### 5.1 **[PRESERVE V1]** 当前 Python worker 外部语义

当前 activation Job 延迟启动一个串行、持久 Python worker 子进程。每次只处理一个
Skill 变更，单次默认 timeout 为 300 秒。

以下错误属于 worker transport failure：

- worker 启动或 stdio 失败；
- broken pipe、EOF 或 OS communication error；
- response 超限、协议无效或 request ID 不匹配；
- timeout；
- response 缺少必需 result/error。

transport failure 会终止当前 worker，并对当前变更最多重新启动并重试一次，即总尝试数
最多为 2。worker 返回的领域执行错误是 business failure，不得触发 worker 重启或自动
重试。

stop 时先关闭 worker stdin 并等待 2 秒；仍未退出则 terminate，再等待 5 秒；仍未退出则
kill。该进程协议和三阶段终止流程是 **[CURRENT]** Python 实现细节。Rust core 完成后可以
进程内执行，但必须保持业务错误不自动重试、transport/执行失败可区分、取消不被解释为
工作尚未发生等外部语义。

### 5.2 Job failure isolation

- **[HISTORICAL]** one-shot 未处理异常结束该原型 run，并把状态设为 `error`；
- **[HISTORICAL]** periodic 未处理异常结束当前原型 run，但不终止后续周期；
- Skill Ledger 某个变更失败时记录 `last_error/last_processed`，继续处理后续通知；
- 每次 Skill Ledger 处理在创建密钥、扫描、签名或更新 activation 前加载并校验配置。
  配置错误以 `ConfigError` 上报，本次处理无上述副作用；修正配置后可处理后续通知，
  不自动重试已失败变更；
- Job runtime failure 当前不会自动停止 daemon 或改变 daemon 顶层 `status`；
- Job 启动阶段抛错则 daemon 启动失败并执行资源回滚。

timeout 或 cancellation 只说明调用方停止等待或发出了取消请求，不证明 blocking、文件、
SQLite、签名或外部进程副作用尚未发生。任何有副作用的 run 不得仅因 timeout/cancel 自动
重放。

## 6. **[CURRENT][PRESERVE V1]** 队列、接受与恢复语义

Skill Ledger pending 是 daemon 内存中的按目录合并 map，不是持久消息队列。notify 返回
`accepted=true, queued=true/coalesced=true` 表示运行中的 Job 已接受到内存 pending，
不表示变更已经执行、持久化或具备 exactly-once 保证。

daemon 在接受后崩溃可能丢失尚未处理的 pending。下一次启动会对显式 managed Skill 目录
执行全量 reconcile，用当前文件和持久化事实状态重新求值；不能承诺重放每一条历史 notify
或保留原通知顺序。

兼容迁移不得把 `accepted` 偷换为持久队列承诺。未来若要求 durable acceptance、operation
status 或 exactly-once，必须单独设计持久 operation ID、恢复日志和版本化 wire 语义。

当前 pending 没有已固化的容量上限或 queue-full wire error。Rust 兼容实现不得擅自引入
会拒绝当前可接受请求的新上限；容量与 backpressure 必须作为独立产品决策和协议变更评审。

## 7. **[CURRENT]** 日志与 trace

### 7.1 **[HISTORICAL]** 当前通用 lifecycle event 原型

通用 one-shot/periodic wrapper 定义：

- `daemon_job_started`；
- `daemon_job_completed`；
- `daemon_job_failed`；
- `daemon_job_cancelled`。

当前事件 data 使用以下字段子集：

| 字段 | 语义 |
| --- | --- |
| `job_name` | Job 稳定名称 |
| `job_kind` | `one_shot` 或 `periodic` |
| `state` | 事件发生后的当前状态 |
| `latency_ms` | terminal event 的单次 run 延迟 |
| `interval_seconds` | periodic 周期 |
| `error_type/error_message` | failed event 的异常类型和文本 |

one-shot 的 started 与 terminal event 使用同一个新 UUID trace ID。periodic 每个 tick 使用
一个新的 trace ID，同一 tick 的 started 与 terminal event 共用该 ID；run 结束后必须恢复
之前的 ambient context。

当前默认 `skill-ledger-activation` 是自定义 loop，不承诺逐批产生上述通用 lifecycle event，
也不承诺把 notify request trace 传播到后续 debounced 执行。上述 one-shot/periodic event
只用于表征 Python 原型，不是 V1 production golden，也不是 Rust 首版必须复制的 event 数量。

本节 UUID trace 是 **[CURRENT][HISTORICAL]** correlation 行为，不是 OTel TraceId。V2
不得把这些 UUID 解析、hash 或 padding 成 OTel TraceId。

### 7.2 当前日志安全边界

daemon 诊断日志是 JSONL。Job 日志不得改变 daemon request 或领域执行结果；Rust panic、
Python traceback、认证材料、passphrase、未脱敏 PII 和任意 secret 不得作为非结构化内容
跨 daemon boundary。

当前通用 framework 会把异常类型和 `str(exception)` 写入 job failure data，尚无独立的
Job error sanitizer contract。这是迁移前必须补齐的安全缺口，不能把任意 worker stderr
或领域异常原文直接复制到目标结构化日志。

## 8. **[TARGET V2]** Rust 首版范围

Rust 首版以当前唯一 production background service 为边界，不实现一个面向未知任务类型的
通用调度平台。

### 8.1 Daemon bootstrap 不是 Job

composition root 在声明 Ready 前按显式顺序完成必要校验和资源初始化；任一步失败都停止
启动，并按逆序释放已获取资源。bootstrap 至少覆盖配置、权限、认证材料、runtime、持久化
连接和 schema compatibility。

不可逆安装/升级和 V1 到 V2 数据迁移必须由显式 installer、init container 或
`asc-state-migrator` 完成，daemon bootstrap 不得隐式执行。Skill Ledger startup reconcile
可以失败并投影 degraded，但不属于 Ready 前置条件。

### 8.2 最小化 JobSupervisor

首版 supervisor 必须：

1. 静态注册具有稳定唯一名称的 background service，并拒绝重复名称；
2. 按确定顺序启动；部分启动失败时只逆序停止已经启动的 service；
3. 拥有全部长期 task、取消信号和 terminal join，不创建 detached task；
4. shutdown 时先停止接收 trigger，再 drain 或 cancel，并等待 owned task 退出；
5. 把 blocking 文件、SQLite、签名、模型或外部进程工作放入明确的 blocking boundary；
6. 捕获 panic/join failure，写入经过脱敏的错误和 health snapshot；
7. 为实际注册的 service 提供 `running/degraded/stopped`、最近 outcome、最近安全错误和
   `last_tick_at`。

首版不要求公开通用 `JobSpec`。注册项只需表达 `name/start/shutdown/health`；timeout、retry、
debounce 和 trigger payload 留在拥有该语义的具体 adapter 中。

### 8.3 Skill Ledger startup/event-driven actor

第一个具体 Rust background service 是 Skill Ledger activation actor。它只需要两种 trigger：

```text
StartupReconcile
SkillFsNotify(SkillFsChange)
```

单一 actor 拥有 pending map，并在同一 select loop 中处理 notify、debounce timer 和 shutdown。
它必须保持：

- startup reconcile 只枚举显式 managed Skill 目录；
- canonical directory 作为 coalesce key；
- `eventKinds/paths` 并集和 reported Skill ID 覆盖规则；
- 500 ms 默认 debounce；
- 单目录串行处理和当前批取消后重入 pending；
- transport/protocol/timeout 最多重试一次，domain failure 不重试；
- startup reconcile 与显式写操作依赖领域事务、幂等键或跨进程锁，而非仅 daemon 内 mutex；
- `accepted/queued/coalesced` 继续表示内存接受，不承诺 durable queue 或 exactly-once。

### 8.4 首版日志与 tracing

每个实际处理批次建立一个 SDK 管理的 `daemon.job.run` OTel span，并记录有界的 AgentSec
语义 attributes，例如 service 名、`startup_reconcile/skillfs_notify` trigger、合并项数和
outcome。startup run 没有上游 parent；多个 notify 合并时不得任意选择其中一个 request
作为唯一 parent。

首版不要求自定义 `job_run_id`、attempt child span 或 contributor Span Links。若没有合法
parent，创建新的 OTel root span；诊断日志从当前 span 取得标准 TraceId/SpanId，不生成第二套
daemon trace ID。OTel 未采样、未配置 exporter 或 Collector 故障不得改变处理结果、当前重试
语义或本地诊断日志写入尝试。

每个开始处理的批次必须产生一个 started 日志和恰好一个 completed/failed/cancelled terminal
日志。自动判断只使用结构化字段，异常、panic payload、worker stderr、traceback、认证材料
和未脱敏敏感值不得直接进入日志或 health。

## 9. **[DEFERRED]** 有实际需求后再设计的能力

以下能力不属于 Rust 首版完成条件：

- 通用 one-shot abstraction；
- periodic/cron scheduler、interval、jitter、missed-tick 和 overlap policy；
- 包含 startup/runtime/retry/shutdown/observability policy 的通用 `JobSpec`；
- 完整 Job lifecycle 与 Job run lifecycle 双状态机；
- 通用 retry/backoff、attempt 模型和 attempt child span；
- contributor cause 集合、OTel Span Links 和截断策略；
- durable queue、operation status、replay 或 exactly-once；
- 为未注册任务预留的 `interval_seconds/next_run_at/pending/inflight` health 字段。

只有出现经过评审的具体 production periodic Job 后，才定义它的 owner、触发目的、interval
配置、首次执行时机、missed-tick、overlap、错误、重试、shutdown、health 和验收 fixture。
不得先以当前 Python `PeriodicBackgroundJob` 原型替未来产品做决定。

## 10. 兼容与演进规则

1. 当前 production registry、Skill Ledger startup reconcile、debounce、合并、retry 分类、
   停止和 health 基础字段属于兼容基线。
2. Python worker 进程、NDJSON worker protocol、Python class 和 asyncio primitives 不是长期
   架构约束。
3. Python 通用 one-shot/periodic 原型及其 optional health/log 字段不是 Rust 首版兼容要求。
4. Rust 为实际 Skill Ledger 处理批次补充 OTel run span 和结构化 lifecycle log 是
   **[TARGET V2]**；不得据此修改当前 Python golden event 数量，也不得要求自定义 run ID、
   attempt span 或 Span Links。
5. `accepted/queued/coalesced` 的 V1 含义保持内存接受语义；durable queue 需要版本化扩展。
6. timeout/cancel 后不得自动重放有副作用工作；需要自动恢复时必须依赖领域幂等键、事务或
   从事实状态 reconcile。
7. Background service 与 request handler、state migrator 或其它 repository consumer 并发
   访问持久状态时，
   不能依赖进程内 mutex 作为唯一一致性边界；必须使用领域事务、幂等键或 reconcile。
8. 新增 background service 必须声明具体目的、trigger、Ready 关系、health projection、日志、
   失败/重试、shutdown 和可执行验收 fixture；不要求先套用通用 `JobSpec`。
9. 新增 periodic 能力必须先有具体 production consumer，并完成第 9 节列出的调度语义评审。

## 11. 验收矩阵

### 11.1 **[CURRENT][PRESERVE V1]** 当前 production 行为基线

| ID | 必须验证的行为 |
| --- | --- |
| DJOB-001 | 默认 registry 恰好包含 `skill-ledger-activation`，status 顺序确定 |
| DJOB-002 | Job 在 socket bind 前启动；启动/bind 失败执行资源和 Job rollback |
| DJOB-003 | shutdown 在 request drain 后停止 Job，并等待 worker/task 清理 |
| DJOB-004 | `skill-ledger-activation` health 保留基础字段和当前 state 投影 |
| DJOB-007 | Skill Ledger startup reconcile 只处理显式 managed 目录 |
| DJOB-008 | 500 ms debounce、按 canonical directory 合并及 reported Skill ID 覆盖 |
| DJOB-009 | drain cancellation 将未处理变更重新放回 pending |
| DJOB-010 | worker transport/protocol/timeout 总尝试数最多 2，domain failure 不重试 |
| DJOB-011 | 当前 worker 停止后不遗留子进程；具体 stdin/terminate/kill 序列仅属于 Python 实现 |
| DJOB-012 | notify accepted 只表示内存接受；显式 managed 目录在重启后通过 startup reconcile 重新求值 |

### 11.2 **[CURRENT][HISTORICAL]** Python 通用框架表征

| ID | 仅用于理解 Python 原型的行为 |
| --- | --- |
| DJOB-005 | one-shot 每 run 一个 legacy trace，started 后恰好一个 completed/failed/cancelled |
| DJOB-006 | periodic 立即首跑、按开始时间对齐、跳过 missed tick、每 tick 新 legacy trace |

DJOB-005/006 不进入 Rust 首版 no-regression 或 release gate；只有未来批准了对应 production
能力后，才以新的目标契约和 fixture 定义 Rust 语义。

### 11.3 **[TARGET V2]** Rust 首版 JobSupervisor

| ID | 必须验证的行为 |
| --- | --- |
| DJOB-013 | readiness-critical bootstrap 在 Ready 前完成并可逆序 rollback；startup reconcile 不伪装为 bootstrap gate |
| DJOB-014 | registry 拒绝重复名称；部分启动失败只逆序停止已启动 background service |
| DJOB-015 | Supervisor 拥有全部 task；shutdown 后无 task、worker、FD 或 blocking work 泄漏 |
| DJOB-016 | Skill Ledger actor 只需 startup reconcile 和 notify trigger，并保持 debounce、合并、串行与取消重入语义 |
| DJOB-017 | service health 区分 running/degraded/stopped 与最近 outcome；每个 started 批次恰好一个安全 terminal log |
| DJOB-018 | Skill Ledger retry 只由稳定错误分类驱动，并保持 transport/protocol/timeout 最多两次、domain 不重试 |
| DJOB-019 | Rust Skill Ledger Job 不依赖永久 Python worker，领域结果和持久化副作用与 oracle 等价 |
| DJOB-020 | Python oracle 与 Rust daemon 对 production CURRENT/PRESERVE V1 health、调度和副作用规范化后等价；Rust 新增日志单独验收 |
| DJOB-021 | 每个实际处理批次具有 SDK 管理的 OTel span；V2 不产生 daemon-owned legacy trace ID，也不要求自定义 job_run_id/attempt span |
| DJOB-022 | 未采样、无 exporter 或 export/Collector 故障不改变处理、当前重试、outcome 或本地日志写入尝试 |
| DJOB-023 | TraceId/SpanId 不参与 authorization、principal、idempotency、deduplication 或 retry decision |

这些 ID 必须映射到机器可执行 manifest 和真实 test/fixture。只存在 Markdown 表格不表示
迁移门禁已经完成。

### 11.4 **[TARGET V2]** Policy reconciliation 专用后台服务

此服务由 `asc-policy-runtime` 实现，daemon 仅装配；它不依赖通用 periodic scheduler 或
完整 JobSupervisor。PAP Binding 提交后通知、retry 到期和有界补扫是触发源。Runtime 与
通知入口可用是 Binding 写准入条件，不是 daemon 启动条件，也不等待每个目标 READY。
Policy/Scope CRUD 不依赖 reconciliation；实际存储错误由各自 Repository 操作返回。
每次尝试才通过 factory 创建 Client，读取凭据；凭据或连接失败按 Binding 重试处理。
每次执行重新读取和准备；退避释放 worker，
同 Binding 互斥到实际调用退出。dirty 仅负责下一轮检查。

自动重试必须等待：Superseded 和仓储错误默认等待 1 秒，RetryAt 按期限等待；
若期限在收尾期间已过去，仍至少等待 1 毫秒。队列默认首次调用后最多自动重试 4 次，
三类结果共用次数；耗尽后保留内存 Exhausted 条目，timer/补扫不能复活。
新 Delete/Update 通知仍立即将等待或耗尽条目改为 Queued，Running 的 dirty 在退出后优先处理，
并重置队列计数；核心已提交的业务预算不被队列重置。Exhausted 仍计入容量，
不伪造业务失败落库或丢弃目标责任；计数不跨 Runtime 重建。
Runtime 直接从 reconciler 获取同一时钟实例，不另行注入 timer 时钟。每页补扫只加锁一次，
不改变已有条目；tick 暂保留全 entries 扫描，不承诺满容量下的扫描延迟。

业务 FAILED 与服务 health 分离。单 Binding 存储/数据错误及 CAS 竞争耗尽保留 WaitingRetry 或 Exhausted，
不维护全局 storage_errors 集合，也不阻断其它请求。存储/数据错误输出 ID 与安全错误诊断，
CAS 耗尽返回独立的 Contended；后续调用重读并恢复已有状态，不伪造失败落库或远端结果。
补扫失败影响 health，但不关闭写准入。Runtime 初始化失败保留 Policy/Scope CRUD、查询及其它服务，
注入不可用通知入口仅拒绝 Binding mutation。
单次 reconcile 的可展开 panic 由核心先尝试结果/失败记账，再由 worker 在调用边界捕获；
最新记录已终结或删除时移除队列条目，否则保留 Exhausted 停止自动执行，避免补扫重放。
查询结果也失败或 panic 时同样按未确认处理；新通知/dirty 仍优先，worker 继续处理其它 Binding。
不覆盖已提交成功结果，不因 panic 清空 deployments，也不把旧尝试失败写到新 Delete/Update。
timer/scanner 或 worker 调度代码自身 panic 才停止领取并标记 reconciliation 服务失败，关闭 Binding 写准入；
daemon 仅记录健康变化，不因此请求全进程 shutdown，Policy/Scope CRUD 和读查询仍可用。
这遵循“单个 Job error 不自动等于顶层 daemon 不可用”的原则。首版不自动重建
失败 Runtime，需进程重启。取消不会中断同步
Client；shutdown 先关闭请求准入并 drain 请求，再停止领取/扫描并 join 实际调用。30s drain
到期后只由进程退出结束剩余工作，不提前释放执行所有权。首版内存记录在重启后丢失；
持久化后重启恢复须独立验收。服务不创建自定义 trace ID，OTel 集成沿用迁移架构的统一契约。

以下均映射到 `v2/crates/policy/asc-policy-runtime/src/reconciliation/tests.rs` 的可执行测试：

| ID | 行为 | fixture / test |
|---|---|---|
| DJOB-024 | 同 ID 合并、并发领取和 dirty/finish 竞争 | `fifo_coalesces_and_dirty_survives_notification_finish_races`、`concurrent_takers_never_claim_one_id_twice` |
| DJOB-025 | 退避释放 worker，Delete 提前唤醒，重新准备 | `one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`、`delete_preempts_waiting_retry_without_waiting_for_clock` |
| DJOB-026 | 容量包括执行/等待，通知遗漏经稳定 ID 分页补回 | `capacity_includes_running_and_waiting_and_stop_wakes_takers`、`compensation_pages_past_capacity_without_any_notifications` |
| DJOB-027 | 提交后通知、准入失败无通知、timer 服务失败关闭 Binding 新写 | `pap_notification_observes_committed_state_and_failed_admission_never_notifies`、`timer_panic_closes_admission_and_shutdown_observes_failure` |
| DJOB-028 | 停机等待实际调用、旧 Apply 观察不覆盖 Delete | `shutdown_retains_actual_call_until_join_and_closes_write_admission`、`delete_admitted_during_apply_waits_for_exit_and_preserves_cleanup` |
| DJOB-029 | 单 Binding 错误重试不阻断其它 Binding/PAP CRUD；扫描降级不关闭准入 | `binding_errors_retry_without_blocking_other_bindings_or_pap_writes`、`scan_failure_degrades_health_without_closing_binding_admission` |
| DJOB-030 | 自动重试等待且有上限，补扫不复活；新通知立即触发并重置队列预算 | `automatic_retries_wait_and_stop_at_budget_without_blocking_other_bindings`、`new_notifications_reset_queue_budget_and_preempt_waiting_or_exhaustion`、`zero_retry_budget_stops_first_failure_and_submillisecond_delay_is_rejected` |
| DJOB-031 | 单次 reconcile panic 隔离，保留完成事实/目标责任，worker 继续处理其它 Binding | `panic_tests.rs`：`attempt_panic_records_failure_and_same_worker_completes_next_binding`、`committed_success_survives_attempt_panic_without_replay`、`unconfirmed_panic_stops_only_that_binding_until_new_notification` |
| DJOB-032 | panic 收尾与新通知竞争不丢工作、不重入，新 Delete 可继续清理 | `panic_tests.rs`：`delete_during_attempt_panic_keeps_dirty_and_cleans_registered_target`、`panic_completion_and_new_notification_race_never_loses_work` |
| DJOB-033 | Runtime 沿用核心时钟；批量补扫保留既有任务、容量及到期语义 | `one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`（非零起点）、`batch_discovery_preserves_existing_work_and_applies_capacity_and_deadlines` |

运行入口：`cargo test -p asc-policy-runtime --locked --offline`。组件中的 scripted 错误不等于
系统性 fault-injection 交付。完整 CLI/daemon E2E 单独 PR，SQLite/崩溃恢复和系统性注入等待
persistent Repository；这些边界见 [Runtime 设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)。

## 12. 当前实现证据

- 通用 Job interface、status、one-shot、periodic 和 manager：
  `agent-sec-cli/src/agent_sec_cli/daemon/jobs/base.py`；
- 默认 Job 注册：`agent-sec-cli/src/agent_sec_cli/daemon/jobs/registry.py`；
- daemon 启动、bind rollback、drain 和停止：
  `agent-sec-cli/src/agent_sec_cli/daemon/server.py`；
- health 投影：`agent-sec-cli/src/agent_sec_cli/daemon/health.py`；
- Skill Ledger debounce、合并、requeue 和 startup reconcile：
  `agent-sec-cli/src/agent_sec_cli/daemon/jobs/skill_ledger/activation.py`；
- worker retry、timeout 和停止：
  `agent-sec-cli/src/agent_sec_cli/daemon/jobs/skill_ledger/worker_client.py`；
- 通用调度、trace 和日志测试：`tests/unit-test/daemon/test_jobs.py`；
- activation 行为测试：`tests/unit-test/daemon/test_skill_ledger_activation.py`；
- worker 生命周期测试：`tests/unit-test/daemon/test_skill_ledger_worker_client.py`；
- daemon/Skill Ledger 端到端行为：
  `tests/integration-test/skill-ledger/test_skill_ledger_daemon_integration.py`。

## 13. V2 tracing 标准依据

- [W3C Trace Context](https://www.w3.org/TR/trace-context/)；
- [OpenTelemetry Trace API](https://opentelemetry.io/docs/specs/otel/trace/api/)：
  background run span、root/parent 关系和 context propagation 的标准语义。attempt child span
  与 contributor Span Links 不属于 Rust 首版要求。

## 14. [TARGET V2] SkillGuard 启动恢复边界

第五批在 UDS 准入前执行一次 SkillGuard 恢复：首先恢复私有换钥 intent，再按已登记的精确根目录
调用 reconcile。任务共享 SkillGuardService 和公共 Action Runtime，使用 120 秒总预算，不启动
周期调度器。单 Skill 恢复失败记录公开错误类别和进程诊断，继续处理其他 Skill；后续写操作在同一
锁内再次检查其恢复 intent。换钥未完成时普通 Ledger 操作拒绝，但 status 与管理员重试可用。
启动恢复与最近一次业务结果分开；进程启动不代表每个 Skill 均已激活。

DJOB-SG-001 的可执行核心证据是 `asc-capability-skill-guard/src/service/administration.rs` 和
`src/service/rollback.rs` 的中断恢复测试；进程装配位于 `v2/apps/asc-daemon/src/skill_guard.rs`。
SkillFS notify 驱动的后台合并队列、重试、shutdown、健康与实际 FUSE 生效在第六批接入，不由本节
提前声称完成。

## SkillGuard 第一阶段落地边界

上述 Python Job 和早期 Rust 建议由第一阶段的具体实现收口：当前使用一个进程内 worker，
共享 `SkillGuardService`、Action Runtime 和公共 Finalizer。没有另建 Python 子进程协议或
通用 Job 调度器。每项变更分别执行 scan 和 activation，前者失败也会尝试后者；业务错误不
自动重放，仅执行前的 `Busy` 在同一有界期限内重试。

实时通知按 canonical 身份合并，500 ms debounce，持续输入时最多延迟两秒；相同 Skill
的具体文件列表不影响扫描范围，因此 worker 始终扫描完整 Skill。实时 pending 限制为 256
个不同 Skill，满队列返回签名拒绝。这是取代上文无界 map 的明确资源边界。启动任务来自
已有私有 registry，完整安排所有显式注册的挂载 Skill，不受该实时 pending 上限截断。

关闭停止接收并等待当前有界操作。内存 pending 不持久化，daemon 重启后通过全量注册目录
补扫恢复，而非重新发送原通知或承诺 exactly-once。`status` 的 `skillfs` 对象返回 queued、
running、processed、failed、lastError；各次 Action 使用公共审计记录，未增加独立 Job
trace-ID 或第二套审计输出。worker 意外退出时关闭队列准入，`healthy=false`、`running=false`，
要求重启 daemon；不会继续确认无法处理的通知。可执行异常退出证据为
`asc-daemon-handler/src/skillfs/worker.rs::unwinding_worker_closes_admission_and_reports_failed_health`。
具体模块与验收边界见
[SkillGuard 第一阶段迁移](SKILL_GUARD_PHASE_ONE_zh.md#第六批-skillfs-边界)。
