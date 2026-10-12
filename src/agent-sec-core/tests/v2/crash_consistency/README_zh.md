# Policy 崩溃一致性测试

[English](README.md)

本套测试验证 Policy Engine 在 daemon 进程突然退出后的恢复行为。
它运行真实 CLI、UDS 服务、discovery、reconciliation runtime 和 SQLite Repository，
通过受控 AgentSight HTTP mock 控制远端操作。配套 Rust 测试覆盖精确的 Repository/
Reconciler 提交边界，不向产品代码添加 failpoint。

## 目标与证据边界

| 目标 | 设计原因 | 对应证据 |
| --- | --- | --- |
| 已接受的 assignment 无需用户再次请求即可恢复 | 内存通知和 discovery worker 随崩溃消失，已提交的 Scope 必须继续作为事实来源。 | discovery 恢复及 `scope_saved`/`binding_saved` 窗口 |
| 保留 assignment 身份和策略快照 | 更新当前 PolicyTemplate 不应改变已有 Scope 选择的 revision。 | Scope 两次崩溃黑盒场景、Repository 重开契约 |
| 远端 I/O 前持久化清理责任 | Apply 可能远端成功，但响应或本地结果丢失；`UNKNOWN` 表示仍可能需要清理。 | `unknown_saved`、`applied_before_result` 及 Apply/delete 复合窗口 |
| 旧 Apply 结果不能撤销删除意图 | 可以保存旧观察，但不能把 `PendingDelete` 覆盖为 `Ready`。 | 三个复合窗口及 CAS 契约 |
| discovery 停止且最后一个 Binding 删除后才删除 Scope | 提前删除父对象会丢失清理责任；没有匹配实例的 Active Scope 仍需保留以继续发现。 | 两次崩溃间的部分清理、Repository 生命周期契约 |
| 恢复过程中再次崩溃仍能继续 | 重启恢复本身并非原子操作。 | 两次崩溃黑盒场景及 `recovery_saved` |
| 保持 SQLite 跨进程 WAL 锁 | 外部 SQLite reader 不应破坏 SHM 协调或引发 SIGBUS。 | WAL 锁契约、黑盒 `saved()` 的独立只读连接查询 |

故障模型是 **SIGKILL AgentSecCore**，保留同一 SQLite 文件，host boot 不变。
HTTP mock 和目标进程独立存活，除非用例明确终止目标进程。这里不覆盖断电、宿主机重启、
文件系统损坏、AgentSight 重启或真实 eBPF enforcement。
认证也使用 mock：HTTP 服务只检查 Bearer header，不校验 token 的有效性。

恢复不承诺 HTTP exactly-once。结果不确定的 Apply 可以针对同一目标身份幂等重放。
测试明确禁止的是：已持久化 `Ready` 的 Binding 在重启后额外 Apply，以及删除已经取代
Apply 后恢复旧的 Apply。

## daemon 黑盒矩阵

本目录包含以下六个测试：五个默认 CI 场景和一个需显式启用的延期协议缺口复现用例。
通过 RPC、已提交 SQLite 状态和 mock HTTP gate 确认杀进程
窗口，不通过产品 failpoint 选择 daemon 内部的精确指令位置。

| 测试 | 已确认的前置状态与崩溃窗口 | 预期恢复行为 | 当前结果 |
| --- | --- | --- | --- |
| [`test_scope_recovers_discovery_and_preserves_binding_across_two_crashes`](test_scope_recovery.py) | Scope 引用策略 revision 1；没有匹配进程；当前策略更新到 revision 2。杀进程，启动匹配进程，恢复到 `Ready` 后再次杀进程。 | discovery 使用原策略快照创建一个 Binding；第二次重启保留同一 Binding view，不额外 POST；最后显式删除 Scope，清理本地和远端状态。 | 通过 |
| [`test_process_exit_during_crash_cleans_binding_without_repinning_scope`](test_scope_recovery.py) | PID pin 和 `Applying` Binding 已提交；在 Apply POST 前阻塞 mock health 响应。杀 daemon，在停机期间终止目标进程。 | 不发送 POST，移除消失实例的 Binding；保留 Active Scope 和原 PID pin；最后显式删除 Scope 成功。 | 通过 |
| [`test_delete_then_crash_cleans_apply_committed_before_its_reply`](test_binding_recovery.py) | 远端 Apply 已提交，阻塞 HTTP 响应；Scope 删除已提交 `Deleting`/`PendingDelete`，Deployment 仍为 `UNKNOWN`；杀 daemon。 | 恢复删除，清理已登记目标，删除 Binding 和 Scope，不额外 POST。 | 通过 |
| [`test_delete_during_apply_then_crash_cleans_committed_target`](test_binding_recovery.py) | 在协调器执行前阻塞 Apply。接受 Scope 删除，再放行 Apply 到 `after_apply` gate，确认远端已提交后杀 daemon，HTTP 响应仍被阻塞。 | 恢复以 HTTP 204 删除已提交目标，移除 Binding 和 Scope，不额外 POST；禁止崩溃前 DELETE 或恢复时 404。 | 通过 |
| [`test_late_apply_cannot_outlive_confirmed_scope_cleanup`](test_binding_recovery.py) | 已收到的 Apply 在进入协调器执行前被阻塞。接受删除，杀 daemon 并重启；恢复 DELETE 得到 `binding_not_found`，本地清理完成，再放行旧 Apply。 | 本地确认清理后，旧 Apply 不得留下远端目标。 | **需显式启用的已知失败：延期处理的 AgentSecCore/AgentSight 协议缺口** |
| [`test_partial_scope_cleanup_survives_two_crashes_without_reapplying`](test_binding_recovery.py) | 两个 Binding 已 `Ready`；阻塞两个已提交 DELETE 的响应，杀 daemon；重启后放行一个目标的响应，确认剩余一个 Binding 且 Scope 为 `Deleting`，再次杀进程。 | 第二次恢复清理剩余目标并删除 Scope；POST 仅为最初两次，清理恢复不执行 Apply。 | 通过 |

### HTTP gate 与重复运行

[`conftest.py`](conftest.py) 提供四个边界：

| Gate | 到达时已发生的事情 |
| --- | --- |
| `health` | 已收到 health 请求；延迟响应，使 daemon 尚不能准备并发送 Apply。 |
| `before_apply` | 已收到 Apply POST；协调器操作尚未执行。 |
| `after_apply` | 远端 Apply 已提交，协调器锁已释放；延迟 HTTP 响应。 |
| `after_delete` | 远端删除已提交，协调器锁已释放；延迟 HTTP 响应。 |

Apply 中接受删除的用例在创建 Scope 前设置两个 Apply gate。先接受删除，再确认远端
Apply 提交，随后杀 daemon；通过屏障而非 sleep 控制顺序，不声称覆盖恢复清理后才执行
的晚到 Apply。

双目标用例在请求删除前设置两个 DELETE gate。每个 gate 会阻塞所选目标的重复响应，
包括重启 daemon 发出的请求。这样可以保持“部分清理完成”的窗口，不依赖请求顺序，
也不会阻塞无关的协调器操作。

Gate 确认有意义的窗口，但 worker 调度、discovery、HTTP 处理和并发 SQLite reader
仍有竞争。重复运行用于发现这些竞争；它不是失败后自动重试并抹去失败结果。

## SQLite/PCP 精确崩溃矩阵

[`crash_recovery.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/crash_recovery.rs)
杀掉运行真实 Repository 和 `BindingReconciler` 的子进程，使用测试 Adapter/Client
及独立存活的 TCP mock 账本。测试 wrapper 在选定提交或远端操作之后输出 gate；
父进程确认 gate，再发送 SIGKILL。discovery 同步和恢复由测试 harness 驱动，
不验证 daemon 的启动和 runtime 装配。

创建类窗口恢复后，必须有一个 `Ready` Binding，策略保持原快照；如果崩溃前已创建
Binding，其 ID 必须保持不变。最后显式删除后，Binding、Scope、mock 目标均为空。
删除类窗口则必须继续完成已经接受的删除。

| 窗口 | SIGKILL 前的边界 | 选择原因 |
| --- | --- | --- |
| `scope_saved` | Active Scope 已提交，尚无 Binding。 | assignment 不能依赖 discovery 启动或内存通知。 |
| `binding_saved` | `PendingApply` Binding 已提交，尚未 claim。 | 已提交 Binding 不能依赖内存队列项。 |
| `claim_saved` | `Applying` claim 已提交，尚未登记目标。 | 执行者崩溃不能让 Binding 永久卡住。 |
| `unknown_saved` | Apply I/O 前，目标身份和 `UNKNOWN` 已提交。 | 没有确认结果时也必须保留清理责任。 |
| `applied_before_result` | mock Apply 已提交，本地 Deployment 仍为 `UNKNOWN`。 | 覆盖远端成功、本地观察丢失。 |
| `result_saved` | `Ready` 结果已提交。 | 重启不应重新 Apply 已完成的任务。 |
| `delete_intent` | Scope `Deleting` 已提交，discovery 停止屏障尚未完成。 | 恢复须完成屏障并向所属 Binding 传播删除。 |
| `stop_saved` | discovery 停止标记和 Binding 删除意图已提交。 | 已接受删除不能依赖之后的唤醒通知。 |
| `delete_unknown_saved` | 删除目标以 `UNKNOWN` 登记完成，尚未发出 DELETE I/O。 | 删除 worker 丢失后仍需清理。 |
| `removed_before_result` | mock DELETE 已提交，本地删除结果尚未保存。 | 丢失结果后重复清理必须安全。 |
| `delete_result_saved` | Binding 删除及最后的 Scope 删除已提交。 | 已完成清理在重启后应保持完成。 |
| `recovery_saved` | 先在 `claim_saved` 崩溃；恢复提交 `PendingApply` 后再次杀进程。 | 恢复自身的状态转换也必须承受再次崩溃。 |

另外三个窗口把 **未完成的 Apply 与已接受的 Scope 删除组合起来**，补充分别测试
创建和删除无法覆盖的交错：

| 窗口 | 崩溃时持久化状态 | 必须达到的恢复结果 |
| --- | --- | --- |
| `compound_delete_intent` | 远端 Apply 已提交；Scope `Deleting`，Binding 仍为 `Applying`，Deployment `UNKNOWN`。 | 完成 discovery 停止，清理目标。 |
| `compound_pending_delete` | 远端 Apply 已提交；Scope `Deleting`，Binding `PendingDelete`，Deployment `UNKNOWN`。 | 继续删除，不再次 Apply。 |
| `compound_observation_saved` | 旧 Apply 的 `Present` 观察已提交，Binding 仍为 `PendingDelete`。 | 保留删除意图，清理已确认目标。 |

每个复合用例还断言远端操作序列严格为 `apply, delete`。
该文件用两个父测试覆盖 **12 + 3 个窗口**，加上被 Rust harness 收集的 `crash_child`
辅助测试；输出“3 tests passed”不代表只覆盖三个崩溃窗口。

## 相关契约与已知缺口

- [`contracts.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/contracts.rs)
  覆盖 CAS/ABA 冲突、旧 Apply 观察与新删除交错、Scope 原子最终删除、快照准入、
  重开恢复、WAL 锁保持，以及不安全 DB/sidecar 拒绝。这些不全是 SIGKILL 测试。
- [`endpoint_recovery.rs`](../../../v2/crates/asc-policy-repository-sqlite/tests/endpoint_recovery.rs)
  验证持久化清理目标在 endpoint 改变后拒绝发送 I/O、保留责任；恢复原 endpoint、
  轮换 token 后清理成功。它使用 close/reopen，不杀进程。

协议缺口在于**过早解除清理责任**：AgentSecCore 把 `binding_not_found` 当成 `Absent`，
但 AgentSight 不会为未知 ID 持久化删除屏障，旧 Apply 仍可能在 DELETE 之后执行。
本地 `UNKNOWN` 记账和 CAS 无法约束 daemon 崩溃前远端已接受工作的执行顺序。
协议修复延期到当前 PR 之外；复现用例保留原有安全断言，默认 skip，设置
`ASC_TEST_DEFERRED_PROTOCOL=1` 后启用并仍会失败。默认 CI 场景通过不代表协议缺口已修复。

修复 SQLite 锁和测试 harness 后，已通过 SQLite crate 全部 34 项测试，以及四个
非延期黑盒场景连续五轮运行（20 次用例执行）；协议场景单独复验仍失败。
这些结果不能证明超出本故障模型的生产崩溃安全。

## 运行与诊断

使用 Linux，以及项目的 Python 3.11.6 和 pytest 环境。从 `src/agent-sec-core` 构建，
然后在 root 测试环境中运行 daemon 测试：

```bash
(cd v2 && cargo build --locked -p asc-daemon -p asc-cli)
PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency -v
```

Mock 绑定 `127.0.0.1:7396`，须在测试环境停止占用该端口的服务。测试共享此地址，
必须串行执行。fixture 使用 `/var/log/sysak/.agentsight/.dashboard_token`，
保留已有 token，仅删除自己创建的 token。SQLite、socket、日志和复制的目标可执行
文件位于 pytest 临时目录。优先在一次性 root 容器中运行，避免使用已安装系统 daemon
所在的环境。

以下命令分别运行五个默认场景（明确显示延期用例被 deselect），以及显式启用原有
协议缺口安全断言：

```bash
PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency -k 'not late_apply' -v
ASC_TEST_DEFERRED_PROTOCOL=1 PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/crash_consistency/test_binding_recovery.py -k late_apply -v
```

将第一条命令作为独立运行重复执行，记录每次结果。默认完整套件会将协议复现用例报告为 skip。
配套 Rust 检查也从同一组件目录运行：

```bash
(cd v2 && cargo test --locked -p asc-policy-repository-sqlite)
```

设置 `ASC_CRASH_DAEMON_LOG=debug` 启用 daemon 诊断，fixture 不改变 CLI stderr 契约。
Teardown 在每个用例临时目录中保存 `daemon-N.stderr.log` 和 `daemon-N.stdout.log`，
并在捕获输出中打印 daemon 退出码、HTTP 轨迹。关联 Scope/Binding ID、目标 ID、CAS
版本及已输出的 OTel trace/span ID。SIGKILL 可能丢失缓冲日志；应通过已提交 SQLite
状态和 mock 操作轨迹确定实际到达的边界。先区分意外退出、屏障超时和恢复断言失败，
再决定修改产品代码还是测试断言。
