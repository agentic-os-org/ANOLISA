# Watcher 拓扑:共享单 inotify 实例

[English](watcher-topology.md)

本文记录 ws-ckpt 的 watcher 拓扑决策:全部 workspace 共享一个进程级 inotify 实例,而不是每个
workspace 一个 `notify::recommended_watcher`。内容涵盖选择该拓扑的理由、它固有的代价(共享的内核事件队列可能溢出)、该代价的封闭与可观测设计、共享架构本身,以及后续改动必须保持的不变量。目标是让维护者改动
`src/crates/daemon/src/fs_watcher.rs` 时不必从评审记录里重新推导一遍结论。

范围说明:本文只讨论 watcher *拓扑*——inotify 实例的数量与事件到达 workspace 的路径。静默期判定模型(latch 清零 vs 最近活动窗口)是 #4049 处理的独立问题。

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
  `Ok(EventKind::Other)`——永远不会进 `Err` 分支。`forward_event` 检测到它后递增累计溢出计数、记 ERROR,并把**所有** route 的写活跃标志置为"writing"。因此每个被 watch 的 workspace 在溢出后都被视为正在写入,直到在一个新的静默期里重新观察到安静:一个吵闹的 workspace 不能在内核丢事件的窗口里把其他 workspace 变成"看起来静默"。
- **可观测面(已实现):**
  - `Request::WatcherHealth` 回复 `WatcherHealthOk { workspaces_without_watcher, watcher_queue_overflows }`;字段是 wire-frozen 的(bincode 按位置编码)。
  - `ws-ckpt status --format json` 报告 `watcher_queue_overflows`(累计值)与 `workspaces_without_watcher`(健康时为空;`serde(default)` 保证对旧 daemon 的线格式容忍)。
  - 人类可读状态表对无 watcher 的 workspace 打警告行,溢出计数非零时打警告行。
  - restore 把 watcher 启动失败聚合成一条 ERROR(失败数/总数、workspace id 列表、`fs.inotify` sysctl 提示)。
- **运维指引。** 任何非零溢出都值得告警:它意味着事件被丢弃、受影响 workspace 在窗口内被按"活跃"对待。溢出持续增长时应调高 `fs.inotify.max_queued_events`、降低 restore/rollback 重挂扫描的并行度,或在静默期重建后安排受影响 checkpoint 重跑。
- **后续加固选项(未实现;记录下来让取舍空间可见):** 基于*速率*的溢出阈值升级为 ERROR 状态行;溢出窗口后对 checkpoint 被拒的 workspace 做重扫;若某部署持续溢出,按 workspace 类别拆成少量实例池。单实例拓扑不妨碍其中任何一项——它们都是路由表之上的增量改动。

## 共享架构设计

- **句柄。** `WorkspaceWatcher` 只持有 route 身份、写活跃标志、watch 挂载目录的 `(dev, ino)`(挂载前经注册符号链接解析——inotify 观察的是 inode,回滚把 live subvolume 换到路径后面时可据此发现 watch 失效)、以及用于观察 re-arm 的单调 `watch_seq`。
- **路由表。** `routes: RwLock<HashMap<PathBuf, Vec<Arc<AtomicBool>>>>`——一个路径映射到其上存活句柄的标志(通常一个;re-arm 期间短暂两个,这就是值为 `Vec` 的原因)。事件按路径祖先链分发,重叠根内的写入会点亮每个包含它的 workspace,与每 workspace 实例的观察一致。
- **锁纪律。** `watcher` 互斥锁串行化注册类变更,且总在 `routes` 之前获取;`routes` 的写锁绝不跨 `watch()`/`unwatch()` 调用持有(notify 事件循环在分发回调里取 `routes.read()`——跨调用持写锁会让自己死锁事件循环)。coverage 表是叶子锁。事件路径除 route 读锁外不取任何锁。
- **发布顺序。** `add_route` 先发布 route 再挂内核 watch:覆盖该路径的描述符在 `watch()` 之前不存在,因此每个可能送达的事件都已能找到 route。注册失败会回滚发布,并释放递归 walk 已装入的部分描述符(notify 6.1.1 自身不回滚)。
- **保重叠的释放。** notify 6.1.1 每路径一个描述符,且递归 `unwatch(root)` 会移除路径以 `root` 为前缀的*所有*描述符——包括重叠幸存者赖以工作的描述符。`unwatch_preserving_overlaps` 在第一个描述符消失前把每个重叠幸存者置为"writing"并标记 uncovered;然后 unwatch;再逐个 re-arm 幸存者。re-arm 失败保留 degraded 标记(fail closed),并以有界级联释放自身的部分注册。
- **静默期。** `check_quiescent` 在 route degraded 时 fail closed,并在静默期前快照 coverage epoch:一个在观察窗口内开始又结束的中断会让 route 恢复健康、留下一个证明不了任何事的静默标志,因此 epoch 变化即推翻 `Safe` 决策。
- **初始化缓存。** `WatcherCache` 只缓存成功的初始化;瞬态 `inotify_init` 失败(`EMFILE`/`ENFILE`/`ENOMEM`)由下一次注册重试,而不是让 watcher 死到 daemon 重启。

## 一致性指引

后续改动必须保持的不变量:

1. 所有 workspace watcher 经 `SharedWatcher` 注册;任何代码不得为 workspace 路径直接构造 `notify` watcher。
2. latch 语义:Create/Modify/Remove 置 writing=`true`,只有 `Close(Write)` 清零。
3. fail-closed 方向:对覆盖或丢事件的任何疑虑都判向"正在写入";静默永远不能读作 quiescent。
4. route 发布先于内核挂载;最后句柄的取消发布与标志摘除是原子的。
5. 任何 unwatch 必须保重叠幸存者(置位、标记、re-arm)。
6. `watch_seq` 每次 `start` 单调递增。
7. `WatcherHealthOk` 字段 wire-frozen;只允许追加式变更。
8. `watcher_shared_instance` 套件刻意独立成测试二进制(经 `/proc/self/fd` 数实例,与 lib 测试共享进程会糊掉计数);保持隔离。

## 参考

- #7233 —— 原始故障(大规模下 watcher 静默丢失)。
- #7669 —— 每 workspace 实例 + 告警 + 硬拒绝的替代方案。
- #4049 —— 静默期模型(与拓扑正交)。
- #7592 讨论 —— 同机对比数据,以及塑造重叠处理与 epoch 机制的各轮评审。
