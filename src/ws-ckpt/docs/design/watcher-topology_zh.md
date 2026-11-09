# Watcher 拓扑:共享单 inotify 实例

[English](watcher-topology.md)

本文记录 ws-ckpt 的 watcher 拓扑决策:全部 workspace 共享一个进程级 inotify 实例,而不是每个
workspace 一个 `notify::recommended_watcher`。内容涵盖选择该拓扑的理由、它固有的代价(共享的内核事件队列可能溢出;任意注册 walk 期间全进程事件投递暂停)、这些代价的封闭与可观测设计、共享架构本身,以及后续改动必须保持的不变量。目标是让维护者改动
`src/crates/daemon/src/fs_watcher.rs` 时不必从评审记录里重新推导一遍结论。

范围说明:本文只讨论 watcher *拓扑*——inotify 实例的数量与事件到达 workspace 的路径。静默期判定模型是独立问题(#4049):每个 route 持有一个单调时钟上的最近活动水位,检查只有在连续 200 ms 窗口内没有写相关事件时才报告 `Safe`,见下文"静默期模型"。

## 背景与问题

改动之前,每个 workspace watcher 各自创建一个 `notify::recommended_watcher`,而每个 watcher
都会执行一次 `inotify_init`。Linux 对每用户 inotify 实例数设限
(`fs.inotify.max_user_instances`,默认 128),因此恢复超过 ~128 个 workspace 的 daemon
会让其余注册以 `Too many open files (os error 24)` 失败——而 restore 只对每个 workspace
记一条 WARN 然后继续。在 #7233 报告的 1000-workspace 部署中,~873 个 workspace
静默失去了变更检测:没有计数、没有状态字段、没有聚合告警,唯一的发现途径是翻 journal。

## 单 watcher vs 多 watcher

候选拓扑有三个:

| | 共享单实例(本设计) | 每 workspace 一实例(#7669) | 实例池化 |
|---|---|---|---|
| inotify 实例 | 每进程 1 个 | 每 workspace 1 个 | 每进程 N 个 |
| 每 workspace fd 开销 | ~0 额外 | 3 | 3/N 额外 + 簿记 |
| 大规模下最先撞的内核上限 | `max_user_watches`(描述符) | `max_user_instances`(默认 128) | N > 128 时 `max_user_instances` |
| 事件队列 | 一条共享 `max_queued_events` 队列 | 每 workspace 独立 | 每池一条 |
| 故障影响面 | 溢出波及全部 workspace | 每实例一个 workspace | 每池一片 |

两个测量锚点,来自 #7592 讨论中同机同脚本对本 PR 头(`39db95fa7`)与 #7669(`771b2977e`)的对比:

- **锁不是瓶颈。** 事件路径每事件只做一次读锁 route 查找加原子写;注册类系统调用由互斥锁串行化,而事件循环不碰该锁。纯写入洪峰下两种拓扑耗时只差几毫秒。
- **共享队列是真实取舍。** 所有 workspace 的事件共享一条 `max_queued_events` 内核队列。当一个大目录 workspace 回滚后重新 watch、同时其他 workspace 在写时,队列可能溢出、内核丢事件;对比测试中,与回滚和写入双方都无关的 workspace 约有半数 checkpoint 在溢出窗口内被拒。#7669 不受该场景影响,代价是每 workspace 占 3 个 fd 且必须调高 `LimitNOFILE`。

## 为什么选择单实例

- **确定性预算 vs 概率性丢失。** 每 workspace 拓扑在 1000-workspace restore 完成之前就会撞到确定性的每用户上限(128 实例),而且改动前的失败形态是静默的。共享实例把稀缺资源变成 watch *描述符*(`max_user_watches`)——两种拓扑下递归 watch 本来就共享这个预算——残余风险(队列溢出)是概率性事件,且有 fail-closed 封闭(见下)与可观测计数。
- **这正是 issue 要求的方向。** #7233 的期望行为列有"上限感知与懒起或共享实例";建议修复中包含跨 workspace 共享 inotify 实例。
- **fd 算术。** 1000-workspace restore 在本设计下需要 1 个实例;每实例拓扑需要 1000+ 个(上限 128),这还没算池化的复杂度。

## 固有代价与告警设计

共享队列溢出是拓扑的固有属性,按一等公民的失败模式处理,而不是记日志了事:

- **fail-closed 封闭。** notify 6.1.1 把 `IN_Q_OVERFLOW` 表现为携带 rescan 标志的
  `Ok(EventKind::Other)`——永远不会进 `Err` 分支。`forward_event` 检测到它后递增累计溢出计数、记 ERROR,并把**所有** route 的最近活动水位推进到当前时间。因此每个被 watch 的 workspace 在溢出后都被视为正在写入,直到在一个新的静默期里重新观察到安静:一个吵闹的 workspace 不能在内核丢事件的窗口里把其他 workspace 变成"看起来静默"。
- **可观测面(已实现):**
  - `Request::WatcherHealth` 回复 `WatcherHealthOk { workspaces_without_watcher, watcher_queue_overflows }`;字段是 wire-frozen 的(bincode 按位置编码)。
  - `ws-ckpt status --format json` 报告 `watcher_queue_overflows`(累计值)与 `workspaces_without_watcher`(健康时为空;`serde(default)` 保证对旧 daemon 的线格式容忍)。
  - 人类可读状态表对无 watcher 的 workspace 打警告行,溢出计数非零时打警告行。
  - restore 把 watcher 启动失败聚合成一条 ERROR(失败数/总数、workspace id 列表、`fs.inotify` sysctl 提示)。
- **运维指引。** 任何非零溢出都值得告警:它意味着事件被丢弃、受影响 workspace 在窗口内被按"活跃"对待。溢出持续增长时应调高 `fs.inotify.max_queued_events`、降低 restore/rollback 重挂扫描的并行度,或在静默期重建后安排受影响 checkpoint 重跑。
- **后续加固选项(未实现;记录下来让取舍空间可见):** 基于*速率*的溢出阈值升级为 ERROR 状态行;溢出窗口后对 checkpoint 被拒的 workspace 做重扫;若某部署持续溢出,按 workspace 类别拆成少量实例池。单实例拓扑不妨碍其中任何一项——它们都是路由表之上的增量改动。

### walk 暂停代价(以 fail-closed 封闭;#8448)

上面的溢出是共享队列代价的一半;另一半没有计数器:notify 6.1.1 在其唯一的事件循环线程上执行 `watch()`(对整棵树的 `WalkDir`)以及对新建/移入目录的 auto-add,且 walk 进行期间不读 inotify fd。共享单实例下该线程服务整个 daemon,因此*所有* workspace 的事件都要在内核队列里等到 walk 结束。

真正的风险不是暂停本身,而是错误的 `Safe`。`check_workspace_quiescent` 依据静默窗口结束前送达的事件做决策;当该窗口落进一次 walk 时,水位保持陈旧,决策在 workspace 仍在被写入时返回 `Safe`。per-route coverage epoch 检测不到它:被检查 route 的 epoch、degraded 标记与 watcher 身份都没有变化,因为 walk 属于另一个 workspace。

#7592 评审中的 release 构建实测:写入方每 10 ms 追加一次,检查开始 80 ms 后注册一个无关的 1000 目录 / 100000 文件 workspace(walk 约 170 ms),共享拓扑下检查 5/5 返回 `Safe`,每 workspace 实例拓扑下 0/5;每目录 50 文件(walk 约 87 ms)时 0/5。触发场景包括回滚 re-arm、大 workspace init、heal 路径、启动时 watcher restore,以及 agent 新建/移入大目录时 notify 的 auto-add(`npm install`、`git clone`、`mv`)。

修复保留该拓扑,让静默期观察在暂停周围自行作废:

- **由我们驱动的注册**(`add_route`、重叠释放/re-arm)包在实例级注册 epoch 与 in-flight 计数之间。`watch()` 只在 notify 的 walk 完成后返回,因此区间是精确的。检查开始时若有注册在途则返回 `false`;静默期跨越 epoch 变化则作废。
- **notify 内部的 auto-add** 由触发它的事件识别——`CreateKind::Folder`,或目录的 `RenameMode::To/Both`——并递增实例级 auto-add 序列。回调运行在 notify 的 `handle_inotify` 处理批次内、该批次处理累积的 `add_watches` 之前;因此在回调期间由另一线程发出的控制消息(对一条从未注册的路径执行 `unwatch`)只会在该批次——包括其 walk——结束后才被应答。该应答就是推进 covered 序列的屏障。检查在 `auto_add_covered < auto_add_seq` 时拒绝开始;窗口内出现新的 auto-add 事件则作废决策。
- **idle 快捷路径**(已经过完整静默窗口 ⇒ `Safe`)在任意注册 walk 结束后 250 ms 内禁用:walk 期间产生的事件可能仍在队列里,排空前静默不能作为证据。

两个入口共享这套观察:`WorkspaceWatcher::check_quiescent` 与 daemon 的 `DaemonState::check_workspace_quiescent` 调用同一个 `observe_once`、使用同一份证据,因此 guarded checkpoint、guarded rollback 与常规 snapshot 三条路径不会同 watcher helper 产生偏差。daemon 侧保留自己的 stale-watch heal 与 watcher 身份校验,并在兄弟操作(例如 heal 重挂 watch 与相邻检查交错)作废窗口时先重新观察一次,再判为活跃。

暂停本身仍在——事件依然要等在 walk 之后,运维仍可能看到事件投递延迟,持续压力下仍会出现上面处理的队列溢出。但由"为其他 workspace 的 walk 暂停投递"造成的 `Safe` 决策不会再出现(#8448)。

## 静默期模型(#4049)

每个 route 持有一个最近活动水位:进程启动锚定的单调时钟(`activity_now_ms`)毫秒数,由每个 Create、Modify、Remove 与 `Close(Write)` 事件以 `fetch_max` 推进。只有距水位已过 `QUIET_WINDOW_MS`(200 ms)时检查才是 `Safe`:已过完整窗口则走 idle 快捷路径,否则等待一个窗口后复查。

- **close 也是活动。** 旧模型持有一个 workspace 级 latch,任意 `CLOSE_WRITE` 都会清零。多个写者重叠时(日志轮转或构建步骤关闭文件、同时另一个追加写者仍在写),一次 close 抹掉了另一写者的活动,下一次检查在写入中途走了 idle 快捷路径(#4048)。
- **检查从不修改水位**,并发检查不会互相抹掉证据。
- **单调时钟而非墙钟。** 墙钟向后跳会把 `fetch_max` 水位钉在未来(时钟追上前每次检查都被拒);向前跳会立即放行活跃写者。
- **出生即活跃。** `WorkspaceWatcher::start` 在 watch 挂好后推进水位,因此每条创建路径(首次 init、daemon restore、回滚 re-arm、heal)在首次报告 `Safe` 前都要观察一个完整窗口。watch 建立前落下的写入对 inotify 不可见;重启后的首个 checkpoint 即使 workspace 空闲也要付一个窗口。
- **残余。** 窗口无法区分"写者在两次突发之间暂停"与"写者已结束":间隔低于 200 ms 的写入节奏会被拦住,间隔超过一个窗口的突发之间仍可能被快照。

窗口计算(`observe_once_with`)由调用方注入时钟与等待,其单元测试使用测试内局部的手动时钟,不依赖 runtime 如何调度后台任务。使用真实 inotify 的测试只验证事件能到达水位。

## 共享架构设计

- **句柄。** `WorkspaceWatcher` 只持有 route 身份、最近活动水位、watch 挂载目录的 `(dev, ino)`(挂载前经注册符号链接解析——inotify 观察的是 inode,回滚把 live subvolume 换到路径后面时可据此发现 watch 失效)、以及用于观察 re-arm 的单调 `watch_seq`。
- **路由表。** `routes: RwLock<HashMap<PathBuf, Vec<Arc<AtomicU64>>>>`——一个路径映射到其上存活句柄的水位(通常一个;re-arm 期间短暂两个,这就是值为 `Vec` 的原因)。事件按路径祖先链分发,重叠根内的写入会推进每个包含它的 workspace,与每 workspace 实例的观察一致。
- **锁纪律。** `watcher` 互斥锁串行化注册类变更,且总在 `routes` 之前获取;`routes` 的写锁绝不跨 `watch()`/`unwatch()` 调用持有(notify 事件循环在分发回调里取 `routes.read()`——跨调用持写锁会让自己死锁事件循环)。coverage 表是叶子锁。事件路径除 route 读锁外不取任何锁。
- **发布顺序。** `add_route` 先发布 route 再挂内核 watch:覆盖该路径的描述符在 `watch()` 之前不存在,因此每个可能送达的事件都已能找到 route。注册失败会回滚发布,并释放递归 walk 已装入的部分描述符(notify 6.1.1 自身不回滚)。
- **保重叠的释放。** notify 6.1.1 每路径一个描述符,且递归 `unwatch(root)` 会移除路径以 `root` 为前缀的*所有*描述符——包括重叠幸存者赖以工作的描述符。`unwatch_preserving_overlaps` 在第一个描述符消失前把每个重叠幸存者的水位推进到当前时间并标记 uncovered;然后 unwatch;再逐个 re-arm 幸存者。re-arm 成功后先再次推进幸存者的水位、再清除标记——与 `start` 一样,其静默窗口从递归 walk 完成后才开始计算。re-arm 失败保留 degraded 标记(fail closed),并以有界级联释放自身的部分注册。
- **静默期。** `check_quiescent` 在 route degraded 时 fail closed,并在静默期前同时快照 per-route coverage epoch 与实例级注册/auto-add 代际:一个在观察窗口内开始又结束的中断会让 route 恢复健康、留下一个证明不了任何事的陈旧水位,因此任意代际变化即推翻 `Safe` 决策。注册或 auto-add walk 在途时检查拒绝开始;idle 快捷路径也会等到排队事件排空后才恢复。
- **初始化缓存。** `WatcherCache` 只缓存成功的初始化;瞬态 `inotify_init` 失败(`EMFILE`/`ENFILE`/`ENOMEM`)由下一次注册重试,而不是让 watcher 死到 daemon 重启。

## 一致性指引

后续改动必须保持的不变量:

1. 所有 workspace watcher 经 `SharedWatcher` 注册;任何代码不得为 workspace 路径直接构造 `notify` watcher。
2. 活动语义:Create/Modify/Remove 与 `Close(Write)` 都以 `fetch_max` 在单调时钟 `activity_now_ms` 上推进 route 水位;任何代码都不降低或清零水位,检查只读取它。
3. fail-closed 方向:对覆盖或丢事件的任何疑虑都判向"正在写入";静默永远不能读作 quiescent。
4. route 发布先于内核挂载;最后句柄的取消发布与水位摘除是原子的。
5. 任何 unwatch 必须保重叠幸存者(推进水位、标记、re-arm、再推进水位)。
6. `watch_seq` 每次 `start` 单调递增。
7. `WatcherHealthOk` 字段 wire-frozen;只允许追加式变更。
8. `watcher_shared_instance` 套件刻意独立成测试二进制(经 `/proc/self/fd` 数实例,与 lib 测试共享进程会糊掉计数);保持隔离。
9. 每次内核 `watch()`/`unwatch()` 都必须在 `RegistrationGuard` 内执行;未包住的注册会重新引入针对所有 workspace 的 #8448 假 `Safe` 窗口。
10. 目录 create/移入事件必须递增 `auto_add_seq` 并挂起屏障;只有当 `auto_add_covered >= auto_add_seq` 且 settle 窗口关闭时,检查才可信任静默。
11. lib 测试在 `cfg(test)` 下把共享实例按线程隔离(`SHARED_WATCHER` 为 thread-local):quiescence 是实例级语义,同一进程内跨测试共用一个实例会让互不相关的测试互相判定 `Writing`。生产仍是全进程单例;多路由共享语义由 `watcher_shared_instance` 集成套件覆盖。

## 参考

- #7233 —— 原始故障(大规模下 watcher 静默丢失)。
- #7669 —— 每 workspace 实例 + 告警 + 硬拒绝的替代方案。
- #4048 / #4049 —— 一个写者的 close 清掉另一写者的活动;即上述最近活动水位。
- #7592 讨论 —— 同机对比数据,以及塑造重叠处理与 epoch 机制的各轮评审。
- #8448 —— 注册 walk 期间的假 `Safe` 窗口;即上述注册 epoch 与 auto-add 屏障。
