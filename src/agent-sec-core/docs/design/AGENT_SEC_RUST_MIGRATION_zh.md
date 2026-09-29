# AgentSecCore V1 到 V2 Rust 迁移桥接与验收计划

## 1. 文档状态与仓库内权威关系

本文负责把 AgentSecCore V1 的当前实现、语言无关契约和可执行回归语料映射到 V2 Rust
工作包。本文是仓库内 V2 产品架构、进程形态、crate 边界、迁移组织和兼容策略的权威
入口。实现、评审和验收只能依赖仓库内可访问的规范；新的架构决策必须直接更新本文及受
影响的契约，不能依赖未随仓库发布的材料。

**[TARGET V2] CLI 命名：** Rust CLI 的 Cargo package/crate 与源码目录继续使用
`asc-cli` / `asc_cli` / `v2/apps/asc-cli`，对外编译产物和命令名统一为 `agent-sec-cli`。
本文其余 `asc-cli` 表示内部组件，不表示另一份可执行文件；当前 PAP slice 与 V1 同名
不代表已完成 V1 全量命令迁移。

此前文档中的“长期保留 Python CLI、PyO3 NativeExecutor、daemon 不可用时自动本地
fallback、backend-first/daemon-last、per-user daemon”路线已经被当前 V2 架构取代，不再
是候选实现或兼容路线。旧路线只保留在 Git 历史中用于审计。

V1 当前行为由以下六份语言无关文档固化：

- [DAEMON_CURRENT_BEHAVIOR_zh.md](DAEMON_CURRENT_BEHAVIOR_zh.md)：daemon 当前控制面；
- [DAEMON_PROTOCOL_V1_zh.md](DAEMON_PROTOCOL_V1_zh.md)：UDS/NDJSON、方法、错误和认证；
- [DAEMON_JOB_CONTRACT_zh.md](DAEMON_JOB_CONTRACT_zh.md)：Job 生命周期、调度、日志和恢复；
- [DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md)：
  V1 进程、安装、systemd、runtime/data path 和日志；
- [SECURITY_MIDDLEWARE_CONTRACT_zh.md](SECURITY_MIDDLEWARE_CONTRACT_zh.md)：action、context、
  result、lifecycle、event 和脱敏；
- [SECURITY_ACTIONS_REFERENCE_zh.md](SECURITY_ACTIONS_REFERENCE_zh.md)：八个 action 的输入、
  输出、错误、verdict 和副作用。

六份契约是 V1 discovery、regression oracle 和 compatibility fixture 的来源，不自行决定
V2 产品形态。Rust 执行内核的详细设计见
[RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md)，
其中的类型和 module 仅是 asc-action-runtime 的实现提案。

本文使用以下标签：

- **[CURRENT]**：V1 Python 实现当前可观察行为；
- **[PRESERVE V1]**：supported V1 兼容接口在兼容期必须保持的语义；
- **[TARGET V2]**：本文及仓库内相关权威契约定义的 V2 目标；
- **[SUPERSEDED]**：已经被当前仓库 V2 架构取代的旧迁移目标；
- **[HISTORICAL]**：只作为设计或测试语料依据。

不能把 V1 的 Python 类、PyO3、asyncio、worker 进程、per-user 目录或 systemd user unit
仅因当前存在就标记为 **[PRESERVE V1]**。外部兼容可以由 Rust binary、协议 adapter、状态
migrator 或版本化接口承接，不要求保留 Python 运行时。

**当前门禁状态：** 六份规范文本已经存在，但 machine-readable acceptance manifest、
验收 ID 到 V1 oracle fixture/runner 的映射、共享 V1/V2 differential harness 和性能基线仍是
待交付物。Markdown 表格本身不构成 Definition Ready、Crate Complete 或 Release Ready。

## 2. **[CURRENT]** V1 实现基线

以下事实用于 discovery，不是 V2 架构要求：

- agent-sec-cli/src/agent_sec_cli/cli.py 是 Python CLI 入口；
- security_middleware.invoke() 通过静态 router 调用八个 action；
- V1 daemon 是 per-user Unix socket 服务，当前默认 registry 有 9 个 method；
- V1 daemon 当前提供 health、Skill Ledger notify、security event/observability query 和后台
  Skill Ledger activation，不执行八个 security action；
- V1 query handler 直接同步读取 SQLite，存在阻塞 event loop 的实现缺口；
- V1 Skill Ledger job 使用持久 Python worker，这是实现细节而不是兼容目标；
- security event 当前写 JSONL 与 SQLite，部分 action 具有专用 sanitizer；
- 当前 trace_id 是 opaque correlation string，不是 OTel TraceId；
- src/lib.rs 中已有 PyO3 Prompt Scanner，只证明 V1 仓库存在该技术，不构成 V2 采用
  PyO3 的依据；
- 当前 per-user runtime、socket、systemd user unit、HOME/XDG data fallback 都只属于 V1
  部署事实。

commit ef0d75f27c389434cf6f4361f5dbcdeaff42ab72 曾实现 scan-prompt handler。其显式
handler registration、blocking boundary、context propagation 和三层 response 可以作为协议设计
证据；Python scanner/preload 和当时的进程形态不构成 V2 目标。

每个 V2 crate 任务在 Definition 中记录自己读取的 V1 源码 commit、直接依赖 contract
revision、搜索路径、已有测试和未自动验证项；不使用构建产物目录作为源码基线。

## 3. **[TARGET V2]** 产品与架构目标

### 3.1 产品形态

1. V2 产品代码全部使用 Rust，包括 asc-cli、asc-daemon、security capabilities、Policy
   Engine、JobSupervisor、状态迁移工具和交付入口。
2. V1 Python daemon、CLI、middleware 和 backend 只作为 discovery 和差分测试 oracle，
   不进入 V2 运行时。
3. V2 默认只有一个 Rust asc-daemon 进程；Policy Engine 以 Rust library 嵌入，不部署
   Python daemon 或默认 asc-policyd sidecar。
4. asc-cli 是 daemon client，不启动 daemon、不拥有 daemon 生命周期，也不在 daemon
   不可用时通过 PyO3 或另一套本地业务实现自动 fallback。
5. 少数纯函数能力是否允许 Rust CLI 直接调用必须形成独立产品合同；在批准前，不构成通用
   daemon fallback。

### 3.2 系统服务、身份与查询

1. Host 上默认 one daemon per host，由 system-scope systemd 管理；Kubernetes 上每个 Node
   一个 DaemonSet 实例。system-level 不等于必须以 UID 0 运行。
2. singleton lock、socket、runtime 和 state 位于 packaging 固定的 system-owned path，
   不位于用户 HOME 或 XDG_RUNTIME_DIR。
3. 同一 daemon 服务多个本地 UID/Agent。peer UID/GID/PID 来自内核，token 用于认证绑定；
   daemon 构造可信 Principal。客户端自报的 UID、role、scope 只能作为不可信输入。
4. QueryScope 由服务端生成并与请求 filter 求交。普通 principal 只能访问 owner scope，
   auditor/admin 才能跨 UID。
5. CLI/TUI 不得直读 SQLite，不得绕过 daemon 直接调用 Compiler、PCP 或 Repository。
6. 恢复完成后 daemon 才进入 READY；shutdown 先停止 admission，再 drain 或 checkpoint。

### 3.3 运行时与领域边界

- asc-daemon 是进程入口和 composition root，只负责 bootstrap、配置、runtime、RPC、
  signals、具体 adapter 装配和进程级 observability。
- asc-daemon-service 负责协议无关的 UDS admission、framing、peer credentials、timeout、
  drain 和 socket 生命周期，并通过 `RequestDispatcher` port 调用上层。
- asc-daemon-handler 负责 daemon wire 解码、method allowlist、server-owned authorization、
  application use case 路由以及 response/error projection；不拥有进程或 transport runtime。
- asc-daemon-core 负责编排 identity、authorization、action、policy、observability、
  management、jobs 和 lifecycle 用例，不复制领域业务语义。
- asc-action-runtime 负责 action validation、admission、execution supervision、timeout、
  cancellation、finalizer 和 observation projection。
- 具体 security capability 实现 CapabilityExecutor；不得使用 SecurityBackend 统称
  capability、context provider、enforcement adapter 和 persistence adapter。
- Policy Compiler/PAP、Policy Runtime、PCP 与 security capability 通过稳定的
  Evidence/AttributeBundle、PreparedPolicy、Binding、Receipt 等合同协作；capability 不依赖
  Policy Compiler，PCP 不依赖 compiler。
- Repository、Client、Sink 是 adapter port。具体 SQLite、HTTP、AgentSight 或模型实现由
  asc-daemon 装配，不反向渗入 domain/runtime crate。
- JobSupervisor 首版只负责已注册长期 task 的 ownership、取消、shutdown、轻量 health 和
  实际执行批次的可观测性；trigger、debounce、当前 retry 和恢复语义由具体 service adapter
  承担。通用 periodic/deadline/retry scheduler 延后到出现具体 production consumer 后设计。
  领域 application service 拥有 reconcile、activation、receipt 或 retention 的业务逻辑。
  详细边界见
  [`DAEMON_JOB_CONTRACT_zh.md`](DAEMON_JOB_CONTRACT_zh.md#8-target-v2-rust-首版范围)。

### 3.4 Action Runtime 和可观测性

逻辑 action 路径为：

~~~text
daemon protocol adapter
  -> asc-daemon-core action use case
  -> ActionRequest + trusted Principal
  -> validation
  -> asc-action-runtime
  -> CapabilityExecutor
  -> ActionResult
  -> unique Finalizer
       -> SecurityEvent/audit
       -> diagnostic logging
       -> OTel tracing/metrics
       -> effect/receipt projection
~~~

必须保持：

- action 静态 allowlist 和 typed request/result；
- transport、action schema、domain precondition 三层 validation 责任；
- verdict、执行成功和 daemon transport success 分层；
- supervisor 拥有 accepted invocation，caller detach 不使执行和 audit 无主；
- 每个已路由 invocation 最多一个 terminal SecurityEvent；
- 每个 action 显式 audit projector，新增字段默认不写入审计；
- SecurityEvent、diagnostic log、OTel 和 telemetry/read model 分别投影；
- blocking work 不阻塞 socket runtime；timeout 不等于副作用已经取消；
- OTel 是 V2 TraceId、SpanId、parent 和 traceparent/tracestate 的唯一权威；
- SecurityEvent 是独立本地安全事实，不受 sampling、exporter 或 Collector 可用性影响。

## 4. V1 到 V2 兼容分类

### 4.1 必须进入 compatibility inventory 的外部面

- supported CLI command、flag、默认值、退出码和机器可读输出；
- daemon RPC method、framing、error code、timeout 和三层 response；
- SecurityEvent、observability 和 public JSON/schema；
- 配置、环境变量、安装路径和状态格式；
- security verdict、fail-open/fail-closed、重试、幂等和副作用；
- redaction、安全日志和敏感信息边界；
- 升级、状态 owner、回滚和混合版本读取。

兼容不要求保留 Python 内部调用路径。若 agent-sec-cli 是 supported command，V2 可以由
Rust agent-sec-cli binary、兼容命令名或受控 wrapper 提供同一外部接口，但不能以 Python/PyO3
作为 V2 依赖。任何删除、重命名或语义变化都必须先有 versioned replacement、兼容期和批准
的 change record。

### 4.2 daemon V1 协议

当前 9 个 method 是 V1 事实。V2 asc-daemon-protocol 必须逐项决定：

- 直接兼容；
- 通过 versioned adapter 兼容；
- 经批准废弃并提供迁移路径。

现有文档提出的 8 个 action.* method 与 17 个 canonical method 总数不是 V1 当前事实。
它们与“所有入口进入 daemon-core、显式 allowlisted action RPC”目标一致，但仍必须在 daemon
protocol crate 的 Definition Review 中确认 method 名、schema、授权、timeout 和兼容版本，
不能只因旧文档写过就宣称冻结完成。

V1 action response 的三层语义继续作为 compatibility candidate：

1. transport/protocol failure；
2. daemon failure：ok=false；
3. action result：ok=true，其中 action 自身仍可失败或返回非零 exit code。

V2 没有 daemon-unavailable 本地 fallback。旧 daemon 对新 method 返回 unknown_method 时，
Rust client 应报告受控的版本/能力不兼容；timeout、EOF、busy、认证或业务错误均不得触发
另一条本地执行路径。

### 4.3 状态与数据迁移

V1 AGENT_SEC_DATA_DIR、系统/user/临时 fallback、JSONL/SQLite、Skill Ledger key、manifest、
snapshot 和 activation 是 discovery 输入。V2 目标是 system-owned persistence，由 daemon
通过 owner principal 和 QueryScope 隔离；asc-cli 不直读数据库。

asc-state-migrator 必须定义并验证：

- V1 per-user 路径发现规则和显式输入；
- 数据 owner 到 V2 Principal/owner scope 的映射；
- schema version、重复导入、事务、失败恢复和回滚；
- V1/V2 mixed-read 窗口；
- 文件权限、symlink、hardlink 和不可信路径处理；
- 多个用户源合并时的冲突、去重和审计证据；
- 不把 credential、token 或 key material 导入通用 persistence crate。

## 5. 迁移组织方式

### 5.1 滚动 contract-first

迁移不采用 backend-first/daemon-last，也不等待一次性全局决策冻结。每个 crate 工作包按直接
依赖独立启动：

1. 记录源码 baseline、直接依赖 contract revision 和 V1 搜索证据；
2. 选择 V1 relationship：migration、partial migration、greenfield 或 adapter；
3. 选择 acceptance type：MIGRATION_EQUIVALENCE、PARTIAL_EQUIVALENCE、
   GREENFIELD_CONTRACT、ADAPTER_CONFORMANCE 或 DISTRIBUTION_LIVE；
4. 完成 Definition Review；
5. 独立 build/test，并由直接消费者验证；
6. 进入相关 integration slice；
7. 提交 Result/Evidence 和回滚方式。

某个 crate 的未决问题只阻塞该 crate 和直接消费者，不默认阻塞其它无依赖工作包。

### 5.2 Crate 工作包边界

现有库 crate 统一直接放在 `v2/crates/asc-*/`，产品入口保留在
`v2/apps/`。action、daemon、data、foundation、integrations、policy 是逻辑分类，
不再作为目录层级；目录扁平化不改变 package 名称、依赖、feature 或工作包边界。

工作包采用本文定义的目标 workspace，至少包括：

- 产品入口：asc-daemon、agent-sec-cli、asc-state-migrator；
- daemon：asc-daemon-protocol、asc-daemon-service、asc-daemon-handler、asc-daemon-core；
- action：asc-action-types、asc-evidence-types、asc-action-runtime 和各 asc-capability-*；
- policy：asc-policy-types、asc-policy-target-contracts、asc-policy-repository、asc-policy-engine、asc-policy-runtime、asc-pap、asc-pcp；
  其中 asc-policy-target-contracts 只定义共享 Adapter/Client trait，依赖纯数据契约
  asc-policy-types；Reconciler 与具体 PEP 实现均依赖该共享层，而不互相依赖实现。
- data：asc-security-events、asc-observability、asc-session、asc-state、
  asc-persistence-sqlite；其中事件持久化实际落地时又拆出 asc-sqlite-kernel（与领域
  无关的 SQLite 内核）、asc-event-log（JSONL 落盘）、asc-security-summary（摘要渲染）
  和 asc-event-sink（双写装配与进程级单例），拆分理由与 schema/迁移契约见
  [《V2 数据持久化层迁移设计》](V2_DATA_PERSISTENCE_MIGRATION_zh.md)；
- integrations：AgentSight/ActPlane、模型和 credential adapter；
- tests：asc-testkit、asc-contract-tests、asc-integration-tests。

asc-foundation-types 只承载真正跨多个 bounded context 的稳定值类型和纯转换，不演变为
common/utils。具体 capability 不依赖 daemon-core；Action Runtime 不依赖具体 capability；
composition root 负责注入。

Policy reconciliation 的具体目录与边界见
[调度、存储与恢复设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)：拟建
`v2/crates/asc-policy-runtime/src/reconciliation/` 承载 WorkQueue、worker 和恢复调度，
`asc-pcp` 每次重新读取并从头执行，不保留跨调用计算缓存；共享 Repository 定义局部条件写，未来
`v2/crates/asc-persistence-sqlite/` 实现持久化。daemon 只装配并管理进程生命周期。
这是目标实施位置，不代表相应 crate、后台接线或恢复已交付。

### 5.3 Integration slices

- **Action Slice**：daemon-core + action-runtime + 一个 capability + security-events；
- **Policy Slice**：PAP + policy-engine + PCP + AgentSight adapter + persistence；
- **Query Slice**：security-events + session + observability + persistence + authorization；
- **Product Slice**：asc-daemon + agent-sec-cli + state-migrator + packaging。

slice 是集成验收单元，不是让所有 crate 串行等待的开发阶段。

## 6. 每个工作包的 Definition 与验收

### 6.1 Definition

每个任务必须记录：

- Goal、范围、非目标、owner、crate 路径和交付物；
- V1 源码 baseline、调用链、调用方、配置、状态和副作用；
- V1 relationship 与 acceptance type；
- 公共 API、port、错误模型和 contract revision；
- 直接依赖、直接消费者和允许使用的 mock/fixture；
- 保留、废弃、新增和有意修复的行为；
- 可重复执行的验收命令、人工证据和回滚方案。

Definition Review 必须确认 oracle/fixture 或 greenfield contract 可执行；未知 V1 行为记录为
Open Question，只阻塞受影响的工作包。

### 6.2 No-regression

对 migration/partial-migration 中的 V1 部分，相同 fixture 必须比较：

- 机器可读输出和错误分类；
- daemon framing、response layer 和 timeout；
- security verdict 和 fail-open/fail-closed；
- 状态转换、持久化和外部副作用；
- audit/event、日志和脱敏；
- 幂等、重试、取消、恢复和回滚。

Python 到 Rust 的内部重构、私有类型变化和不影响外部行为的性能优化不是 regression。安全或
正确性修复必须在实现前形成 change record；若影响外部接口，仍提供版本化兼容路径。

### 6.3 分层门禁

1. **Definition Ready**：Definition、直接依赖、contract revision 和验收命令完成。
2. **Crate Complete**：独立 build/fmt/clippy/test，通过本 crate fixture 和 contract tests。
3. **Integration Ready**：直接消费者通过，composition root 完成注入，并验证身份、状态、
   timeout 和失败语义。
4. **Distribution Ready**：干净 checkout 可重复构建；binary 进入 raw/RPM/container/systemd/
   Helm；生成 checksum、SBOM 和 build metadata。
5. **Release Ready**：升级、回滚、状态兼容、canary、真实 AgentSight/ActPlane 和完整 V1/V2
   pass/fail 矩阵留存证据。

Mock E2E、server-side admission 和真实内核执行是不同证据层级，不能互相替代。

## 7. 系统级验收标准

### 7.1 功能与接口

- supported CLI/RPC/config/schema/state 接口兼容或具有批准的版本化迁移；
- V1 protocol fixture 在 compatibility adapter 上通过；
- 每个 migrated capability 的结果、verdict、error、event 和副作用与 oracle 等价；
- 失败 ActionResult 不被错误投影为 daemon transport failure；
- handler、Job 和 CLI 都调用相同 application service，不复制 capability/policy 语义；
- Rust runtime 不导入 V1 Python daemon、middleware 或 backend。

### 7.2 身份、授权与查询

- 第二个 Host daemon 实例被拒绝；
- 两个不同 UID/Agent 通过同一 system socket 访问且 owner scope 隔离；
- caller 自报 UID/role/scope 不能提升权限；
- normal principal 不能跨 owner 查询，auditor/admin 受显式授权；
- CLI/TUI 无直接 SQLite、Compiler 或 PCP 绕过路径；
- 外部 job 保留 owner principal，内置任务使用 System principal。

### 7.3 安全事件与 tracing

- V1 legacy event 与 V2 event 通过 schema version 区分并可在迁移窗口混合查询；
- V2 TraceId/SpanId 只由 OTel SDK/context 生成和传播；
- session/run/call/tool-call/action/backend/policy/verdict 仅作为有界语义 attributes；
- SecurityEvent 独立于 sampling/exporter/Collector；
- audit projector 默认不记录新增 request 字段；
- trace identity 不参与 authorization、principal、idempotency、deduplication 或 replay。

### 7.4 进程、状态与分发

- Linux 安装 system-scope unit，不安装 user-scope unit；
- Kubernetes 每个目标 Node 恰有一个 Ready DaemonSet 实例；
- daemon 不自行 daemonize，也不在启动时隐式执行不可逆 migration；
- state migrator 的重复运行、失败恢复、权限、owner 映射和回滚通过测试；
- raw/RPM/container/systemd/Helm 均包含对应 Rust binary 和版本元数据；
- 不要求 Python interpreter、site-packages、PyO3 extension 或 wheel 才能运行 V2。

### 7.5 可靠性与性能

迁移前必须在固定硬件、OS、build profile、数据规模、并发、预热和采样方法下记录 daemon
冷启动、p50/p95/p99、吞吐、空闲/峰值 RSS、FD 和 task/subprocess 数。Rust 替代实现未经
带环境元数据的实测报告，不得宣称优于或不劣于 V1。

压力和 soak 测试必须证明没有持续内存、FD、task 或子进程泄漏；SIGTERM 停止 admission、
处理或 checkpoint 在途工作，并完成 socket、lock 和 audit flush 的受控收尾。

## 8. 必需交付物

- 每 crate Discovery/Definition/Result/Evidence 任务卡；
- V1 compatibility inventory 和批准的 change records；
- machine-readable acceptance manifest 与验收 ID 映射；
- 共享 V1 oracle fixtures、Rust runner 和 differential harness；
- daemon protocol、action、job、event、state migration 和多 UID authorization fixtures；
- 四个 integration slice 的可重复执行证据；
- 性能、压力、soak、故障注入和安全测试报告；
- raw/RPM/container/systemd/Helm 安装、升级、回滚和卸载报告；
- checksum、SBOM、build metadata 和 release matrix；
- V1 Python runtime 删除清单和确认 V2 产物不链接/装载 Python 的验证结果。

## 9. 主要风险及控制措施

| 风险 | 控制措施 |
| --- | --- |
| 将 V1 实现细节误标为 V2 兼容要求 | CURRENT/PRESERVE V1/TARGET V2 分层；以本文的仓库内 V2 架构为准 |
| 为“全 Rust”静默破坏外部接口 | compatibility inventory、versioned replacement、change record |
| crate 并行导致 contract 漂移 | 每任务固定 direct dependency revision，消费者 contract test |
| system daemon 引入跨 UID 数据泄露 | trusted Principal、server QueryScope、多 UID E2E |
| CLI 或 TUI 绕过 daemon 授权 | 禁止直读 SQLite/Compiler/PCP，架构依赖检查和黑盒测试 |
| timeout/断开后副作用状态不明 | supervisor ownership、operation status/receipt、禁止客户端重放 |
| audit 自动复制敏感字段 | 每 action 显式 projector，默认 deny，隐私 fixture |
| OTel 故障影响安全功能 | tracing/export 与 ActionResult、SecurityEvent sink 隔离 |
| per-user 数据迁移丢失 owner | state migrator owner mapping、事务、重复运行和回滚测试 |
| 只有 Markdown、没有可执行门禁 | manifest、fixture、runner、pass/fail matrix 是完成条件 |

### OTel tracing 基础切片落地

`asc-observability` 已提供统一 OTel Context、Agent Baggage、兼容输入及只读关联快照；
CLI/client/daemon/PAP/compiler 已接线，产品 main 启用 runtime feature 和可选 OTLP。
该基础设施不依赖尚未落地的 Action Runtime、安全事件 Rust sink 或本地链路重组。
协议/部署/中间件契约同步记录 TARGET V2 条款，V1 实现继续仅作为冻结输入 oracle。
实施、验收边界及 server-first/caller-first 回滚顺序见 [V2 OTel 验收](V2_OTEL_ACCEPTANCE_zh.md)。

## PII Checker 第一阶段实现入口

PII 检测核心、中央规则、Runtime/Finalizer、RPC/CLI 和 Hook/RPM 验收位于
[PII 两阶段迁移设计](PII_V2_MIGRATION_zh.md)。该文显式区分当前执行服务与未来
PIP 证据获取、PDP 决策、PEP 执行反馈的覆盖范围；不改变本文的系统级部署及 OTel 目标。
