# PAP daemon API V2 工作包验收记录

| 属性 | 值 |
| --- | --- |
| 状态 | PAP integration ready；不是 distribution/release ready |
| 验收日期 | 2026-09-07 |
| 源码基线 | `main@9f109d55964cfc5820d41564869f70879a8de21f` |
| contract revision | 本文、`DAEMON_PROTOCOL_V1_zh.md` 和 `DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md` 同一变更 |

## 1. Goal、范围和非目标

本工作包为 V2 daemon 增加 15 个显式 allowlisted PAP 方法，覆盖 Policy、Scope 和 Binding
的 current-record CRUD；由 kernel peer credentials 构造 trusted Principal，经
`asc-daemon-handler` 调用 `asc-daemon-core::PolicyAdministration`，再委托给 `PapService`。
工作包同时提供首个 `prevent_file_deletion` compiler 和仅用于集成的 process-local
Repository。

以下不属于本工作包的完成声明：durable persistence、Reconciler、target Adapter、真实
AgentSight/ActPlane enforcement、production socket/package hardening、完整 readiness，以及
V1 九个 daemon method 的替代或退役。

## 2. Crate relationship 与 acceptance type

| crate / entrypoint | V1 relationship | acceptance type | 当前证据层级 |
| --- | --- | --- | --- |
| `asc-policy-types` authored contract correction | greenfield V2 contract | `GREENFIELD_CONTRACT` | crate contract |
| `asc-policy-engine` | greenfield | `GREENFIELD_CONTRACT` | direct trait + daemon consumer |
| `asc-pap-repository-memory` | adapter；无 V1 durable-state 承诺 | `ADAPTER_CONFORMANCE` | Repository + PAP consumer |
| `asc-daemon-protocol` PAP method family | greenfield method set over an existing V1 envelope | `GREENFIELD_CONTRACT` | typed wire + real UDS |
| `asc-daemon-core` PAP boundary | greenfield | `GREENFIELD_CONTRACT` | handler consumer |
| `asc-daemon-handler` | protocol/application adapter | `ADAPTER_CONFORMANCE` | real UDS |
| `asc-daemon` composition | partial migration | `PARTIAL_EQUIVALENCE` | foreground binary + UDS + signal cleanup |

直接依赖 contract 使用本基线中的 `asc-foundation-types`、`asc-policy-types`、`asc-pap` 和
`asc-daemon-service`。V1 Python daemon 只作为 discovery/oracle 来源，不进入 Rust runtime。

## 3. External compatibility report

- 15 个 `policy.*` 方法是 `[TARGET V2]` 新增面，不冒充当前 V1 九个 method，也不修改现有
  CLI、Python daemon 或 V1 action response。
- PAP wire 直接复用 domain `PolicyTemplate`、`ScopeSelector`、`PreparedPolicy`、
  `PreparedScope` 和 `BindingView`；method params 拒绝未知字段。
- `prevent_file_deletion` 的 contract 明确收敛为
  `ResourceOperation::Delete + FileResolution::PathEntry`。它不承诺阻止 rename/move、link、
  truncate、内容修改或其它 namespace mutation。
- daemon 为每个 dispatch 生成新的 UUID request ID；成功和失败 response 均只暴露有界的
  public error contract，不暴露内部 persistence/compiler error。
- shared V1 request envelope 的 `trace_context`、`caller`、`timeout_ms` 和未知顶层字段兼容性
  尚未在此工作包中实现，因此本记录不声明 V1 envelope compatibility 已完成。
- PAP-CR-007 删除未使用的旧 Scope 读取格式：显式 `legacy_execution_domain`、缺失或
  null selector 均不再解码为 PreparedScope（包括 Binding 内嵌 Scope）。PID/cgroup
  格式不变；旧 kind 在 Create/Update 请求上均返回 `invalid_request`，错误文案改为
  unknown variant。其它既有 legacy 字段读取兼容不随本次变更删除。

## 4. Internal contract change record

| ID | 变更 | 原因与影响 |
| --- | --- | --- |
| PAP-CR-001 | 将 `PreventFileDeletion` 从含糊的 rename-out 表述收窄为 path-entry delete | 当前 compiler、protocol fixture 和 IR 只生成 `Delete`；在进入 distribution 前消除过度承诺，不改变 V1 runtime |
| PAP-CR-002 | `DaemonError` 在构造和 decode 时统一限制为 256 UTF-8 bytes | 防止任一 handler 绕过公共 response bound；超长 caller-authored decode error 使用稳定通用消息，不回显输入 |
| PAP-CR-003 | `pid`、`cgroupId` selector validation path 投影为 `InvalidArgument` | 这些路径来自 authored selector，不是 canonical/internal state |
| PAP-CR-004 | serialized CRUD scenario 要求所有 dispatch request ID 互不相同 | 冻结每个请求生成 fresh UUID 的 correlation contract |
| PAP-CR-005 | Binding revision 仅随 spec 变化；Delete 不可撤销；清理完成后移除记录 | 2026-09-08 V2 contract correction；Delete/失败重试同版，Applying 可受理 Delete，旧 ID 不允许 Update 重建；方法和 DTO 不变 |
| PAP-CR-006 | Repository 请求写入增加完整 expected Binding 条件 | 区分创建与条件更新，关闭 service-read/worker-claim/物理删除间的竞争；prepared 与部署记录按 spec 保留 |
| PAP-CR-007 | 移除 LegacyExecutionDomain 及缺失 selector 的读取回退 | 当前无旧 Scope 数据；ScopeSelector 仅含 pid/cgroup_id，PreparedScope 的 selector 必填；不预留未使用的兼容格式 |
| PAP-CR-008 | 移除 ScopeTemplate、附属枚举与 Scope templateDigest | 2026-09-08 用户确认的 V2 简化；PreparedScope 只含 scopeId/revision/selector，PAP 不生成固定模板或摘要；Policy 同名字段不变 |
| PAP-CR-009 | 移除 PreparedPolicy.templateDigest 和 Binding.executionDomainId 读取兼容 | 前者仅生成/存储/校验格式，未用于去重或 CAS；后者读取后即丢弃。按用户确认删除，旧字段（含 null）拒绝；Policy authored template、IR payloadDigest 及 retired 兼容保留 |
| PAP-CR-010 | 移除 Policy/Scope retired 读取兼容和 PolicyEnvelope.payloadDigest | 2026-09-08 用户确认：retired 读取后即丢弃，payloadDigest 始终为 None 且无消费者。取消 CR-009 暂留的这三项，Policy/Scope 直接派生 Deserialize，已删除字段含 null 均拒绝 |

若未来需要阻止 rename-out，必须先为 source/destination namespace 语义建立 IR 和直接 Adapter
conformance；不能只把所有 `NamespaceMutation` 无差别加入当前 rule。

## 5. Pass/fail matrix

| ID | 验收项 | executable evidence | 结果 |
| --- | --- | --- | --- |
| PAPAPI-001 | compiler 输入、完整 IR 和 delete-only 语义 | `asc-policy-engine/tests/compiler_contract.rs` + `compiler-contract.json` | PASS |
| PAPAPI-002 | process-local Repository 满足当前 PAP request slice | `asc-pap-repository-memory` unit tests | PASS |
| PAPAPI-003 | 15 个 method 的完整 serialized CRUD | `asc-daemon/tests/pap_protocol.rs::real_uds_executes_the_complete_pap_crud_fixture` | PASS |
| PAPAPI-004 | invalid params、domain validation、not-found 与稳定 error | `real_uds_rejects_every_invalid_crud_parameter_class` | PASS |
| PAPAPI-005 | server-owned authorization 且 caller data 不提权 | handler/core tests、all-method deny UDS test、binary DPROC test | PASS |
| PAPAPI-006 | 每个 daemon dispatch 返回有效且唯一的 UUID | shared serialized CRUD runner | PASS |
| PAPAPI-007 | process bootstrap、signal 和 socket cleanup | `dproc_002_003_and_partial_013_binary_registers_pap_and_cleans_socket` | PASS（DPROC-013 partial） |
| PAPAPI-008 | full workspace regression | `cargo test --workspace --locked` | PASS |
| PAPAPI-009 | lint、format 和 API docs | Clippy、rustfmt、Rustdoc commands below | PASS |
| PAPAPI-010 | durable state、target enforcement 和 packaging rollout | 不在本工作包范围 | NOT RUN |
| PAPAPI-011 | Scope selector 必填且仅支持 pid/cgroup_id | `prepared_binding_contract.rs::scope_requires_an_explicit_supported_selector_including_inside_bindings`；`pap_contract.rs`；invalid-requests fixture 经真实 UDS 验证 Create/Update 旧 kind 错误 | PASS |
| PAPAPI-012 | Scope 精简字段及直接消费者 | `scope_contains_only_identity_revision_and_selector_and_rejects_removed_fields`；PAP CRUD/真实 UDS 完整响应；Adapter 固定输出与 Reconciler 完整 fixtures | PASS |
| PAPAPI-013 | Policy 摘要与 Binding 废弃身份字段清理 | `policy_round_trips_without_template_digest_and_rejects_the_removed_field`、`removed_legacy_fields_and_unknown_fields_are_rejected`；完整 PAP/UDS/Adapter/Reconciler 回归 | PASS |
| PAPAPI-014 | 删除 retired/payloadDigest 后严格解码 | `removed_legacy_fields_and_unknown_fields_are_rejected` 和 `canonical_policy_rejects_removed_payload_digest_at_every_embedding_boundary`；独立模型、Policy/Binding 嵌套边界及完整 golden round-trip | PASS |

可重复执行命令：

```bash
cd v2
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps --locked
git diff --check
```

## 6. Direct-consumer evidence 与限制

- `PolicyTemplateCompiler` 通过 `PolicyCompiler` port 被 `PapService` 调用，并由完整 UDS CRUD
  scenario 消费。
- `ProcessLocalPapRepository` 通过 `PapRepository` port 被同一 `PapService` 消费；并发/CAS
  行为由 PAP 和 Repository tests 覆盖。
- `PolicyAdministration` 被 `DaemonDispatcher` 消费；serialized bytes 经真实 UDS，而不是只调用
  Rust struct constructor。
- binary fixture 只能证明当前 host 身份、protocol 注册、permission 和 signal cleanup；不能
  替代安装后 systemd/container/Kubernetes 或真实 target enforcement。
- memory Repository 在进程重启后丢失全部状态，不得作为 durable acceptance evidence。
- 2026-09-08 生命周期修正：`pap-crud-e2e.json` 的删除响应改为同 revision 的
  `bindingPendingDelete`；真实 UDS 验证 Applying Delete、禁止取消删除与同版重试。
  `asc-pcp/tests/pap_lifecycle.rs` 验证真实 PAP + memory + 同步核心的清理、NotFound、
  LIST 移除、新 ID 创建及最大 revision。目标 Client 为脚本替身，不代表 daemon 已接线。
- 2026-09-08 Scope 清理：在 `5a6a3460` 加本次改动上运行 V2 workspace test、Clippy、
  fmt、Rustdoc 和 diff 检查；覆盖 PAP、协议/UDS、Adapter 与 Reconciler 直接消费者。
  此验证不包含 durable Scope 数据迁移或真实 PEP 执行。
- PAP-CR-008 同步更新完整 Scope/Binding 与 Reconciler 输入输出快照，仅移除 Scope
  的 template/templateDigest。显式旧字段在反序列化边界拒绝；不提供静默兼容读取。
  Adapter 移除不再可表达的 lifetime 检查，但既有 PID→process_tree 翻译、DSL 和
  Client 请求 golden 保持不变。Scope 更新幂等性直接比较 selector；其 revision/CAS
  不变。移除仅为 ScopeTemplate 时间校验引入的 time 依赖。
- 随后 PAP-CR-009 移除 Policy templateDigest 的生成、格式检查和完整快照字段；PAP
  不再需要 sha2，serde_json 仅保留为测试依赖。Binding 改用直接派生 Deserialize，
  不再为丢弃 executionDomainId 保留 Wire 类型。现有 Policy 模板比较、revision/CAS、
  Adapter 输出、Client prepared/request golden 不变；无 durable-state 迁移声明。

## 7. Rollback

2026-09-08 PAP-CR-010 验证基线：`f15431ee` 加本次改动；V2 workspace test、Clippy、
fmt、Rustdoc 和 diff 检查通过。正常 golden 本来不含 retired/payloadDigest，因此
无需改写；Compiler、PAP、Adapter、Client 和 Reconciler 的直接消费者回归继续通过。
新增测试拒绝 retired 的 true/false/null，以及 payloadDigest 的有效摘要字符串/null。
仅移除字段和解码兼容，不删除其它用途的 Digest 类型，不代表 durable migration 或
真实 PEP 验证。回退本项需同步恢复模型字段、Wire 解码、编译器 None 初始化及测试；
既有正常输出没有变化。

回滚本工作包时撤销对应 Rust commit，并从 workspace/composition root 移除新增 crate 和 PAP
dispatcher registration。当前 Repository 不写 durable state，因此没有 schema/state downgrade；
停止进程后其状态即消失。若只回滚 contract correction，不得仅恢复 rename-out 文案：必须连同
支持该语义的 IR、compiler、Adapter fixture 和版本化兼容记录一起交付。
仅回退 PAP-CR-007 时，必须一起恢复 Scope enum/读取回退、PAP/协议校验及对应错误
fixtures；不能只恢复 enum 而让旧 selector 通过 authored 请求。当前无旧 Scope 数据，
无需持久化迁移。
回退 PAP-CR-008 时应同步恢复 Scope 数据模型、PAP 生成逻辑、Adapter 检查和完整
fixtures；不能单独恢复模板输入而忽略其中的约束。此精简不涉及 Policy 模板/摘要。
PAP-CR-009 为后续独立清理；回退时需一起恢复 Policy 摘要字段与 PAP 生成逻辑、依赖和
fixtures，以及 Binding 旧字段读取实现。不能只恢复必填字段而使 PAP 返回值缺失它。

## Binding 调度拒绝契约补充（V2）

PendingApply/PendingDelete 允许因入队拒绝直接进入 ApplyFailed/DeleteFailed；PAP 通过
专用 Repository 原子条件写同步记录原因。worker 只认领最新 Pending，已 Failed 的旧唤醒
跳过。GET/LIST 的 status.error 随 status.phase 一起保存，不改变 spec revision 或部署身份。
范围、并发限制、wire fixtures 与可执行 BQA-001～010 验收见
[Binding 队列拒绝验收](BINDING_QUEUE_ADMISSION_ACCEPTANCE_zh.md)。

当前 V2 契约修订：移除 Repository RuntimeState，重试次数和 deadline 仅由 WorkQueue
持有，重建队列时重置。fixture 的 initialSchedule 是测试调用方的内存进度输入，
不属于 initial/expected Repository 记录；旧跨重启预算保持要求已被 CR-020 取代。

### OTel carrier 的后续兼容扩展

原生请求新增可选 version 1 `traceContext/compatibility`，请求新增独立 32 KiB 传播预算；
业务 4 MiB、响应 schema/UUID、授权、revision 和 CAS 不变。现有 CRUD goldens 继续执行；
新版 CLI wire capture 先校验 carrier，再逐项比较原业务 envelope。
新增传播/故障/回滚证据见 [V2 OTel 验收](V2_OTEL_ACCEPTANCE_zh.md)，不扩展此处的持久化或分发验收声明。
