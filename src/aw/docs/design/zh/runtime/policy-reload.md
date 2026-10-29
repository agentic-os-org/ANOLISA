# 策略热更新与原生旁路接入

[English](../../en/runtime/policy-reload.md)

AW 可以显式更新通过稳定桥接启动的空闲实例策略，原生宿主进程保持运行；忙碌实例拒绝更新。
受限桥接路径通过 `aw run --reloadable` 显式启用，固定 Qoder 1.1.73 TUI 和
QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App，以及 Qoder SDK 1.0.56 / bundled Worker
1.1.78。外部 TUI 和 App 接入通过 `aw agent connect --pid PID` 提供；SDK daemon
外部接入和 steer 仍在规划中。
`qoder-sdk` 的普通 `aw run` 为每个受管 Query 建立常驻子 scope。
普通 QwenPaw App 启动为每个受管 Agent reply 建立任务 scope。
两种普通常驻路径支持显式空闲策略更新；`--reloadable` 保留独立的受限合同。

## 普通 resident 任务快照

普通 Qoder SDK 和 QwenPaw 启动启用同一套显式空闲事务。launcher 持有根实例并关联
存活 worker。受管 Qoder `startDaemon()` 和 QwenPaw App startup 在首个任务前确认就绪。
各受管工厂在任务准备或私有租约申请前登记唯一的完整任务占用。
受限 `--reloadable` 桥接继续使用稳定派发合同。其他普通 Adapter 在完成并验证原生
安全切换点之前保持固定策略，后续接入包含 Claude SDK 和 Pi。启动策略持续生效，
直到更新成功。`Ready` 与 `RequestStart` 是两个独立 RPC；预热 worker 可在尚无任务时
空闲更新。任务预留后取得当时活动策略的快照，占用开始后阻止后续激活。
启动回执始终确认启动配置，与后续任务快照独立。

公共 service 在同一实例锁下串行处理根占用、子任务预留和策略激活。准备中或未确认
的子任务也阻止更新。每个子 Host 固定一份版本，通过 `ScopePolicy` 只公开版本、事件
路由和原生 Hook 描述。工厂先安装快照，再确认子任务并开放派发。CLI 回调在整个任务
周期从子 Host 读取权威路由和预算。快照不包含 Provider 参数、配置或凭据。

子 scope 关闭后，根占用继续覆盖原生回调、严格 Post、受管 MCP 和私有执行租约清理。
清理失败或无法确认时保留占用。下一 Qoder Query 或同一 QwenPaw Agent 的后续 reply
可以取得新激活版本；已有 Qoder Query 始终保留原策略。准入和激活复查 owner/worker
存活状态，包括持久化审计后、发布 Host 前。并发容量使用配置的 binding 上限，完成
占用及时移除，没有累计历史调用上限。快照缺失、确认过期或 owner 失效会阻止任务执行。

`aw update --config aw.yaml --agent TARGET` 在原配置关联的同一服务中选择根实例。
多个匹配要求 `--instance ID`。既有 `aw agent connect --pid` / `aw install --pid` 及
`--remove` 流程继续支持 Qoder TUI、QwenPaw App 的 PID 接入。便捷选择器
`aw update --pid` 延后，与 Adapter 接入独立；服务侧进程关联和身份检查持续生效。
此更新路径不增加跨 namespace 控制。管控侧选择执行容器，在最终路径准备 Hook 文件，并将
更新和任务派发串行化；更新成功后再恢复派发。活跃任务及清理期间返回 `busy`，
不提供自动重试、等待任务排空或任务内切换。

[空闲更新支持矩阵](../../../../../../docs/developer-guide/zh/aw/deployment/policy-updates.md)
记录各启动模式的就绪接点、安全边界及选择入口。普通 Qoder SDK/QwenPaw 扩展公共服务事务，
受限桥接及外部 attach 沿用已有合同；普通 Claude SDK、Pi、Qoder CLI、OpenClaw、Hermes、
AgentScope SDK 保持固定策略。Qoder 预热应先调用受管 `startDaemon()` 再等待用户流量。
无显式 daemon 时，`createQuery()` 须在 30 秒启动期限内初始化，其预留使更新保持 busy，
直到 Query 完整关闭。后续 Adapter 复用更新准入、版本校验和审计，并实现、验证各自的
就绪、任务占用和快照采纳接点。

## Qoder SDK 请求准入

启用 `--reloadable` 时，Query 工厂注册稳定工具路由并包装输入/输出，
使请求准入声明保留到原生清理完成。
每条消息在提交 SDK 之前取得已有 AW 请求声明。同一绑定的受管 Query 共用活动策略，
原生 daemon 仍由应用管理。关联进程是执行 SDK 回调的应用 Worker，而不是调用方任意
提供的 daemon PID。

Worker 1.1.78 可能在生成器清理完成前发出 command completion 和 result。
其公开 `getContextUsage()` 控制请求会排在当前原生请求之后；桥接等待这一只读屏障
完成，才释放对应命令的请求声明。这依赖实测的原生命令循环契约，不仅是版本比较或
收到 `Stop`。屏障失败时，桥接关闭 Query，等待原生关闭握手与尚未完成的回调。
清理失败时保持 busy，直到所属 Worker 退出。后台读取器继续观察终止消息，即使调用方
收到 result 后暂时停止读取。输入耗尽时先等待已有声明释放，避免 SDK 自动结束输入
与控制屏障竞争。

更新复用 `HookPlan`、`UpdatePolicy`、版本检查、策略校验、Provider 准备和审计；
服务层不引入 SDK 代码。扩展 SDK/Worker 兼容组合时，需要复验控制队列屏障、取消和
多 Query 声明隔离契约。外部旁路接入与按 session 独立策略仍是单独的能力。

## 分开生命周期

| 能力 | 前提 | 职责 |
| --- | --- | --- |
| 策略热更新 | 实例已安装可工作的 AW 桥接 | 准备并激活实例策略版本 |
| 原生 attach/detach | 原生 Agent 由外部启动 | 注册或移除桥接，并建立进程关联 |

策略热更新应复用桥接注册。外部接入需要区分注册已请求、桥接已确认和策略已激活，
再放行要求该策略的请求。仅发现 PID 不能完成原生工具准入或结果替换。

保留 `aw run --config FILE --agent TARGET` 作为集成启动入口。
框架继续管理模型、凭据、界面、调度、权限和会话；Adapter 管理原生注册与效果采纳，
服务管理版本选择、预算、关联和元数据审计。用户流程不应要求组装内部 RPC。

## 已实现：离线更新边界

`Configuration::validate_policy_update` 比较两份已经完整校验的配置，仅允许以下部分变化：

- `spec.providers`，包含 Provider 私有配置与 transport。
- `spec.events`，包含步骤、guard、错误动作和事件预算。
- `spec.execution.default_event_budget_ms`。

其他字段必须相同，包括元数据、服务路径、Agent 入口、原生调度设置和可选观测配置。
新增或删除可选的冻结字段也会失败。采用公共字段允许列表，避免将未来新增 Schema 字段
隐式当作可变字段。忽略格式和映射顺序；省略的默认值仍保持省略。

两份输入都经过现有有界解析、Schema 和静态引用校验。Provider 私有配置仍为不透明数据。
比较不运行命令、不修改文件、不启动服务、不探测原生版本，也不授予能力。
错误只报告公共配置部分，不打印配置值。

```bash
aw validate --config next.yaml --against current.yaml
```

成功打印 `policy update scope valid; runtime activation is not checked`，
只说明变化限于策略字段。Provider 发现、私有配置检查、原生支持、就绪确认、闲置确认和
激活仍需运行时准入。基线缺失或无效时失败。原有 `aw validate --config FILE`
保留输出与语义。此比较适用于所有已配置的 Adapter，不证明任何框架支持运行时热更新。

## 已实现：预热与显式空闲激活

预热容器、AW、稳定的 before/after 桥接和可复用的原生宿主。具体 Agent 或 Session
仍可在用户请求到达时由框架创建。先初始化受管 Qoder daemon 或 QwenPaw App、准备
Hook 文件，再于首个请求派发前应用策略。普通 resident worker 和已就绪受限桥接均在
没有任务或清理占用时允许更新。
模型、凭据和 Session 配置继续由框架管理；此操作只修改前述 AW 策略字段。

`aw run --config current.yaml --agent TARGET --reloadable` 即使默认没有用户 Hook，
也会预装稳定回调。`aw status` 返回实例 ID、激活策略版本、就绪、请求/工具数量和进程元数据。
通过 `aw update --config current.yaml --agent TARGET` 应用同一份已编辑的配置，
选择目标的唯一实例并读取当前版本；保留独立 `--policy next.yaml` 形式。多个匹配要求
`--instance`；`--socket` 和
`--expected-revision` 用于显式定位及指定并发前置版本。查询到的版本仍约束激活，冲突不自动重试。
不要求用户组装内部回调。
仅修改配置文件不会激活。

服务代次和启动配置摘要保持不变；更新直接定位现有实例，不按候选摘要另找 daemon。
每个实例保留自己的配置字节、Host 和不可变进程上下文。更新一个实例不改变其他实例，
也不改变后续新 binding 的默认策略。

1. 准备前拒绝不支持、未就绪、存在活跃请求/工具/事件或版本过期的实例；并发更新返回 `busy`。
2. 校验策略范围、原生调度和预算，复用 Provider 发现及私有配置准入准备新 Host。
   准备过程不持有请求准入门。
3. 在与原生请求入口共用的准入门内复查版本、进程身份、就绪及全部请求/工具/事件生命周期。
   新请求先进入时，更新返回 `busy` 并丢弃候选 Host。
4. 空闲时先持久化非终态激活 intent，再于准入门内复查 owner 存活、取消和截止时间。
   发布 Host 后写入唯一 `active` 终态，返回实例、前后版本、审计 key、准备及激活耗时。
   若发布后的终态审计持久化失败，释放准入门前封锁后续派发并停止服务，须重启恢复；
   不回滚，也不写入声称旧策略继续生效的 rejected 终态。

busy、准备失败、冲突和提交前取消都不排队或安排稍后激活；保留旧策略并释放候选。
准备及准入门持有时间有界，不等待工具排空，不替换运行中的工具或 middleware。
同一次工具调用的 before/after 使用同一版本。RPC 超时可能无法确定操作是否已经发生；
应查询实际版本后再决定下一步，不派发要求尚未确认版本的请求。

管控侧负责独占实例分配，并将策略选择与 Session 派发串行化。激活确认不等于预留下一条请求。
策略作用域是实例，不是 Session；一个 App 的多个 Session 共用策略，任一请求活跃都会 busy。
按 Session 分配策略不在首版范围内。

AW 事件数或工具数为零不代表完整请求空闲；模型等待仍为 busy。
请求返回后，原生终结证据继续保留未结束工具（含后台任务）；结束状态未知时保持 busy。
受限桥接跨版本保留原始回调关联声明；resident scope 复用现有回收与已退休身份保护。
旧回调不能重新取得预算。事件证据使用所属 Host 的版本及
策略约束，不再使用服务启动配置。

本地协议为 `aw-service/v1alpha7`，旧 daemon/binding 通过版本和服务代次检查显式拒绝。
进程关联不依赖可选观测，记录 root PID、启动 ticks、UID、PID namespace，交叉确认桥接
进程祖先，并在激活前重新检查生命周期。审计保留关联与切换，不记录 Provider 配置值。


CLI 在支持的 resident/reloadable 启动或原生连接时，记录规范配置路径、socket 和原服务身份。
私有元数据按路径摘要保存在 `$XDG_RUNTIME_DIR/aw/connections`，未设置时使用
`/tmp/aw-UID/connections`，不包含策略或凭据内容。原子替换用户文件不会改变连接。
status、detach 和 update 使用记录的身份，不根据候选配置的 endpoint 或摘要重新选服务。
代次过期或记录权限不安全时明确失败。`aw service stop --config` 删除对应连接记录；
私有锁文件保留到运行时目录清理，避免并发锁 inode 被替换。新的显式 attach 可以接入重启后的
服务，并在原生 readiness 之后、报告成功之前应用当前期望策略。每次 `aw run` 都在选择
服务前检查已有连接。修改文件后重新启动需要先停止记录中的服务；活动实例使用
`aw update`。原服务停止后，显式启动会刷新已有连接。没有连接记录的普通启动根据配置选择服务。

用户只维护一份期望配置。文件编辑属于离线操作；busy 和提交前失败保留内存中的旧策略，
不回写用户文件。提交后终态审计失败会阻止后续派发并停止服务，须重启服务和 worker 后复用。
现有服务快照和策略版本校验不变。按 session 选择策略和自动重试仍不支持。

### 更新权限与审计

运行时更新与修改启动配置需要同等信任：它可以替换 Provider 的 `transport.argv`，
也可以关闭必需的 before Hook。只有受信任的同用户控制面可以通过现有私有服务 socket
及 peer UID 检查提交更新。校验目标服务代次、实例和期望版本；实例 binding 或原生 Hook
回调不代表更新权限。同用户控制不能隔离相同 UID 下的 Agent 或其他租户；需要此边界的
部署必须在本协议之外隔离控制面权限。

启用运行时更新前，每次尝试必须记录仅含元数据的审计：已验证的调用方 UID/PID、
服务代次、实例、新旧策略版本摘要、结果/原因及实际激活版本。不记录配置正文、
Provider 参数、凭据或原生 payload。区分失败、busy 与实际激活；无法持久化必要的
激活 intent 时拒绝激活。Host 发布后终态审计失败会封锁派发并停止服务，不能将其报告为
提交前拒绝或静默回滚。

### 后续：Agent 循环边界的 steer

后续有需求时，可以让显式更新保持 pending，在活跃请求内由 Adapter 确认的安全边界激活。
原生循环通知或规划中的通用 Hook 机制可以提供边界。工具批次结束、等待模型响应的时刻
是候选位置，本身不证明安全：仍须确认无活跃工具、阻止并发派发，并验证 Adapter 能为
后续 Hook 选择新版本，无需重建正在执行的 middleware 链。已经发送的模型请求和执行完的
Hook 不追溯修改。pending/accepted 与 active 分开，实际切换后才确认生效。

首个运行时版本不要求 steer 队列、自动延迟激活、空闲轮询推断或新增通用 Hook 事件。

## Qoder 稳定桥接与外部接入

热更新实例通过临时 `--settings` 注册稳定桥接；后续策略变化发生在 AW，不依赖原生文件监听。
固定 TUI 入口通过 SessionStart、UserPromptSubmit 和 idle_prompt Notification 报告边界。
Stop Hook 可以要求继续运行，因此 Stop 不作为空闲确认。工具失败、权限拒绝只结束工具
生命周期，不增加 Provider 事件。缺少空闲通知时保守保持 busy。
普通 print/TUI 启动保持不变；print 不作为可复用热更新宿主。

外部接入使用独占 HOME 和项目作用域中的原生用户 settings，复用稳定路由。
SessionStart 已发生，因此通过 `ConfigChange` 确认：有限次数的后续配置变更触发新
注册回调，在用户输入前核验来源路径和原生进程，激活 Ready 并写入带 nonce 的凭据。
该确认回调失败不会否决无关的 settings 修改；原生工作目录信任仍由调用者确认。

安装将所属配置组合并到最新 settings。移除先通过 Unbind 原子检查完整请求空闲状态并
关闭服务准入，再将所属工具和生命周期路由替换为临时确认路由。原生采纳该路由证明
工具路由已移除，随后删除确认路由和私有绑定产物。用户配置组与其他字段保留，不恢复
过期的整份配置快照。发布时发现并发写入会保留被交换的文件供检查，并明确报错。
新创建的原生 settings 文件在移除后保留为空文档。

## QwenPaw 稳定桥接与外部接入

预装一个稳定 middleware 桥接。`on_reply` 跟踪完整请求及模型等待，`on_acting` 跟踪实际
工具 generator。回调从 AW 获取激活计划，保留原生调度、拒绝和最终结果替换。
只有正常耗尽 generator 才释放请求/工具声明；原生工具执行前的策略拒绝也会释放该工具声明。
AgentScope 2.0.10 不会将包装层关闭逐层传递到所有子 generator，因此最终响应或包装层
`aclose()` 成功都不能证明原生清理完成。QwenPaw 的 ToolCoordinator 收到最终响应或
ERROR/INTERRUPTED 片段就停止拉取，因此这些输出须等正常耗尽流并发送结束通知后再交付。
终结片段后允许 Toolkit 返回累积响应；终结片段或最终响应后的其他输出会被拒绝。
非终结片段仍流式传递，正常返回的结构化错误在清理后释放占用。确认完成前的提前关闭、取消和异常退出保留 busy；无法确认
结束时应使用新实例。
外部实例通过目标 App 的插件管理接口接入，确认 endpoint、工作目录和执行进程的关联。

热安装复用固定版本的现有 AW 插件及 startup-ready 回调。2.2.2.post1 原生插件 API
在热安装时会执行 startup 回调；安装目录中的私有 `aw-attachment.json` 描述文件
解决运行中 App 无法继承新增环境变量的问题。CLI 验证所选 App PID 拥有 loopback
端口，并要求该 PID 返回带 nonce 的凭据；仅 API 成功不能证明回调成功。
原生验收覆盖已有 Session 的后续请求和第二个独立 profile。

移除先检查完整原生空闲准入，再调用原生卸载 API，核验已安装描述文件和资产仍属于
本次挂载，并保留外部 App。不会替换已为请求组装的 middleware；首次挂载要求 App
位于首请求前的空闲状态。

## 进程关联与所有权

记录服务代次、实例、attachment 和版本，以及 root PID、启动时间、UID、PID namespace、
原生 profile 和框架版本。交叉核验 session/tool-call ID 与回调来源，不仅信任自报字段或
程序名；需要时用 pidfd 跟踪生命周期。这是同用户进程关联，不是跨用户认证边界。

`aw run` 继续管理自己的原生子进程、信号转发和回收。外部 attach 仅管理所属注册、
binding 和回调资源；detach 不终止 Agent。进程退出使关联失效。首版要求调用者停止
进程前显式 detach，不增加进程监督或自动挂载回收器。失败的 detach 保留所有权记录
供重试；进程或服务代次已替换时拒绝清理。

## 交付与回归验收

1. 离线比较：本切片已实现。
2. 固定入口的稳定桥接、实例快照、就绪、显式空闲激活、busy 拒绝及进程关联已实现。
   CLI 接入回归和 AgentScope middleware 测试覆盖这些契约；原生清单要求独立的
   Qoder 1.1.73 TUI 启动及派发前空闲切换用例。完整的托管 TUI 工具轮次采纳及 QwenPaw App
   热更新仍需独立验收，并保留 1.1.64 print 测试。
3. 通过 `aw install --pid` / `--remove` 提供原生 attach/detach，覆盖作用域注册、
   原生确认及 PID/start ticks 不变验收。
4. 逐步扩展其他 Adapter。OpenClaw/Hermes 及 Pi/Claude SDK/AgentScope 是后续候选；
   热更新和旁路接入是独立能力，SDK 接入可以要求预装桥接。

每个运行时接入都需验证同进程增删改、首请求版本选择、准备失败回退，活跃请求、模型等待
和长工具调用期间返回 busy 且不延迟激活，不支持空闲确认、更新与请求准入竞争、并发更新、
所有终结状态、过期关联和清理。steer 后续单独验收。
保留安全与原生拒绝、Tokenless 新调用重试检查、最终授权文本替换、原生调度及现有 CLI/配置行为。元数据审计与观测不依赖可选视图。

声明支持前向回归与原生清单增加精确用例和固定版本。分别测量 Agent 启动、桥接确认、
策略准备和激活；离线检查与回调 fixture 不等于原生采纳验收。
