# Binding 队列拒绝：范围、契约与验收

本文定义 V2 PAP/Runtime 的 Binding 队列拒绝契约及验收标准，不改变 V1。目标是让调用方及时知道调度被拒，
并在能够确认时把该请求停止在 FAILED；后续 GET/LIST 可解释原因。成功响应仍只表示
受理或返回当前状态，不证明远端部署完成。

本文适用于实现该契约的版本。发布本身不改变契约，也不要求修订本文；行为、接口或
验收要求发生变化时，应同步更新本文及对应的可执行 fixtures。

## 行为与责任

1. WorkQueue.enqueue 返回成功、Full 或 Stopped。容量按 entries 计；已有 ID 的通知
   在队列未停止时可以合并，即使容量已满。Queue 不读写 Repository。
   保存前 check_ready 只检查 stopped/fatal，不检查容量；Runtime 不可用时返回
   `{requestId,error:{code:"unavailable",message:"reconciliation runtime is unavailable"}}`，
   不保存本次 Binding 意图，也不返回 BindingView。实际存储失败仍映射 internal。
2. PAP 先保存 Pending 意图，再通知队列。Full/Stopped 后调用 PapRepository 的
   fail_pending_binding；同一事务检查原 ID、原 spec revision、原 Pending 状态，
   更新为 APPLY_FAILED/DELETE_FAILED 并写入 status.error。
   不维护或保存重试预算，不修改 spec、revision、deployments 或清理责任。
3. 条件写成功后返回完整 BindingView，响应与受理成功相同，为 `{requestId,result:{spec,status}}`。
   spec 包含已保存 ID/revision；status.phase 为 APPLY_FAILED/DELETE_FAILED，
   status.error.code 为 RECONCILE_QUEUE_FULL/RECONCILE_QUEUE_STOPPED。
   这是调用完成但 Binding 操作失败，不是部署成功。CLI mutation 在 stdout 输出完整结果并返回 1；
   GET/LIST 查询 Failed 记录仍返回 0。参数、鉴权、提交前不可用及未确认状态的错误仍走协议 error。
   条件写失败时只重读，不将旧失败循环应用到新 Pending。状态或 revision 已改变则返回
   当前记录；例如 worker 已认领时返回 APPLYING/DELETING。记录已删除按 NotFound 返回。
4. 存储写入失败，或条件写冲突后仍读到同 revision、同 Pending 时，返回 internal，
   保留具体调度原因并明确“无法确认请求终止，后台仍可能执行”。不伪称已停止。
5. 补偿扫描保留。队列项只是 ID 唤醒；worker 重读状态并 CAS 认领。先扫描后写 Failed
   产生的旧队列项不执行 Failed，不需要记录 queued 标志或删除队列项。
6. BindingView.status 为 `{phase, error?: {kind, code}}`；没有错误时省略 error。
   对 code 做边界校验，非法码投影为 RECONCILE_INTERNAL_ERROR，不暴露远端正文。
   调度拒绝使用 REJECTED 与 RECONCILE_QUEUE_FULL/RECONCILE_QUEUE_STOPPED。
   显式请求恢复 Pending 并清除旧错误；worker 认领进入 Applying/Deleting 也清除旧错误。
   移除 Repository RuntimeState；WorkQueue 持有 AttemptSchedule（次数、deadline），
   自动重试和补扫不重置，新的显式请求/重建队列重置。RetryPolicy 来自配置。
7. 不增加 operation ID，也不改变远端身份。接受同 revision 的 Pending→执行→Failed→
   用户重试 Pending 后旧拒绝才写入的少见 ABA 边界；revision/status CAS 不保证隔离它。

当前 concrete Repository 是 ProcessLocalPapRepository。本次没有 SQLite 接入、落盘、
跨进程 fencing 或重启恢复保证；也不调整队列容量算法、扫描周期或 Policy/Scope 准入。

### 自动执行终止与 slot 释放

自动重调度预算耗尽或单次执行/调度查询 panic 后，worker 保持 Running，重读当前记录，
对原 revision、原 Apply/Delete 意图条件写入对应 Failed 与原因。预算耗尽使用
RECONCILE_RETRY_EXHAUSTED；panic 使用 RECONCILE_WORKER_PANICKED；已存在的终态原因保留。
确认写入成功或记录已终态/缺失后，释放 slot 和 AttemptSchedule。状态补丁不修改 spec、
deployments 或清理责任；Failed 只表示停止执行，不证明远端没有副作用。
新 revision、Delete 意图和 dirty 通知优先；CAS 冲突不使用新快照重写旧失败，交回队列读取新状态。
读写失败或 panic 导致终止无法确认时，才保留 Exhausted 阻止补扫自动重放。
全过程 Repository I/O 在队列锁外，但同一 ID 的 Running 所有权一直保留。

### 调用方重试边界

`bindings.create` 的 ID 由服务端生成，没有幂等键。调度拒绝确认后不回滚创建，
返回的 BindingView.spec.bindingId 可直接用于 GET，并通过相同 spec 的 UPDATE 显式重试。
调用方应检查 status.phase，不能因 CLI 退出码为 1 就盲目重发 CREATE，否则会创建另一个 Binding。
若 Failed 写入无法确认而返回协议 internal，应先核对已保存记录，不能假定没有副作用。

DELETE 的队列拒绝确认后保留 DELETE_FAILED、错误原因及全部目标清理责任；
它不表示远端部署已移除，也不会由补扫自动恢复。调用方须对同一 ID 显式重试 DELETE，
并确认最终 GET 返回 NotFound。此选择保持“明确失败后不自动执行”的契约。

## 可执行验收矩阵

测试名均可用 cargo test 的过滤器单独执行。完整门禁为在 v2 下执行
`cargo test --workspace --all-targets --locked`、
`cargo clippy --workspace --all-targets --locked -- -D warnings` 和 `cargo fmt --all -- --check`。
另须运行 `tests/v2/e2e/` 的真实 CLI/daemon 测试以验证嵌套 status 契约；
RPM 门禁通过 `make test-e2e-rpm-v2` 运行，Cargo 测试不能替代它。

| ID | 判定 | 执行证据 |
|---|---|---|
| BQA-001 | 新 ID 满队列被拒；Queued/Running/WaitingRetry/Exhausted 合并成功；停止明确拒绝 | asc-policy-runtime: admission_capacity_merges_every_existing_state_and_stop_is_explicit |
| BQA-002 | Apply/Delete 拒绝原子保存完整 Failed 记录、原因；保留部署责任；显式重试清除错误并实际执行 | asc-policy-runtime: admission_full_failed_snapshot_stale_wakeup_and_explicit_retry |
| BQA-003 | PAP 写 Failed 后旧快照认领冲突；旧扫描唤醒零 prepare/远端调用 | BQA-002 + admission_pending_cas_preserves_state_and_fences_revision_and_status |
| BQA-004 | worker 先认领则 PAP 不写 Failed；提交后停止也写具体原因 | asc-policy-runtime: admission_worker_claim_wins_or_stopped_failure_is_recorded |
| BQA-005 | revision/status 不匹配不写入；状态和原因一起做 CAS，不保存次数或 deadline | asc-policy-runtime: admission_pending_cas_preserves_state_and_fences_revision_and_status |
| BQA-006 | CREATE 报错包含已保存 ID/revision；Policy/Scope 仍可写 | asc-policy-runtime: admission_create_reports_saved_identity_and_policy_scope_remain_available |
| BQA-007 | Failed 写入失败不宣称终止，保留 Pending，不写入未确认的失败原因 | asc-pap: scheduling_failure_write_error_does_not_claim_terminal_state |
| BQA-008 | 不安全错误码不进入 GET/LIST | asc-policy-runtime: admission_error_projection_sanitizes_untrusted_status_codes |
| BQA-009 | 真 UDS 的 Apply/Delete、Full/Stopped 的完整 mutation 结果及 GET/LIST 与 golden 一致 | asc-daemon: real_uds_scheduling_rejection_and_get_list_match_frozen_wire |
| BQA-010 | CLI mutation 的 Failed 结果输出 stdout 并返回 1；GET/LIST 返回 0 | asc-cli: scheduling_errors_and_binding_reasons_preserve_wire_output |
| BQA-011 | 同进程补扫/重复通知保留重试预算；重建队列不继承次数和 deadline | asc-policy-runtime: queue_retains_retry_progress_across_scan_and_duplicate_wakeup_but_restart_resets_it |
| BQA-012 | 认领时原子清除旧错误；显式重试重置保留的内存进度 | asc-policy-runtime: claim_clears_previous_error_and_explicit_retry_resets_retained_progress |
| BQA-013 | 满队列通过 readiness；stopped/fatal 返回 unavailable；前置拒绝不创建 Binding；存储错误仍为 internal | asc-policy-runtime: readiness_distinguishes_capacity_from_stopped_and_fatal；asc-daemon: unavailable_preflight_returns_wire_error_without_creating_binding、unavailable_reconciliation_only_rejects_binding_writes；asc-daemon-core: pap_errors_are_projected_once_at_the_application_boundary |
| BQA-014 | 耗尽/panic 先写 Failed 后释放容量和进度，保留完整目标记录；未确认写入保留条目 | asc-policy-runtime: terminal_write_releases_capacity_and_preserves_complete_deployment_record、unconfirmed_write_retains_slot_and_conflict_preserves_new_delete、automatic_retries_wait_and_stop_at_budget_without_blocking_other_bindings |
| BQA-015 | Failed 写入期间保持 Running；新 revision/Delete/dirty 不被旧失败覆盖 | asc-policy-runtime: newer_intent_or_notification_is_not_failed_by_old_attempt、failed_write_keeps_running_ownership_while_new_delete_wins_cas |
| BQA-016 | Skipped 查询 panic 不使全局 Runtime 失败，写 Failed 后其他 Binding 继续 | asc-policy-runtime: skipped_read_panic_is_terminalized_without_stopping_other_bindings |

BQA-002 使用完整 prepared-binding fixture，比较完整 BindingStateSnapshot 和远端调用序列。
BQA-009/010 共用 `v2/fixtures/reconciliation/admission-wire.json` 的完整序列化结果。
已有补扫分页、同 Binding 串行、dirty 通知、自动重试和 PAP CRUD 测试继续作为回归门禁。
这些证据不等同于真实 AgentSight/kernel enforcement 或系统服务部署验证。


上述重建测试模拟调度器进程内状态丢失，不声称已实现 SQLite 或真实进程崩溃恢复。

## 本次验证结果

2026-09-11，Rust 1.93.1：BQA-001～012 全部通过。
全 workspace/all-targets 共 250 passed、0 failed、0 ignored；包含真 UDS、CLI 输出与
重建队列后的预算/退避验证。全 workspace Clippy（-D warnings）、fmt 和 diff check 通过。
HTTP/UDS 测试需要本地 socket，沙箱拒绝绑定后已在允许的沙箱外环境重跑。

## 内部变更与回退

内部端口变更：enqueue 返回 Result；PapRepository 增加窄条件失败操作；BindingView.status 改为
包含 phase/error 的结构；生命周期允许 Pending 直接进入对应 Failed。PAP 不新增对 Runtime/PCP 的依赖，
handler 只映射应用错误，daemon 继续负责组装。

外部兼容：相对变更前的接口，status 从字符串改为对象，不兼容只接受字符串状态的客户端。
V2 客户端必须同步读取 status.phase/status.error。本次实现保持 schema version 不变，
这只记录本次变更的版本处理，不作为后续接口变更免于版本管理的依据。
移除 RuntimeState 是内部存储契约变更；当前进程内实现无需磁盘迁移。队列满通过完整 BindingView 的 Failed 状态表达；
客户端不得只看协议 result 就认为操作成功，也不能把 CLI 退出码 1 解释为 Binding 从未创建。

回退需成组撤回端口、实现、消费者、fixtures 和文档，并重新运行上述门禁。不要单独回退
worker 状态检查或 Failed 状态写入，否则可能重新出现“报失败却继续部署”的不一致。
