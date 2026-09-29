# Binding Reconciler 历史验收报告

本报告保留各历史版本的执行结果，测试数、字段和测试名均以对应版本为准。
其中 RuntimeState、跨重启保留预算等描述不适用于调整后的契约；当前验收入口为
[Binding 队列拒绝验收](../../../docs/design/BINDING_QUEUE_ADMISSION_ACCEPTANCE_zh.md)。
`core-output.txt`、`client-output.txt` 等日志保留原始历史内容，不作为当前实现的通过证据。

Runtime 集成版本验证日期：2026-09-09。被验收的源码 HEAD：
`79c460a89ab95d3514e323cde8dd0fad1971e9d1`（`feat(sec-core): binding reconcile orchestrator`）。
本报告按版本记录结果：下表与文末「Runtime 集成验证」对应上述 HEAD；后续 CR-018
错误隔离修正的结果见其独立章节，两个版本的准入行为与测试数不能混用。2026-09-08
各节保留历史阶段证据，不再作为共享执行锁、跨调用 prepared 缓存或 daemon 未接入等旧行为的依据。

该版本已交付同步核心、局部条件写、PAP 提交后通知、WorkQueue、worker、重试定时器、
分页补偿扫描和 daemon 装配。Client 按尝试延迟初始化；不保存跨调用中间结果。
验收类型为 `GREENFIELD_CONTRACT` 与 `ADAPTER_CONFORMANCE`，不保留 V1 runtime。

## Runtime 集成版本结果矩阵（2026-09-09，79c460a8）

| 验证项 | 结果 | 证据 |
|---|---|---|
| 共享 Adapter/Client 契约 | PASS；3 tests | `asc-policy-target-contracts/tests/ports.rs` |
| REC-CORE-001 至 022 | PASS；47 变体 | runner 与 `required-variants.json` 严格对照 |
| 串行故障/重试/状态场景 | PASS；45 变体、70 步 | `core-cases.json`：完整输入/输出及有序 trace |
| 受控并发与 timeout/join | PASS；2 变体 | `concurrency.json`；caller 保留同 Binding 所有权至调用退出；Queue 竞争另见 RRT-001/002/005 |
| 核心状态决策 | PASS；8 tests | `asc-pcp/src/state_tests.rs`，由 `state::tests` 执行 |
| 内存 Repository 依赖契约 | PASS；6 tests | `asc-pcp/tests/repository_contract.rs`：局部写、CAS、观察与意图保护、原子删除 |
| 实际 AgentSight Adapter + scripted Client | PASS；1 test | `actual_agentsight_adapter_uses_the_core_port_without_interface_changes`；完整 plan 输入，本次 prepared 原样传递，不写入 Repository |
| 实际 AgentSight Client | PASS；35 tests | 7 unit + 8 Client + 19 prepared + 1 factory；包含凭据延迟读取及更新后的真实 HTTP Authorization |
| 实际 AgentSight Client 组合 | PASS；5 tests | `asc-pcp/tests/agentsight_integration.rs`：4 条 loopback HTTP 路径及 1 个跨次身份重新准备测试 |
| Client 初始化失败重试 | PASS；1 test | `asc-pcp/tests/client_initialization.rs`：Apply/Update/Delete 初始化失败与下次尝试 |
| PAP 生命周期 + 内存核心 | PASS；4 tests | `asc-pcp/tests/pap_lifecycle.rs` |
| Queue / worker / 定时器 / 补扫 | PASS；13 tests | `asc-policy-runtime/src/reconciliation/tests.rs`；逐项 RRT 映射见文末 |
| daemon 装配及启动 | PASS；2 装配 + 2 binary bootstrap tests | `asc-daemon/tests/reconciliation.rs`、`tests/bootstrap.rs`；不等于完整进程 E2E |
| V2 workspace 回归 | PASS；224 tests | [Runtime 门禁输出](runtime-gates-output.txt) |
| Clippy / fmt / 提交空白检查 | PASS；exit 0 | 同上，具体命令见文末 |
| 完整 CLI→daemon→mock AgentSight E2E | DEFERRED | 独立 PR，不是本阶段门禁 |
| SQL / 跨重启恢复 / 系统性 error injection / live PEP 与 kernel 执行 | NOT RUN / DEFERRED | 内存与 mock 验收不替代这些能力 |

[ACCEPTANCE.md](ACCEPTANCE.md) 的核心、真实组件组合及 Runtime 本阶段门禁有以下可执行证据。
PAP/daemon 通知链路已装配；本报告不宣称真实远端 enforcement 或持久化恢复通过。

## Review 修复增量（历史：Runtime 接入前）

以下为 `cadd2833` 阶段的内部契约修正与 Adapter/Client conformance 历史记录。
该阶段未接 worker；后续 Runtime 已接入。共享锁、缓存、依赖图的历史结论均不得套用到当前版本。

| Review 项 | 处理及可执行证据 |
|---|---|
| 1：DELETE absence | code 独立解析；缺失 retryable 默认 false，429/5xx 保持 retryable；`delete_absence_depends_only_on_status_and_code` |
| 2：HTTP 凭据 | 仅 IP 字面量 loopback 允许 HTTP，其余必须 HTTPS；`bearer_credentials_require_tls_outside_literal_loopback`；localhost HTTP 配置需迁移到 127.0.0.1/::1 |
| 3：ActPlane 来源 | 按确认移除 Adapter 本地 compiler 调用及 Git 依赖，删除测试中的同版本编译断言；保留完整 DSL golden 和语义测试。lockfile 移除 7 包，无依赖升级 |
| 4：依赖审计面 | [DEPENDENCIES.md](../../DEPENDENCIES.md) 登记 TLS/unsafe 来源和发布检查；未更换 ring，也未声称已完成第三方审计或新增 CI 门禁 |
| 5、12：README | 当时区分同步核心与待接 worker；当前 Queue/worker 已由 Runtime 交付 |
| 6：槽位 | 核心读取缺失 Binding 后返回 Skipped，1000 个不同缺失 ID 不产生槽位；存活记录锁身份保持；本次删除完成并确认记账后回收 registry 槽位，旧持有者仍可安全重读缺失 |
| 7：panic | `src/panic_recovery_tests.rs` 的 7 个测试、11 个故障变体通过；完整状态及 trace 检查涵盖 claim 前后、prepare/登记/目标请求、结果提交前后、存储恢复及新意图 CAS。中毒 backend/abort/daemon health 仍属明确边界 |
| 8：boot ID | 合法 UUID 写法按值比较，nil/非法 UUID 拒绝；`replay_compares_boot_uuid_values_and_preserves_request_bytes` |
| 9：daemon 一致性 | 该历史阶段未接入；当前装配、health/join 证据见 Runtime 章节，durable recovery 仍延期 |
| 10：target 数据形状 | 详细设计第 7.0 节记录旧草案退役和当前序列化边界；不恢复无消费者的旧类型/serde envelope |
| 11：UUID feature | v5 只由 Client 请求；`cargo tree -p asc-daemon --edges normal,build,features --locked --offline` 确认无 v5/sha1_smol，也无 ureq/ring/ActPlane；workspace 构建仍允许 feature 合并 |

`cargo test --workspace --locked --offline` 在允许本地 socket 的环境重跑 **PASS**；
沙箱内首次执行的 daemon bootstrap socket 绑定失败不能作为代码回归结论。
全 workspace Clippy、format 和 diff 检查通过。该 review 时的核心 48 变体通过；本次生命周期修正改为 47 变体；
missing 变体 trace 记录一次存在性读取，不产生执行槽位。

本轮进一步将 Repository 收敛为共享层的 `get_binding_state` 和
`compare_exchange_binding_state`；内存 adapter 不再依赖 PCP。执行锁/待提交结果及
claim/register/finish 决策迁入核心，write ID 回执保证 post-commit panic 安全重放。
公开 wire 及已有完整状态序列化形状不变；trace 同步反映读取/CAS 次序。
回退时同时恢复核心、repository、port 实现和上述 fixtures；Client 配置/解析变更
可单独回退，但会重新引入 review 缺陷。当前无数据库迁移或真实 PEP 清理要求。

## 核心阶段可复现命令（历史：2026-09-08）

拆分验证：已从 `b35e6c3a` 导出独立 V2 快照（不含 `asc-pcp`），其 workspace
test、Clippy、fmt 和提交 diff 检查均通过；完整核心版本也通过同样门禁。
Client 的生产及测试依赖均不含 Reconciler/Repository。独立快照的 daemon/UDS
回归需要允许本地 socket；沙箱内启动失败后，放开测试端口限制重跑通过。

从 `v2` 执行：

```sh
cargo test -p asc-pcp --locked --offline -- --nocapture --test-threads=1
cargo test -p asc-policy-target-contracts --locked --offline
cargo test -p asc-agentsight-client --locked --offline -- --nocapture --test-threads=1
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
git diff --check
```

核心 crate 有 39 个 Rust tests（9 个核心状态、6 个存储、9 个 panic/槽位、2 个 fixture、5 个引用解析、4 个 HTTP 组合、4 个 PAP 生命周期组合），该历史阶段其中一个 fixture runner 执行完整的 47 变体，
不能将 Cargo 显示的 `1 test` 误解为只有一个验收场景。
逐项输出见 [core-output.txt](core-output.txt)。完整记录位于 `objects.json`，`$ref`
允许通过语义名称递归引用共享对象，展开后仍是完整记录；禁止字段覆盖、循环、
非字符串或缺失引用，不从运行结果生成 expected。ordered trace 位于两个场景文件。runner 比较实际 trace 和最终完整
记录，出错时输出 expected/actual 差异，并拒绝未消费注入和额外调用。

Client 的独立测试输出见 [client-output.txt](client-output.txt)。迁至核心的 4 条 HTTP 组合路径覆盖：
Apply 成功和 Delete 断连重试、Update 旧目标删除成功/新目标创建失败后重试、
同 revision Delete 在 POST 返回前到达，以及 HTTP 成功后结果写库失败只重试记账。
mock server 比较确切请求方法、路径和 POST 字节；在收到 POST/DELETE 时检查
repository 中对应目标已登记 UNKNOWN，测试结束拒绝未消费的预期请求。

## Fixture 表达精简（2026-09-08）

将无语义编号替换为 `record.create.pending`、`report.replace.partial-retryable` 等
按角色/用途命名的对象。共享完整 Policy、Scope、Binding spec、目标身份和稳定请求，
保留 record 的运行字段及报告观察，不引入状态 patch 或动态生成 expected。

以下为生命周期变更前、单独精简步骤的等价性记录；当前 fixture 数量见文首及 Runtime 集成验证。

| 文件 | 原行数 | 精简后行数 |
|---|---:|---:|
| `objects.json` | 8,626 | 2,029 |
| `core-cases.json` | 4,100 | 3,260 |
| `concurrency.json` | 270 | 165 |

三份数据文件总行数从 12,996 降至 5,454（约 58%）。46 个串行场景的 70 步及
2 个并发场景全部保留；精简前后递归展开的输入、期望记录、调用参数/结果、trace
逐项完全相等。引用解析新增共享对象展开、循环、缺失、覆盖和非字符串 5 个测试。
对象命名与维护规则见 [ACCEPTANCE.md](ACCEPTANCE.md) 第 3 节。

## Runtime 集成版本的内部契约与接入注意事项（79c460a8）

共享 trait 位于 `asc-policy-target-contracts`，数据位于 `asc-policy-types::target`；
Client 不依赖核心或 Repository。实现入口见 [asc-pcp](../../crates/asc-pcp/README.md)。

- `asc-policy-repository` 提供一致聚合读取及局部条件写，`ReconciliationPatch` 不携带 spec。
  PAP 与 Reconciler 共用 memory Repository 的权威 Binding 状态，不要求新增全局 resourceVersion。
- Runtime 的 Running entry 持有同 Binding 调度所有权，贯穿 Client I/O、观察提交及 panic 收尾；
  核心没有第二份共享执行锁。局部 `ExecutionSlot` 仅供本次调用收尾，不能跨调用恢复结果。
- 每次重试读取最新状态并重新 translate/prepare；plan、prepared body、待提交结果不进 Repository。
  Client 返回的目标身份必须在修改请求前登记为 UNKNOWN，成功观察先于生命周期完成。
- Client factory 在尝试内创建实例；Apply 的 prepare 与 create/update 复用同一实例，
  Delete 按保存的 route 创建 Client。具体凭据、UUID、HTTP、DSL、PID 验证仍归具体 Client。
- Retry policy 在首次认领时固定；同意图的普通通知不重置预算。新 Delete 保留 spec revision，
  重置自身重试控制并保留全部可能存在的目标。只有确认所有目标不存在才删除聚合。
- PAP 成功提交后通知 ID；Queue 负责合并、FIFO、dirty、WaitingRetry、容量与有界补扫。
  结果提交失败后退出调用，不保留跨调用补写缓存；下次根据 Repository 中的状态恢复预算并重新执行。
- 核心仍是同步单次 attempt；Runtime 管理实际线程，daemon 管理服务装配和停机。
  timeout 不代表同步 Client 已结束，也不能提前释放同 ID 的调度所有权。

## Runtime 集成版本的外部兼容性与回退边界（79c460a8）

PAP 方法、参数、返回 DTO、授权和 UDS framing 保持不变；Binding mutation 接受意图后
现会触发后台下发，GET/LIST 可观察最终状态。daemon 不暴露 AgentSight endpoint/token 参数。
该版本 Runtime 不可用时拒绝 PAP mutation 并保留查询；凭据和连接失败归 Binding 尝试，
不阻塞启动。后续 CR-018 收窄写准入影响范围，具体以其独立章节为准。

内部 Client registry 改为 `TargetDeploymentClientFactory`；Repository 使用局部条件写，
移除跨调用缓存及核心共享执行锁，调用方、完整 fixtures 和文档必须随接口一起回退。
无 SQL schema 迁移，但不能据此推断回退无远端副作用：daemon 已能调用真实配置的 AgentSight。
部署环境须先清理或交接保留的目标责任，再 drain/join 并停用下发组件；具体步骤见文末。

进程退出会丢失内存意图、目标记录及未提交结果。跨重启遗留策略和跨服务 fencing 仍未验收。
Queue 去重、容量、重试等待、公平调度和补扫已实现；不据此宣称额外的全局速率限制器或
多 PEP 原子下发已经交付。

## Binding 生命周期修正（历史：2026-09-08）

以下保留生命周期修正阶段的验证记录；涉及共享 slot、完整快照写入和缓存的实现描述
已由当前局部写与每次重算契约替代，不是 Runtime 的当前设计。

`bindingRevision` 仅在 spec 更新时 +1；Delete 与同 spec 失败重试保持 revision。
PENDING_DELETE/DELETING/DELETE_FAILED 禁止所有 UPDATE。DeleteFailed 重试重置预算，
保留 prepared/targets；全部 Absent 后以 `BindingStateWrite::delete()` 条件删除聚合。
旧 ID 的 GET/UPDATE/DELETE 为 NotFound，LIST 移除；重新部署通过 CREATE 新 ID、revision 1。

PAP Repository 写入增加 `expected: Option<&BindingView>`，Some 为 update-only，
None 为 insert-if-absent；防止 service 读到的旧记录在删除后被重建。Reconciler 保持
完整快照 CAS、观察先于完成判断及 pending completion 重放。整体删除移除回执，
其重放通过 ID 缺失确认，替换缺失 ID 一律 Conflict。执行槽位仅在删除确认且无待写
结果后回收；已有 Arc 等待者安全重读缺失。新增存储及 panic/响应故障测试覆盖这些边界。

fixture 完成删除的 expected 改为 null，删除 `REC-CORE-019/deleted`，
`REC-CORE-021/fresh-binding` 验证新 ID 的初次下发；共 47 变体。PAP 真实调用的
4 个组合测试另行验证完整删除后创建流程，原始 CAS 注入不替代 PAP 准入验收。
真实 UDS 验证同 revision Delete、Applying Delete、禁止取消删除和失败重试；
完整 CRUD 响应引用改为 `bindingPendingDelete`。

本次验证：完整 workspace 171 tests、核心 39 tests / 47 JSON 变体通过；
Clippy（-D warnings）、rustfmt、Rustdoc 与 git diff --check 通过。

该历史阶段验收是 memory-only + scripted Client / HTTP mock，没有 SQL schema 变更。
后续 Runtime 已新增 daemon worker；跨重启恢复与真实 PEP enforcement 仍未验收。回退生命周期修正必须一起
回退状态规则、PAP 条件写接口、聚合删除语义和对应测试/文档，不能单独恢复 Delete 增版。

## 方法清理验证（历史：2026-09-08）

删除 Client 旧直接入口、PAP status-only 写口、无消费者的 Serialization/is_terminal，
收窄核心裸 mutex/AttemptOutcome。PAP 同次构造校验、Client 批量 target 解析及测试存储
实现去重；外部请求/响应、完整 fixture 和 prepared 字节不变。

核心仍为 39 tests / 47 JSON 变体：25 个 unit tests（含内部锁/异常/fixture 验收），
14 个 integration tests。原 `tests/acceptance.rs`、`tests/panic_recovery.rs` 分别迁为
`src/acceptance_tests.rs`、`src/panic_recovery_tests.rs`，内部断言保留。
Client 34 tests（原 8 个直接入口测试已迁移）；PAP 11 个 service tests + 3 个 validation
tests；memory 2 tests；daemon PAP protocol 9 tests 通过。新增验证包括整批目标修改前
全量校验、旧 revision CAS 冲突、Delete 全状态接纳、原错误文案和编译输出拒绝后不写库。

完整 `cargo test --workspace --locked --offline`、Clippy `-D warnings`、格式与
diff 检查通过；UDS/HTTP 测试在允许本地 socket 的执行环境完成。上述仍是当前内存与
mock 组合验收，不证明真实 PEP 或跨重启恢复。内部接口回退应同步其调用方及测试。


## CR-018：Binding 错误隔离与 PAP 准入修复

2026-09-09，基于 `feat/v2-agentsight-client` / `79c460a8` 的工作区变更。
验收类型为 V2 `GREENFIELD_CONTRACT` 修正；V1 无对应 Runtime，不引入 Python 运行依赖。
内部变化：CAS 重试耗尽返回 `StoreError::Contended`；移除 `storage_errors`；Policy/Scope CRUD
不检查 reconciliation，Binding 仅在 Runtime 不可用、停止或 fatal 时拒绝准入。

| 验收项 | 结果 | 证据 |
|---|---|---|
| register/finish CAS 耗尽 | PASS | `bounded_cas_contention_is_distinct_from_storage_failure_and_preserves_state`：两阶段分别注入 16 次 Conflict，比较完整记录、调用次数和后续成功结果 |
| DJOB-029 单 Binding 错误隔离 | PASS | `binding_errors_retry_without_blocking_other_bindings_or_pap_writes`：Unavailable/Invalid/Contended 三类错误，A 等待时 B、新 Binding 与 Policy/Scope CRUD 继续推进，时钟到期自动恢复 A |
| 补扫 health 与准入分离 | PASS | `scan_failure_degrades_health_without_closing_binding_admission`；stop/fatal 的原有拒绝测试仍通过 |
| DPROC-020 daemon 直接消费者 | PASS | `unavailable_reconciliation_only_rejects_binding_writes`：不可用通知入口下完成 Policy/Scope CRUD，Binding 三类写入被拒绝且没有新增记录 |
| workspace 回归 | PASS，227 tests | `cargo test --workspace --locked --offline`；本地 HTTP/UDS 监听使用沙箱外执行环境 |
| 辅助函数拆分后直接消费者复验 | PASS，87 tests | `cargo test --locked --offline -p asc-policy-runtime -p asc-pcp -p asc-pap -p asc-daemon` |
| 严格 lint / 格式 / 文档 | PASS | `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check`、`cargo doc --workspace --no-deps --locked --offline`、`git diff --check` |

外部兼容性：无 wire 字段、错误码或存储格式变更；故障场景下 Policy/Scope 原先被拒绝的请求
现在可成功，单 Binding 错误不再拒绝无关请求。实际 Repository 错误仍正常返回。
存储错误可能使 Binding 诊断无法落库，此时只输出 ID 与安全错误类别并保留重试责任；
不将 CAS/存储失败解释为远端失败或确认不存在。

边界：以上是内存 Repository、scripted port、回环 HTTP/UDS 及既有 daemon bootstrap 证据；
不证明普通 PAP 并发可自然产生 16 次冲突，也不证明 SQLite 耐久性、跨进程恢复或 live PEP 执行。
回滚时整体撤销 CR-018 的核心错误分类、Runtime/PAP 门禁、直接消费者测试及相关契约，无数据迁移；
回滚会恢复单 Binding 错误阻断所有 PAP 写入的旧行为。

## Runtime 集成验证（2026-09-09）

### 版本、范围与执行环境

- 源码 HEAD：`79c460a89ab95d3514e323cde8dd0fad1971e9d1`。本节对该 commit 的独立源码快照执行验证；
  报告后续提交的 SHA 不替代被测代码版本，也不将后续代码修改自动视为已验收。
- V1 relationship：新增 V2 内部能力；验收类型为 `GREENFIELD_CONTRACT`、`ADAPTER_CONFORMANCE`。
- 直接依赖及消费者版本：下表均由同一 HEAD 的 workspace 构建，版本为 `0.1.0`。
- 工具链：`rustc 1.93.1 (01f6ddf75 2026-02-11)`、`cargo 1.93.1 (083ac5135 2025-12-15)`；
  workspace 声明 MSRV `1.88`，本次未另行验证 MSRV 工具链。
- `Cargo.lock` SHA-256：`10610aff53a0f2ccb8b4d70b7d6651069538f0a2d77097deec39aacc93c5e24e`；test/clippy 使用 `--locked --offline`。
- Linux 执行环境需允许 loopback TCP 和 Unix-domain socket；不要求宿主 AgentSight 或其 token 文件就绪。

| 工作包 / 直接依赖或消费者 | crate 版本 |
|---|---|
| `asc-policy-runtime` | `0.1.0` |
| `asc-pcp` | `0.1.0` |
| `asc-policy-repository` | `0.1.0` |
| `asc-pap` | `0.1.0` |
| `asc-pap-repository-memory` | `0.1.0` |
| `asc-policy-target-contracts` | `0.1.0` |
| `asc-policy-types` | `0.1.0` |
| `asc-foundation-types` | `0.1.0` |
| `asc-policy-engine` | `0.1.0` |
| `asc-policy-adapter-agentsight` | `0.1.0` |
| `asc-agentsight-client` | `0.1.0` |
| `asc-daemon` | `0.1.0` |

### 可复现命令与实际结果

先检出上述源码 HEAD，再从 `v2` 执行以下命令。报告中的 PASS 只覆盖本节列出的版本和阶段。

```sh
git rev-parse HEAD
rustc --version
cargo --version
cargo test -p asc-policy-runtime -p asc-pcp --locked --offline
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
git show --format= --check 79c460a89ab95d3514e323cde8dd0fad1971e9d1 -- .
git diff --check
```

| 命令 / 套件 | 结果 | 数量与输出 |
|---|---|---|
| Runtime + PCP 定向 test | PASS；exit 0 | 53 tests：Runtime 13；PCP 40（24 unit + 5 AgentSight 组合 + 1 Client 初始化 + 4 PAP 生命周期 + 6 Repository 契约）；[完整输出](runtime-output.txt) |
| workspace test | PASS；exit 0 | 224 tests，0 failed、0 ignored；[完整输出](runtime-gates-output.txt) |
| workspace clippy，全部 targets，`-D warnings` | PASS；exit 0 | [输出](runtime-gates-output.txt) |
| workspace fmt check | PASS；exit 0 | 无格式差异；[命令记录](runtime-gates-output.txt) |
| 被验收提交的空白检查 | PASS；exit 0 | `git show --format= --check`；[命令记录](runtime-gates-output.txt) |

53 tests 已包含在 workspace 的 224 中，不能相加。核心 fixture runner 另外覆盖 47 个 JSON
变体（45 个串行场景、70 步，另有 2 个受控并发场景），这是场景数，不是额外 Rust test 数。
先前的 [core-output.txt](core-output.txt)、[client-output.txt](client-output.txt) 仅作为历史阶段输出，
本阶段以新附的两个 Runtime 输出文件为证。输出仅将源码绝对路径规范为 `$CHECKOUT/v2`。

### RRT 门禁逐项映射

下面的 PASS 限定为设计第 10.2 节分配给 Runtime 集成的范围，不扩大到该行的持久化阶段要求。
同一测试可覆盖多个门禁，因此不能用门禁行数充当测试数。缩写对应实际源码：

- R：[Runtime 测试](../../crates/asc-policy-runtime/src/reconciliation/tests.rs)。
- S：[核心状态测试](../../crates/asc-pcp/src/state_tests.rs)。
- P：[Repository 契约测试](../../crates/asc-pcp/tests/repository_contract.rs)。
- L：[PAP 生命周期测试](../../crates/asc-pcp/tests/pap_lifecycle.rs)。
- H：[AgentSight 组合测试](../../crates/asc-pcp/tests/agentsight_integration.rs)。
- I：[Client 初始化测试](../../crates/asc-pcp/tests/client_initialization.rs)。
- U：[panic 收尾测试](../../crates/asc-pcp/src/panic_recovery_tests.rs)。
- C：[核心 JSON 场景](core-cases.json)，由 `acceptance_tests::complete_serialized_core_cases` 执行，
  与 [required-variants.json](required-variants.json) 严格对照，断言完整状态和有序 trace。

| 门禁 | 结果 | 可执行证据 | 已验证范围 / 限制 |
|---|---|---|---|
| RRT-001 | PASS | R::`fifo_coalesces_and_dirty_survives_notification_finish_races`；R::`concurrent_takers_never_claim_one_id_twice` | 同 ID 合并、不同 ID FIFO、并发领取不重复 |
| RRT-002 | PASS | R::`fifo_coalesces_and_dirty_survives_notification_finish_races`；R::`delete_admitted_during_apply_waits_for_exit_and_preserves_cleanup` | barrier 下通知/finish 竞争；Running dirty 保留下一次处理 |
| RRT-003 | PASS | R::`one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`；S::`retry_noop_does_not_reset_attempts_and_bad_cas_does_not_mutate` | 虚拟时钟下退避释放 worker；到期重算、预算不被普通通知重置 |
| RRT-004 | PASS | R::`delete_preempts_waiting_retry_without_waiting_for_clock`；R::`retry_deadline_is_invalidated_by_delete_and_discovery_never_dirties` | Delete 立即唤醒 WaitingRetry；旧 deadline 不重复入队 |
| RRT-005 | PASS | R::`delete_admitted_during_apply_waits_for_exit_and_preserves_cleanup`；H::`same_revision_delete_during_real_http_apply_preserves_target_for_cleanup` | 同 Binding Apply/Delete 不重叠，旧观察保留、Delete 意图不覆盖 |
| RRT-006 | PASS | C::`REC-CORE-019/{not-due,missing,ready,apply_failed,delete_failed,old-notification}`；R::`compensation_pages_past_capacity_without_any_notifications` | 未到期/终态/缺失正确 Skipped；补扫跳过终态 |
| RRT-007 | PASS | R::`capacity_includes_running_and_waiting_and_stop_wakes_takers`；R::`compensation_pages_past_capacity_without_any_notifications`；R::`retry_deadline_is_invalidated_by_delete_and_discovery_never_dirties` | 容量含 Running/WaitingRetry；无通知也能分页发现；发现不把现有任务置 dirty |
| RRT-008 | PASS（内存阶段） | P::`reconciliation_patch_cannot_write_a_spec_or_change_identity`；S::`deployment_only_registration_preserves_concurrent_runtime_without_conflict`；L::`spec_change_clears_prepared_but_keeps_previous_target_for_cleanup` | 局部写保护 spec，spec 修改保留旧目标；SQL 局部事务另验 |
| RRT-009 | PASS（内存阶段） | P::`replay_after_pap_write_is_acknowledged_without_overwriting_new_intent`；P::`stale_removal_cannot_erase_a_newer_status_or_target_observation`；C::`REC-CORE-007/{status,revision}` | CAS 不推进新生命周期；合法观察与新意图保护；不宣称跨进程 fencing |
| RRT-010 | PASS（本阶段） | H::`real_http_apply_then_uncertain_delete_and_retry`（含 `inspect_registered`）；P::`removal_is_atomic_replayable_and_stale_writes_cannot_resurrect_revision_one`；C::`REC-CORE-009/{create-registration,update-registration}` | UNKNOWN 登记先于修改 HTTP；确认不存在后原子删除；保留既有登记失败回归 |
| RRT-011 | PASS | R::`one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`；I::`initialization_failure_retries_apply_update_and_delete_without_losing_target_responsibility`；H::`retry_prepares_current_process_identity_without_storing_it_in_cleanup` | 自动重试重新 prepare；本次 prepare 与 Client I/O 复用同一实例和请求 |
| RRT-012 | PASS（内存阶段） | C::`REC-CORE-010/recompute-after-storage-repair`；H::`result_storage_failure_reprepares_and_safely_replays_http`；`asc-policy-repository/src/lib.rs` 的 RuntimeState/ReconciliationPatch 与完整 fixture 输出 | 无跨调用 plan/prepared/body/pending outcome 存储；SQL schema 与真实新进程验证延期 |
| RRT-013 | DEFERRED | 无持久化 Repository 的关闭重开/进程恢复执行证据 | 不作本阶段门禁 |
| RRT-014 | DEFERRED | 内存预算断言不等于跨重启预算及异常时钟验收 | 持久化阶段验证 |
| RRT-015 | DEFERRED | 已有 H::`result_storage_failure_reprepares_and_safely_replays_http` 为有限回归，不能替代系统性注入 | 持久化提交窗口、PID/boot/route 恢复契约另验 |
| RRT-016 | PASS（本阶段） | R::`shutdown_retains_actual_call_until_join_and_closes_write_admission`；R::`failed_worker_closes_admission_and_shutdown_observes_failure`；R::`storage_failure_waits_without_ownership_and_health_recovers_after_success` | join 前不释放实际调用；准入和 health 正确；强制退出与持久化注入延期 |
| RRT-017 | PASS | R::`pap_create_and_changed_spec_update_notify_after_commit_and_deliver_latest_input`；R::`one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`；R::`delete_preempts_waiting_retry_without_waiting_for_clock` | 新 spec 对应完整新请求；自动重试从头执行；Delete 不额外 prepare |
| RRT-018 | PASS（本阶段） | C::`REC-CORE-010/recompute-after-storage-repair`、`REC-CORE-018/new-delete-budget`；H::`result_storage_failure_reprepares_and_safely_replays_http`；U::`panic_without_committed_result_recovers_budget_and_schedules_fresh_attempt` | 结果未提交保留目标；恢复后重算，不复用跨调用结果，也不重置新意图预算 |

### daemon 与具体 Client 的直接消费者证据

workspace 中的以下测试均为 PASS，已包含于 224 tests：

- `asc-daemon/tests/reconciliation.rs::configured_composition_delivers_pap_intent_and_joins_its_workers`：
  真实 PAP、Runtime、核心、Adapter 与 scripted Client；完整目标 plan、重试控制及 deployment 比对，
  CREATE 最终 READY、DELETE 最终不存在，并 join worker。
- `asc-daemon/tests/reconciliation.rs::unavailable_reconciliation_rejects_writes_but_preserves_queries`：
  不可用通知入口拒绝 PAP mutation，原有查询仍成功。这是降级准入组件测试，不是线程创建失败的进程注入。
- `asc-daemon/tests/bootstrap.rs` 的 2 个 binary tests：真实 daemon 启动、peer credential 授权、
  只读查询、SIGTERM 与 socket 清理；Client 凭据不参与启动。它们不向宿主 AgentSight 下发策略。
- `asc-agentsight-client/tests/factory.rs::factory_defers_credentials_and_refreshes_them_between_attempts`：
  缺失/无效 token 分类为可重试，文件更新前后的 Client 分别发出正确 HTTP Authorization。
- `asc-pcp/tests/client_initialization.rs`：factory 注册不做 I/O，未到期不创建 Client；
  Apply/Update/Delete 初始化失败后正常记账重试，同次准备与下发使用同一实例。

### 内部变更、外部兼容性及延期边界

本阶段补齐 CR-010～017：局部条件写、无跨调用 checkpoint、Queue 独占同 ID、dirty 与
WaitingRetry、到期/补偿、daemon 生命周期和 Client 按尝试初始化。PAP 的方法、DTO 和身份授权
未扩展；新增的是提交意图后的实际后台下发及不可用时的写准入控制，不把 PENDING 当成保护已生效。
Client/Repository 的内部 Rust 接口与相关 fixtures 同步变更，不以历史完整快照写入或缓存重放证明当前行为。

RRT-013～015、RRT-008/009/010/012/016/018 的持久化扩展、跨进程恢复和系统性 error injection
均为 DEFERRED / NOT RUN。完整真实 CLI→daemon→mock AgentSight E2E 单独开 PR，
不要求先有持久化；live AgentSight/ActPlane、BPF/kernel enforcement 另行验收。
内存中的 retry、Running/WaitingRetry 或 UNKNOWN 不提供跨重启耐久性与跨服务 fencing。

### 回滚说明

1. 停止接受新的部署/更新意图，保留受控删除或清理通道。根据当前 Binding 与 deployment
   记录清理或移交全部可能存在的目标，包括 UNKNOWN；不能因本次测试只用了 mock 而跳过部署环境检查。
2. 完成目标责任交接后，停止 UDS 新准入并 drain 已接受请求，再停止 Queue 领取/扫描并 join
   实际 Client 调用。超时不代表远端取消成功；先重启进程会丢失内存目标记录，不能作为清理手段。
3. 按匹配工作包一起回退 daemon/PAP 通知装配、Runtime、PCP、共享 Repository/Client factory
   接口、内存 adapter、workspace/Cargo.lock 及 fixtures/文档。不得留下旧消费者与新接口混用，
   也不得在移除 worker 后继续对外承诺 Binding 自动完成下发。
4. 当前没有 SQL schema 迁移；未来持久化版本必须单独定义 schema/数据回滚。回退后的匹配版本
   重新执行上述 test/clippy/fmt 门禁，并确认 daemon 启停与准入符合所保留的交付范围。

## CR-019：自动重试等待与次数上限（2026-09-09）

源版本：`770464ceb729b301f20821f6e318922fee6a7423` 加本节 CR-019 修订；
`asc-policy-runtime`、`asc-pcp` 和直接消费者 `asc-daemon` 均为 `0.1.0`。

所有自动重试先进入等待；Superseded 与仓储错误默认等待 1 秒，RetryAt 保留核心期限，
已过期的返回期限仍至少等待 1 毫秒。队列默认首次调用后最多自动重试 4 次。
新通知仍可立即打断等待；dirty 优先于旧调用结果，重置队列计数但不重置核心业务预算。
耗尽时保留有界 Exhausted 条目，防止补扫无限重启；保留 Repository 记录和目标清理责任，
不伪造存储中的业务失败。Exhausted 计入容量，队列预算不跨 Runtime 重建。

| 验收项 | 结果 | 可执行证据 |
|---|---|---|
| DJOB-030 自动重试延迟与上限 | PASS | `automatic_retries_wait_and_stop_at_budget_without_blocking_other_bindings`：五类连续结果、期限、完整原记录、调用顺序、2 次重试预算对应总计 3 次调用；耗尽后 tick/补扫无再次调用 |
| 新通知抢占和预算重置 | PASS | `new_notifications_reset_queue_budget_and_preempt_waiting_or_exhaustion`：Queued 合并、Running dirty、WaitingRetry 和 Exhausted 的通知路径 |
| 零预算及等待精度 | PASS | `zero_retry_budget_stops_first_failure_and_submillisecond_delay_is_rejected`：禁止自动重试与拒绝截断为零的等待配置 |
| DJOB-025/028 直接消费者回归 | PASS | 真实 PAP Delete 抢占等待、Apply 期间 Delete 保留目标责任与同 ID 串行；daemon 装配与启停回归通过 |
| Runtime/PCP | PASS | 59 tests：Runtime 18、PCP 41；0 failed、0 ignored |
| Workspace | PASS | 230 tests；0 failed、0 ignored |
| Clippy / fmt / diff | PASS | 全 workspace、all-targets、warnings denied；格式与空白检查通过 |

复现命令（从 `v2` 运行）：

```sh
cargo test -p asc-policy-runtime -p asc-pcp --locked --offline
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
git diff --check
```

外部兼容性：无 PAP wire、Binding revision 或持久化格式变更；新增内部 Runtime 配置
`max_auto_retries` 和进程内队列状态。上述注入仅验证组件分支契约；完整进程 E2E、SQL
持久化、跨重启预算与系统性 error injection 仍按原计划另行验收。

回滚：停止新领取并 join 实际调用，按上节要求清理或移交未确认目标责任；同时回退
Runtime 配置、队列计数/Exhausted、调度转换、测试和契约，无 schema 迁移。
回退会恢复 Superseded 的零延迟重入队及仓储错误的无次数上限调度，不能继续宣称本节门禁通过。

### 默认容量调整（2026-09-09）

默认 entries 上限从 4,096 提高到 65,536，内存随实际条目增长，不预分配全部容量。
`cargo test -p asc-policy-runtime --locked --offline`：18 passed、0 failed；
`cargo fmt --all -- --check` 和 `git diff --check` 通过。此调整未做满容量性能验收，
也未解决 Exhausted 长期占用容量的问题；回滚只需同步恢复默认值及文档。

## CR-020：单次 reconcile panic 隔离（2026-09-09）

源版本：`770464ceb729b301f20821f6e318922fee6a7423` 加 CR-019、默认容量调整及本节修订。
`asc-policy-runtime`、`asc-pcp`、`asc-daemon` 均为 `0.1.0`。

worker 在每次 reconcile 调用外捕获 panic，复用核心既有的结果/失败条件记账。
unwind 收尾与后续状态查询完成前始终保留 Running；已确认终态或缺失时释放条目，
未确认结果（含查询错误或 panic）保留 Exhausted，停止该 ID 自动执行；dirty 新通知优先。
worker 继续处理其它 Binding，不关闭服务或写准入。timer/scanner 与调度代码自身 panic
仍使服务停止，daemon 继续提供其它操作；不新增服务自动重建或持久化中间状态。

| 验收项 | 结果 | 可执行证据 |
|---|---|---|
| DJOB-031 单次 panic 隔离 | PASS | `panic_tests::attempt_panic_records_failure_and_same_worker_completes_next_binding`：完整失败/UNKNOWN 记录、A/B 调用顺序、单 worker 继续工作与 PAP CRUD |
| 已提交成功结果保留 | PASS | `panic_tests::committed_success_survives_attempt_panic_without_replay`：Apply 成功和 Delete 删除后的 panic，不重放目标操作 |
| 未确认结果停止自动执行 | PASS | `panic_tests::unconfirmed_panic_stops_only_that_binding_until_new_notification`：失败记账不可用，查询分别返回 Running/错误/panic；B 继续、新 Delete 可清理、补扫不复活 |
| DJOB-032 新意图与收尾竞争 | PASS | `panic_tests::delete_during_attempt_panic_keeps_dirty_and_cleans_registered_target`、`panic_completion_and_new_notification_race_never_loses_work`：真实 PAP Delete 与 64 次 barrier 竞争 |
| DJOB-027 服务故障边界 | PASS | `timer_panic_closes_admission_and_shutdown_observes_failure`：timer panic 仍关闭服务，shutdown 返回失败 |
| 核心及直接消费者 | PASS | Runtime/PCP 共 64 tests：Runtime 23、PCP 41；0 failed、0 ignored |
| Workspace | PASS | 235 tests；0 failed、0 ignored；包含 daemon 装配与启停回归 |
| Clippy / fmt / diff | PASS | all-targets、warnings denied；格式和空白检查通过 |

复现命令（从 `v2` 运行）：

```sh
cargo test -p asc-policy-runtime -p asc-pcp --locked --offline
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
git diff --check
```

V1 无对应能力；外部 PAP wire、状态枚举及 Repository 格式不变，内部仅调整 panic 隔离边界。
本节取代历史 Runtime 报告中“单次执行 panic 停止整个服务”的行为；历史测试名仍按其版本保留。
此验证不证明 abort、已中毒共享依赖或跨进程故障隔离，也不替代后续 SQL/完整进程 E2E。

回滚：停止领取并 join 实际调用，保留/移交已登记的目标责任；同时撤销单次捕获、队列 panic
收尾入口、对应测试及 DJOB/DPROC 文档，无 schema 迁移。回滚后单次 panic 会重新停止整个
reconciliation 服务，不能继续宣称 DJOB-031/032 通过。

## CR-021：状态转换、迁移分类与共享时钟验证（2026-09-09）

源版本：`770464ceb729b301f20821f6e318922fee6a7423` 加 CR-019～021 及默认容量调整。
`asc-pcp`、`asc-policy-runtime`、`asc-pap`、`asc-daemon` 均为 `0.1.0`。
V1 无对应能力；本节属于当前 V2 内部契约修正及直接消费者验收。

非法状态转换不再回退为原状态，立即返回 `StoreError::Invalid`。不支持的跨 route Apply
以 Rejected 结束并保留原部署责任。Runtime 从 reconciler 取得同一时钟实例，移除
独立 clock 参数；扫描页经 discover_many 一次加锁合并。PAP rustdoc 反映当前提交后通知
及补扫实现，将持久化意图和跨重启验收留给 persistent Repository 工作包。

| 验收项 | 结果 | 可执行证据 |
|---|---|---|
| 非法结果转换 | PASS | `reconciler::tests::invalid_outcome_transitions_return_invalid_without_producing_a_write`：6 种非 running 状态 × 3 种结果分支均返回 Invalid |
| 跨 route 拒绝且不丢清理责任 | PASS | `complete_serialized_core_cases` 中 `REC-CORE-020/cross-route-rejected`：完整记录/调用 trace，无 create/update、无重试期限，推进时钟后仍跳过 |
| DJOB-033 时钟与批量补扫 | PASS | `one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline`：50,000ms 起点与 50,100ms deadline；`batch_discovery_preserves_existing_work_and_applies_capacity_and_deadlines`：完整 entries/ready、去重、容量、停止与到期边界 |
| 核心及 Runtime | PASS | 66 tests：PCP 42、Runtime 24；0 failed、0 ignored |
| Workspace / daemon 消费者 | PASS | 237 tests；0 failed、0 ignored；包含 daemon 装配、启停、PAP 与 HTTP/UDS 回归 |
| Clippy / fmt / diff | PASS | workspace all-targets、warnings denied；格式与空白检查通过 |

复现命令（从 `v2` 运行；HTTP/UDS fixtures 需允许绑定本机 socket）：

```sh
cargo test -p asc-policy-runtime -p asc-pcp --locked --offline
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
git diff --check
```

外部兼容性：PAP wire、Binding revision、状态枚举及 Repository schema 不变。
内部 `ReconciliationRuntime::start` 移除 clock 参数，`ReconcileAttempt` 新增 clock 方法；
BindingReconciler 与全部包装实现、daemon 装配同步适配。自定义包装须返回底层核心的时钟，
该接口不证明任意第三方实现均遵守契约。Invalid 按现有有界单 Binding 调度处理，不关闭全局准入。

第 11 项的全 entries 扫描保留为性能边界；仅合并扫描页加锁，未增加到期索引，未执行
满容量压力/延迟验收。此结果也不证明 SQL 耐久性、跨重启时钟转换或完整进程 E2E。

回滚：停止领取并 join 实际调用，保留/移交已有目标责任；同组回退核心转换与错误分类、
时钟接口及调用方、批量补扫、fixture 和文档，无 schema 迁移。回滚后跨 route 再次进入
有界重试，装配重新允许独立时钟，本节对应门禁不再成立。
