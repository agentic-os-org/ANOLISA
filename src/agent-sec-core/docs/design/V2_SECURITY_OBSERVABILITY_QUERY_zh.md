# V2 安全事件与 Observability 查询设计

状态：**[TARGET V2：obs 已实现，已有源码 E2E 与 AgentSight 联调证据]**。验证范围见 §9.2。
本分支在 `trace@ef4fa4029` 采集基线上实现三个 obs RPC、daemon-only report/review、
可信 UID 写入和查询隔离。本地 schema CLI 已补齐；基线 PR #6745 提供四个 sec RPC 和 events CLI，
其契约见 [daemon 协议](DAEMON_PROTOCOL_V1_zh.md#64-secsummary)。源码进程验收不代表已发布 RPM/systemd 安装态验收。
V1 是功能验收基线；接口可调用、基础下钻可用或已有测试通过，均不代表完整迁移完成。
逐项功能验收及有意差异见 §8.1；不将源码验证等同于全部部署环境验证。

## 1. 范围与决策

1. CLI/TUI 的持久化数据读取全部通过 daemon；不得打开 SQLite/JSONL，也不得在 daemon
   不可用时回退本地读取或自动启动 daemon。
2. 普通用户仅查询当前连接 **UDS peer UID** 的数据；UID 0 的 root 默认查询所有数据。
   daemon 从内核认证身份构造 `QueryScope::Own(uid)` 或 `QueryScope::All`，不接受客户端授权声明。
3. 身份来自 UDS peer credentials。响应中的 `uid` 是数据归属，不是客户端授权声明。
   非 root 的 `PolicyAdministrator` 不获得跨用户读取权限。
4. 这是 OS 用户隔离。同一 UID 下的 Agent 共享可见范围；全部以 root 运行的 Agent
   不能靠 UID 相互隔离。经代理连接时归属为 daemon 实际看到的 peer UID，不信任转发字段。
5. 复用现有 transport、Dispatcher、reader 和 repository，按下面的用例拆模块；
   不为每个 RPC 新建 crate，不引入独立查询 daemon、后台队列或缓存。

本文补充 [Rust 迁移总计划](AGENT_SEC_RUST_MIGRATION_zh.md) 的查询工作包。
[单条采集契约](V2_OBSERVABILITY_INGESTION_zh.md) 描述已实现的 `obs.record`；
本分支通过存储侧扩展为该采集接口增加可信 UID，不改变采集 wire 与 JSONL。

## 2. 采集基线与可复用能力

以下以采集基线 `ef4fa4029` 描述迁移来源；本分支实际交付见 §9。

| 能力 | 可复用源码 | 查询设计扩展 |
|---|---|---|
| V1 daemon 查询 | [security_query.py](../../agent-sec-cli/src/agent_sec_cli/daemon/handlers/security_query.py) | 迁移 7 个方法的业务语义 |
| V1 events CLI | [cli.py](../../agent-sec-cli/src/agent_sec_cli/cli.py) | RPC 客户端、过滤和输出兼容 |
| V1 observability CLI | [observability/cli.py](../../agent-sec-cli/src/agent_sec_cli/observability/cli.py) | report/review 的 RPC 数据源 |
| 安全事件 reader | [security_events/reader.rs](../../v2/crates/asc-persistence-sqlite/src/security_events/reader.rs) | 已有 query/get/count/count_by/summary/关联候选查询，需强制 UID 范围 |
| 可观测 reader | [observability/reader.rs](../../v2/crates/asc-persistence-sqlite/src/observability/reader.rs) | 已有 session/run 列表与计数、事件列表，需 UID 范围和服务装配 |
| 安全摘要 formatter | [asc-security-summary](../../v2/crates/asc-security-summary/src/lib.rs) | CLI 对 daemon 返回的已授权数据进行文本展示 |
| 关联匹配 | [V1 correlation.py](../../agent-sec-cli/src/agent_sec_cli/observability/correlation.py) | V2 仅有候选查询，需迁移完整匹配与结果投影 |
| 会话报告 | [V1 session_report.py](../../agent-sec-cli/src/agent_sec_cli/observability/session_report.py) | 通过 RPC 取得数据后聚合和格式化 |

V1 CLI 直接读本地 SQLite，V1 daemon 则另有查询 RPC；V2 保留业务能力，但统一经 daemon。
当前 V2 的 [方法清单](../../v2/crates/asc-daemon-protocol/src/method.rs) 已注册三个 obs query 方法；
[CLI 注册](../../v2/apps/asc-cli/src/commands.rs) 已提供 observability report/review。
以下表格保留完整迁移范围；四个 sec RPC 与 events CLI 已由基线 PR #6745 提供。

## 3. CLI 接口 [TARGET V2]

| 接口 | 行为 | daemon 数据来源 |
|---|---|---|
| `events` | 过滤、分页；table/json/jsonl 输出 | `sec.events.list` |
| `events --count` | 匹配事件数，保留 V1 offset 语义 | `sec.events.list` 的 total |
| `events --count-by <field>` | category/event_type/trace_id 分组，输出 JSON object | 无 offset 时用 `sec.events.count_by`；有 offset 时分页读取后按 V1 语义聚合 |
| `events --summary` | 安全态势文本摘要，未指定时间时默认最近 24 小时 | `sec.events.list`，复用摘要 formatter |
| `observability report --session-id <id>` | 指定授权范围内的会话，`--format text/json` | session/run/timeline；session 附带安全聚合 |
| `observability report --last` | 授权范围内最近有记录的会话 | `obs.sessions.list` 后执行同一报告路径 |
| `observability review` | TUI：session → run → event，下钻详情与关联安全事件 | `obs.sessions.list`、`obs.runs.list`、`obs.timeline.get`（含完整关联详情） |

Observability CLI 保持 V1 参数集合：`report` 仅提供 `--session-id`、`--last`、`--format`；
`review` 无业务参数，从 session 列表开始下钻。daemon 的时间过滤参数保留供其他查询客户端使用。
Report JSON 与 V1 frozen fixture 完整对照，不新增顶层 `uid`；RPC 的归属字段仅用于内部隔离。

`events` 保留 `--event-type/--category/--trace-id/--session-id/--run-id`、
`--since/--until/--last-hours`、`--limit/--offset`、`--output/-o`。
默认 limit 为 100、offset 为 0、输出为 table；保留 V1 参数互斥与错误退出语义。
CLI limit 可以通过多个有界 RPC page 完成，不能把 daemon 单页上限当成 CLI 总量上限。
`--count` 为 `max(total - offset, 0)`；daemon total 自身不受分页 offset 影响。
`--count-by` 的 offset 是先跳过按时间倒序排列的事件再分组，不是跳过分组结果；
V1 RPC `sec.events.count_by` 拒绝 limit/offset，因此 CLI 不能直接透传这些参数。

`--summary` 与 `sec.summary` 不是同一输出：前者保留 V1 安全态势文本，后者是 dashboard
聚合数据。V1 文本摘要最多读取 10,000 条；迁移先保留此上限，不宣称它是无限量全库分析。
Report 统计 LLM 次数、请求/响应字节数、工具次数，以及按 category/result 的安全事件数量；
不得将 result 的 succeeded/failed 改称检测 verdict 的 pass/deny。
Report/review 只能在 daemon 已授权的结果上聚合或展示；完整报告必须遍历所需分页，
中途失败不得把部分数据当成完整报告。跨请求不承诺全局快照一致性。

完整设计保留 7 个查询 RPC；本次交付其中三个 obs 方法，不新增 `sec.events.count` 或 `obs.session.report`。
将来确需 daemon 直接生成报告时另行定义 report RPC，不能当成 V1 已有方法。
`observability schema` 在本地按当前 Rust hook、metadata shape 与 metrics 契约动态生成，
不读持久化数据，不需要查询 RPC 或有效 socket。生成逻辑与采集共用 hook/metadata 字段规则；
V1 JSON 快照仅存放于 fixtures 中作完整输出对照，不参与运行时生成。此 schema 保留 V1 的
规范字段格式；别名、时间归一化、截断及“至少一个已知 metric”等自定义规则仍由 Rust 执行。

## 4. daemon 接口 [TARGET V2]

方法名和业务字段以 V1 handler 为兼容基线；外层使用现有 V2
`{requestId,result}` / `{requestId,error}` 信封。`caller="agentsight"` 选择 V1 的
`request_id/ok/data/stdout/stderr/exit_code` 信封，不依赖 `trace_context`；obs 结果按冻结的
V1 字段投影，sec 业务结果原样透传。授权仍只依赖 UDS peer UID。

七个查询方法的参数、返回字段及分页默认值统一见
[daemon 协议 §6](DAEMON_PROTOCOL_V1_zh.md#6-method-catalogue)。本节仅说明迁移语义与安全边界。

全部 7 个目标查询方法的授权范围均由 UDS peer 决定：root 使用 All，非 root 使用 Own(peer_uid)。
sec 按原始 session ID 筛选；root 返回所有 UID 下的匹配记录。
root 返回中的同名 session 显示为 `UID_session_id`，仅用于区分归属；查询不解析展示前缀。
分组及 affected_sessions 按各 UID 的 session 分开统计。obs 的组合 ID 下钻保持 §4.1 的现有行为。
未知来源安全库须先按 §5.2 隔离。

### 4.1 过滤、分页与关联

- 保留 V1 时间解析与非法范围处理的 fixtures，不能静默改变时区解释。
  分页 offset 保留 V1 signed 64-bit 上界，内部 reader 不得截断。
- 所有列表、total、分组、summary、详情和关联候选查询都先应用服务端授权范围，再执行
  业务过滤、排序、LIMIT/OFFSET 或聚合。禁止先取跨用户结果再在内存中过滤。
- 安全事件按时间倒序；session 按最近活动倒序；run 和 run 内记录按时间正序。
  同时间使用稳定的记录 ID 排序，分页结果可重复验证。
- Timeline 的 limit/offset 针对 observability 行，关联项追加后再按时间/kind 排序，
  items 数量可能大于 limit。客户端推进 offset 时统计 observability 项，不能用混合 items 数量。
- Timeline 保留 V1 关联优先级：tool_call_id 精确匹配、before_agent_run 的 run 匹配、
  适用场景的字段加时间匹配；保留 hook/category 限定、每类最多一个结果、reason/rank/time_delta。
  单条与批量关联结果必须一致。安全事件计数按 session/run 归属统计，与匹配出的关联项数量不同。
- Session、run、correlation 的完整内部 key 包含 UID；同名业务 ID 不能跨 UID 合并。
  正常 GUID 无随机碰撞时保持原 SessionId。root 全量范围内如果同一 SessionId 属于多个
  UID，session 列表的 `session_id` 改为 `UID_SessionId`，未知 UID 使用 `unknown_SessionId`；
  普通用户仍只看到自己的原 SessionId，不能通过名称获知其他 UID 是否存在。
  碰撞判断覆盖整个库，不受分页或时间筛选影响。root 的 session 精确筛选、run/timeline
  接受列表返回的组合 ID，并在服务端还原原始 session 和数据 UID 后查询。
  组合 ID 是资源定位符，不是授权凭据；非 root 不解析其 UID 前缀。
  原始重名 ID 直接下钻仍报 invalid_argument；组合名若也与另一个真实 SessionId 重名，
  同样报错，不猜测目标。不增加映射表或改写数据库中的原 SessionId。
  Report/review 原样传递选中的 session_id；事件 metadata 和关联匹配仍使用原始 ID。
  新增/清理数据可能改变是否重名，客户端应刷新列表，不承诺跨请求的定位符快照。

### 4.2 失败与资源边界

参数错误返回 invalid_argument；服务未装配、存储尚不可用返回 unavailable；
查询超时返回 deadline_exceeded；超过响应/资源预算返回 resource_exhausted；其他存储错误返回 internal。
错误不回显原始内容、数据库路径或其他用户记录是否存在。
身份范围内无记录是成功的空结果；不存在/不可见的 session/run 返回空列表。
Observability 主数据和安全计数的存储损坏、无权限或不支持的 schema 显式报错。
Timeline 的可选安全关联恢复 V1 的可用性优先行为：候选查询先按 UID 过滤，再按时间/event_id
正序最多读取前 1000 条，不检测或报告候选截断；坏记录跳过，关联存储故障返回空关联，
继续展示 observability 页面并写安全诊断。权限错误和取消/deadline 不降级。

复用现有 blocking dispatch、取消和 deadline 机制，沿用 V1 query 的 5 秒请求预算。
SQL 执行也必须受有界等待/中断控制，不能只在 handler 入口检查 deadline。
响应保持在现有 transport/client 帧预算内；主数据、分组结果或最终响应超限时明确报错。
可选关联读取仍有单行/候选集约 4 MiB 预算；超过该预算按关联存储故障降级为空关联。正常查询不产生新的业务安全事件，避免查询污染统计；
沿用安全诊断日志且不记录敏感查询结果。

## 5. UDS UID 归属与存储扩展 [TARGET V2]

### 5.1 可信归属

`QueryScope` 是内部类型，不可从请求反序列化；由 daemon 从内核 peer UID 创建，贯穿
query service、存储端口和 repository。sec 的授权范围内过滤见 §4。授权不依赖 PolicyAdministrator，
不从 metadata、OTel Baggage、trace/session/run ID 或客户端转发信息推导归属。

| 数据 | 当前状态 | 必须补齐 |
|---|---|---|
| 安全事件 | 表有 uid，现有 ActionService 传递 peer 身份；EventFilters 无 UID 条件 | 确认写入来源可信，以 scope UID 强制 SQL uid 条件；后台事件不能套用查询者 UID |
| Observability | schema revision 2 含 nullable uid，自动升级 revision 1 | 已实现：handler/core/sink/writer 显式传递 peer UID，同次 INSERT 落盘 |

Observability 的 UID 是存储侧归属，不加入客户端 `obs.record` params，也不改变原有
record metrics/metadata 的业务含义。新记录必须有可信 UID，UID 与记录在同一次 SQLite
插入中落盘；不能先写记录再异步补 UID。JSONL 原有记录格式保持不变，恢复时不能从其中
自报的 session/metadata 推导 UID。安全事件与可观测记录关联时按记录 UID 匹配；root 全量读取也不能跨 UID 关联。

### 5.2 数据库初始化与升级

- Observability 数据库 schema revision 为 2。新库直接包含 `uid` 及 UID/time、
  UID/session/run/time 索引；已有 revision 1 数据库由 daemon writer 自动升级。
  复用 SQLite kernel 的 `extra_columns` 收敛，在同一事务内补列、建索引并更新版本；
  任一步失败均回滚，保留 revision 1 和原记录，故障排除后可重试。
  已带 `uid` 的 revision 1 数据库保留原值，不重复添加列。
- daemon 写入必须携带可信 UID。可空 `uid` 的未知归属记录仅 root 的全量查询
  可见，并显式返回 uid=null；不能 `COALESCE(uid, 0)` 使其归 root，也不能与已知 UID
  的安全事件关联。旧表新增 nullable `uid` 后，历史记录自然为 NULL，不回填为 0。
  此升级仅作用于 daemon 已配置的数据库，不发现或合并 V1 per-user 数据目录。
- 安全事件 uid 只有在来源被确认可信时才可用于隔离。混合来源、导入文件或不明写入者的
  数据必须经过管理员明确映射/隔离；不能仅凭记录中自报的 uid 就上线用户查询。
- 数据库初始化由 daemon 写入侧负责，query reader 只读且不创建或修改 schema。
  初始化失败时查询不可用，不得退回未过滤 reader 或 CLI 本地 fallback。
- 数据恢复需使用配套数据库备份；JSONL 不含可信 Observability UID，不能据此恢复 UID。
  回退到仅支持 revision 1 的 daemon 前，应停止写入并恢复升级前配套备份；
  不提供自动降级，不可仅修改 `user_version` 冒充旧结构。备份之后的新数据不在该回滚保证内。

## 6. daemon 组件与依赖

| 组件 | 位置 | 职责 |
|---|---|---|
| Query 方法与 DTO | asc-daemon-protocol | closed allowlist、严格参数和结果契约 |
| Query Handler | asc-daemon-handler | wire 校验、调用 core、错误投影，不直连 SQLite |
| QueryScope | asc-daemon-core | server-only UID 范围；本阶段无需扩展角色体系 |
| SecurityQueryService | asc-daemon-core | 安全事件 list/get/count_by/summary 用例 |
| ObservabilityQueryService | asc-daemon-core | session/run/timeline、组合授权范围内且同 UID 的安全事件计数 |
| 两类查询存储端口 | core 定义，daemon 注入适配器 | 每个操作必须接收 scope；不提供服务可调用的无 scope 旁路 |
| SQLite adapter/repository | 复用 asc-persistence-sqlite | 参数化 SQL、强制 UID、分页聚合、可区分的存储错误 |
| SecurityCorrelation 模块 | 查询应用层 | 复用候选查询并迁移 V1 匹配；依赖查询端口，不自行打开数据库 |
| composition root | asc-daemon | 解析与 writer 相同的系统路径，构造 reader/adapter/service 并注入 Dispatcher，drain 后关闭 |

数据流为 `CLI/TUI → UDS → Handler → core QueryService(QueryScope) → SQLite adapter → repository`。
Core 不依赖具体 SQLite crate。关联模块在两个已授权数据源之间工作，不能重新扩大范围。
现有 `ObservabilityService` 继续负责采集；查询服务与写入服务分别装配，共享一致的路径和归属规则。
Report 暂在 CLI 上聚合 daemon 数据，无额外 daemon report 组件；TUI 也只增加 RPC 数据源。
CLI 的查询调用使用普通函数/闭包，不另设 Transport trait 或 Client 包装。
查询 service 独占两个存储适配器；保留 core 查询端口以隔离 SQLite 依赖，以及贯穿 SQL 的取消控制。

## 7. 兼容分类与实施顺序

| 分类 | 内容 |
|---|---|
| [PRESERVE V1] | CLI 名称、过滤/输出方式，7 个查询方法名与业务结果含义，关联匹配规则，report/review 的完整用户功能，以及普通用户仅见自己的数据 |
| [TARGET V2] | V2 信封、daemon-only 读取、UDS UID 强制隔离、observability UID 持久化 |
| 有意差异 | 系统 daemon 下 root 默认可见全部；未归属历史行仅 root 可见；存储故障不伪装空结果；拒绝身份 override。普通用户自身数据可见性是兼容要求，不是功能差异 |
| 非本阶段 | 非 root 跨 UID 审计、独立 report RPC、自动历史导入、全局快照、独立查询进程/缓存 |

本次实施：可信 UID 写入与初始 schema → scope 强制查询端口 → 三个 obs RPC、关联 → report/review。
安全事件 RPC/CLI 由基线 PR #6745 提供，保留其既有 UID 隔离和查询实现。
本阶段 V2 查询文档仅维护在组件 README 与组件内设计文档，不新增仓库顶层用户指南或对其引用。

## 8. 完整验收矩阵

下表保留 obs 与 sec 的完整目标；本次已执行证据和未验证边界见 §9，不能将本表整体标为通过。

| ID | 可执行验收要求 |
|---|---|
| QRY-001 | 两个真实不同 UID 经 UDS 写入/查询；root 可见全部且可按 UID 缩小范围；非 root PolicyAdministrator 仍仅见自身；无需成为 PolicyAdministrator |
| QRY-002 | 覆盖全部 7 RPC、get、total、group、summary、latest、分页；普通用户任一输出不得泄露另一 UID；root 全量结果不得错误合并 UID |
| QRY-003 | 两用户使用相同 session/run/tool_call ID，列表、计数、report 与 timeline 仍隔离；UID 请求参数被拒绝；root 仅在重名时使用组合 session ID，普通用户不能借此前缀越权 |
| QRY-004 | 采集 UID 取 peer，不能取 daemon UID/Baggage；SQLite UID 与记录原子插入；写入失败不产生可查询的无主记录 |
| QRY-005 | 新库初始化 schema 2；revision 1 原子升级，失败回滚且可重试；重复初始化和重启保持归属；NULL UID 仅 root 可见且不能与已知 UID 关联；未知安全事件来源不被信任 |
| QRY-006 | V1 frozen fixtures 比较 CLI table/json/jsonl/count/count-by/summary、参数互斥、offset、时间边界与错误退出 |
| QRY-007 | session/run 列表及双库计数、稳定排序、分页；timeline offset 按 observability 行推进，不丢页/重复计数 |
| QRY-008 | V1 关联 fixtures：精确/回退、hook/category、时间边界、单条与批量等价；候选读取先按 UID 过滤 |
| QRY-009 | report 授权范围内 --last、text/json、LLM/工具/安全统计；root 使用列表返回的 session ID 下钻、重名 session 不混合；多页完整读取；TUI 下钻与非交互终端行为 |
| QRY-010 | daemon 不可用时 CLI 不读本地库/不启动 daemon；数据库权限/损坏错误不伪装无事件；查询不创建/迁移库 |
| QRY-011 | deadline、SQLite 等待/取消、超大 details/分组/timeline 响应有界；主数据/响应失败无部分成功 JSON或敏感诊断；可选关联故障不阻断页面 |
| DPROC-QRY-001 | 安装态 CLI 只能通过 daemon 读系统数据；真实跨 UID 与重启验收，源码进程测试不替代 systemd/RPM 验收 |

交付证据必须分别报告库内 fixtures、CLI/daemon 本地 E2E 和安装态跨 UID 验证，
并记录有意兼容差异、新库初始化及 revision 1 → 2 数据库升级结果。

可执行 fixture 映射（路径以组件为根；表中明确列出未覆盖范围）：

| ID | Fixture / 当前边界 |
|---|---|
| QRY-001 | `v2/apps/asc-cli/tests/observability_query.rs::uds_peer_owns_ingestion_and_cli_pages_reports_without_local_fallback` 验证当前真实 peer；不覆盖两个真实 UID 的安装态验证，环境限制见 §9.2 |
| QRY-002 | `v2/crates/asc-daemon-protocol/tests/pap_contract.rs::observability_query_method_inventory_matches_dispatch` 覆盖 obs 三方法；sec 四方法由基线 `v2/apps/asc-daemon/tests/sec_query_protocol.rs` 覆盖 |
| QRY-003 | `v2/crates/asc-persistence-sqlite/tests/owned_queries.rs::root_and_user_scopes_do_not_merge_colliding_sessions_or_correlations`；`v2/crates/asc-daemon-handler/src/observability_query.rs::tests::rejects_identity_overrides_and_invalid_filters_before_storage_access` |
| QRY-004 | 上述 CLI UDS fixture；`v2/crates/asc-event-sink/src/configured.rs::observability_tests::both_paths_receive_the_record`；`owned_queries.rs::owned_write_rejects_future_revision_without_inserting` |
| QRY-005 | `owned_queries.rs::initial_schema_is_revision_two_with_uid`、`legacy_schema_upgrade_preserves_unknown_rows_and_admits_owned_writes`、`upgrading_owned_revision_one_preserves_existing_owners`、`legacy_schema_upgrade_failure_rolls_back_and_can_be_retried`、`unknown_rows_stay_isolated_after_reopen`；覆盖本地 SQLite 升级，不替代安装态验收 |
| QRY-006 | `v2/apps/asc-cli/src/commands/observability/query.rs::compatibility_tests::report_matches_frozen_v1_json_and_text` 覆盖 obs report；sec 输出由基线 events CLI fixtures 覆盖 |
| QRY-007 | `owned_queries.rs::totals_and_half_open_time_windows_precede_pagination`、`exact_session_filter_preserves_scope_totals_and_time_window`；上述 CLI UDS fixture 覆盖多页 timeline |
| QRY-008 | `v2/crates/asc-daemon-core/src/query/correlation.rs::tests::frozen_v1_single_and_batch_matches_agree_with_core`；`owned_queries.rs::correlation_candidates_keep_v1_order_and_limit_without_failing_timeline`、`malformed_candidates_do_not_hide_valid_correlations_or_observations`、`candidate_storage_failures_do_not_fail_the_observation_page`；QRY-003 fixture 验证候选 UID 隔离 |
| QRY-009 | 上述 CLI UDS fixture（含 PTY 正常/信号退出）；`query.rs::tests::root_last_preserves_result_attribution_and_named_session_rejects_ambiguity`、`named_session_is_resolved_in_one_filtered_rpc` |
| QRY-010 | `owned_queries.rs::missing_corrupt_unready_and_malformed_stores_are_not_empty_successes`；上述 CLI UDS fixture 覆盖 daemon 不可用与无本地回退 |
| QRY-011 | `owned_queries.rs::live_cancellation_interrupts_sql_instead_of_only_checking_ingress`、`oversized_payloads_fail_without_partial_timeline_results`；`query.rs::tests::a_later_page_failure_emits_no_partial_report`；`owned_queries.rs::candidate_fault_tolerance_preserves_scope_and_deadline_errors`；不代表全部分组/传输边界已验收 |
| DPROC-QRY-001 | 源码 fixture：`v2/apps/asc-daemon/tests/bootstrap.rs::daemon_binds_when_optional_query_indexes_fail`（root 分支验证 admission/诊断）；不覆盖安装态跨 UID、systemd/RPM 与重启，环境限制见 §9.2 |

### 8.1 V1 功能对齐是完成条件

之前 QRY-009 仅写“TUI 下钻与非交互终端行为”，粒度不足以验证 V1 review 的完整功能。
以下要求补充而非替代 QRY-001～011；不得把实现简化作为默认豁免。
迁移应满足 V1 的全部用户功能；每个不满足项都必须记录 V1 行为、V2 实际行为、
用户影响、原因分类（架构约束、明确产品决策、实现遗漏或验证不足）、源码证据和补齐条件。
没有通过对照验证的项标为未验收；仅记录理由不等于允许省略功能。

| ID | V1 基线与验收要求 | 当前状态与原因 |
|---|---|---|
| PAR-001 | Session → Turn → Event → Detail；Enter 和鼠标选行下钻；返回保留父页面位置 | 已实现键盘、鼠标下钻及父页面状态保留；PTY 验证鼠标进入、Enter 下钻与逐级返回 |
| PAR-002 | Session 展示 Last seen、Session、Turns、Events；Turn 展示 Started、Run、Preview、Events | 已恢复全部字段；按用户要求不展示查询归属。PTY 检查两级时间列，保留 V1 预览与计数含义 |
| PAR-003 | Event 展示 Time、Hook、Call / Tool、Security Result；无匹配显示 `-`，多类匹配分别显示结果 | 已改为 observation 行，按 UID + observation ID 汇集关联摘要；单测覆盖 verdict/status/valid/result 投影及无匹配，PTY 验证 Security Result 与 prompt_scan:deny |
| PAR-004 | Observation 详情集中展示身份与时间、Metadata、Metrics、关联 Security Events；关联详情含 category、event_type、result、reason、delta、security_at 和 details | 已集中展示 Metadata、Metrics 和 Security Events；单测与 PTY 检查匹配原因、时间差、结果与详情，不混入其他 UID 或 observation |
| PAR-005 | 子页面 q/Esc 返回一级，Session 根页面返回退出；保持方向键、可滚动详情、空状态和非交互终端错误行为 | 已恢复 q/Esc 返回，入口退出；非 TTY 退出码为 2；PTY 验证逐层返回及 terminal 恢复，保留 V1 空状态文案 |
| PAR-006 | Review 时间按用户本地时区显示，详情同时有可追溯 UTC 时间；表格和长内容可在终端尺寸变化后阅读 | 已使用本地时间 + UTC、按窗口大小展示、详情换行和宽表水平滚动；PTY 在 Asia/Shanghai 检查时间与 resize，单测验证中文长内容换行 |
| PAR-007 | Correlation 保持精确与回退优先级、hook/category、每类结果选择、reason/rank/delta，安全结果投影保持 V1 verdict/status/valid/result 语义 | 28 个 V1 单条/批量冻结用例覆盖精确、回退、缺失 ID、类别过滤、每类选择、并列排序与 PII source/hash；保留同 UID 断言，并以 UI 测试验证结果投影 |
| PAR-008 | report 的 --last/--session-id、text/json 字段、LLM/字节/工具/安全计数、空数据、参数组合和退出行为逐项与 V1 对照 | 已恢复 V1 文本布局、数字分组、时长舍入及 --last 优先级；5 个 V1 生成的 JSON/text 用例、205 行分页、错误退出和无部分输出验证；有意差异见下文 |
| PAR-009 | observability schema 输出与 V1 schema 功能等价；record 的既有输入校验和采集契约保持兼容 | 已从 Rust 契约动态生成 schema；库测试和 CLI 测试均与完整 V1 fixture 对照，验证无有效 socket 仍成功、metadata 必填字段及 metrics 与采集一致。record 继续由原有采集 fixtures 验证 |
| PAR-010 | events CLI 与四个 sec RPC 保持 §3～4 的全部 V1 功能 | 由基线 PR #6745 提供；入口为 `v2/apps/asc-cli/src/commands/events.rs`、`events_summary.rs` 和 `v2/apps/asc-daemon/tests/sec_query_protocol.rs`；完整功能验收仍须逐项对照 |

V1 对照入口为 [CLI](../../agent-sec-cli/src/agent_sec_cli/observability/cli.py)、
[review](../../agent-sec-cli/src/agent_sec_cli/observability/review.py)、
[report](../../agent-sec-cli/src/agent_sec_cli/observability/session_report.py) 和
[correlation](../../agent-sec-cli/src/agent_sec_cli/observability/correlation.py)。
Review 验证必须检查列表内容、选中 observation 的关联详情和按键后的页面状态，
不能只检查进程成功退出。新增分页、刷新等能力不得替代已有功能。
可以更换 TUI 技术栈或视觉样式，但不能因此省略字段、导航和关联展示能力。

与 V1 的有意差异及理由：

- **架构与授权**：CLI 持久化查询全部经 daemon；root 全量查询时 JSON 新增 uid，
  同名 session 要求选定 UID，防止跨用户合并。本地生成 schema 不受 daemon-only 限制。
- **故障边界**：Timeline 的可选安全关联恢复 V1 容错：候选取前 1000 条、坏记录跳过、
  存储故障返回空关联，优先保证页面可用；此时空关联不代表完整读取或没有安全事件。
  Observability 主数据、安全计数和完整 report 的读取故障仍明确失败，不输出部分报告；
  损坏 metadata/metrics 也不会以原始片段掩盖存储错误。
- **系统资源限制**：V1 TUI 一次加载所有行；V2 按 100 条 observation 分页，关联项不计入
  页长，保留全部可访问记录并提供翻页。字体、颜色和控件样式随技术栈变化，用户功能不减少。
- **数值约束**：report 字节计数接受非负整数、可转换字符串、布尔值与截断小数；无效值、
  负数或超出 u64 的计数明确失败。Rust 有界计数不能复制 Python 无界整数行为，不允许静默归零或溢出。
  tool_name 缺失时保留 unknown；非字符串名称明确报错，避免把无效工具维度静默归入 unknown。

### 8.2 UID 隔离验收

所有展示与关联遵循 §1、§4.1、§5 的 UID 约束。QRY-003 使用同名 session/run/tool_call
验证 UID 隔离；QRY-001 与 DPROC-QRY-001 还要求真实不同 UID 的 UDS/安装态验证。
库内构造身份的测试不能替代内核身份接入验收；本轮验证范围与环境限制见 §9.2。

## 9. 本次 obs 交付及 sec 基线

### 9.1 已实现的契约

- `asc-daemon-core::query` 定义 `QueryScope`、实时取消/截止信号、错误类型、两个存储端口
  和 `ObservabilityQueryService`；obs handler 只做严格参数校验及安全错误投影。
  `QueryHandler` 统一装配 sec 与 obs 查询，由 dispatcher 通过 `with_queries` 一次注入。
  sec 保留原有存储端口和授权范围；两类查询独立绑定，单侧不可用不影响另一侧。
- obs reader 使用独立只读连接，每个操作在事务中统计和分页；不调用容错返回空结果的旧 reader。
  SQL progress handler 和关联循环检查同一个实时取消信号。各 RPC 沿用 5 秒 dispatch 预算；
  SQL busy wait 上限 200 ms。单行与候选集约 4 MiB，最终响应预留信封预算；
  候选按 V1 取前 1000 条，坏记录跳过，关联读取的存储/资源故障降级为空关联并记录诊断。
  主数据和最终响应超限仍报错。按 observation 分页，关联使用 V1 单条匹配规则。
- `obs.sessions.list` 的可选 `session_id` 在 SQL 分组、计数与分页之前精确匹配，
  不改变 UID/时间过滤。CLI 指定 session 时只发一次 session 查询（limit=2），
  root 原始同名 session 的 total > 1 仍报歧义，应使用列表返回的组合 ID；不扫描无关 session。
- `obs.sessions.list` 增加 `security_by_category_result`：同 UID/session/时间窗口下六类
  report 安全事件的 category × result 计数。`security_event_count` 则包含全部类别。
  report 只需三个 obs RPC 即可完成完整统计，不依赖 sec RPC；timeline 的关联项
  不能替代这项聚合。未知 UID 的聚合为空，并展示无法归属安全事件的提示。
- TUI 使用分页交互而非一次加载全部数据；支持方向键/j/k、Enter 下钻、b 返回、n/p 翻页、
  r 刷新、q/Esc 逐级返回和 Ctrl-C 全局退出。恢复 V1 表格字段与集成安全详情，
  支持鼠标、窗口缩放、本地时间和 Unicode 换行；非交互终端退出码为 2，退出恢复 terminal 属性。
  外部 SIGINT/SIGTERM/SIGHUP 由原子标记通知 UI，正常退出路径恢复 raw mode、光标、
  alternate screen 与鼠标捕获后返回 130/143/129；键盘 Ctrl-C 保持原有退出行为。
  空闲事件等待每 100 ms 检查信号；正在执行的同步 RPC 仍受原有客户端超时限制。
  SIGKILL 无法捕获，不属于终端清理保证。
  report 与 V1 一致，仅支持 --session-id、--last、--format text/json；逐页聚合，
  全部成功后才输出；不同工具名称超过 10,000 个时要求缩小时间范围，不输出部分统计。
- 系统 obs writer 使用 schema revision 2，新库直接包含 `uid` 和归属索引；
  revision 1 通过通用 `extra_columns` 在事务内补列、建索引并更新版本，不新增迁移回调。
  历史记录保留 NULL UID，已有 UID 保留原值；不执行 `owner_uid` 列重命名。
  测试覆盖首次建库、重复初始化、升级后写入、失败回滚与重试、重启后的 UID 保留
  及未知归属隔离；V1 oracle facade 不作为 daemon 查询旁路。
  security row schema 保持 revision 3，daemon writer 额外创建 UID/time、UID/session/run/time
  和 UID/session/run/tool/category 复合索引，query reader 不创建索引。
  必要 schema 初始化失败仍阻止 daemon 启动；可选 security 查询索引准备失败仅记录诊断，
  不阻止 admission。建索引仍同步发生在 READY 前，大库耗时尚未测量，不宣称已有启动时限保证。
  daemon sink 仅暴露带 UID 的 `record_owned`；V1 无 UID facade 只保留在独立持久化兼容层。
  写入时 schema 检查和 INSERT 使用同一次连接获取，仍拒绝不支持的 revision。
- 启动时准备 obs schema，失败则 obs query 返回 unavailable；JSONL 保持惰性创建。
  UID 写入失败绝不产生一条可查询的无 UID 新记录；JSONL 先写成功后 SQLite 失败仍可能
  留下一条 JSONL，沿用采集错误契约且不自动重试。

### 9.2 验证方式与已测范围

逐项 fixture 入口见 §8。`v1-correlation.json` 保留 28 个 V1 单条/批量关联用例，
公共 record/event 默认字段与各场景覆盖字段共同构成输入；Rust 测试额外注入另一 UID 的候选。
这些 JSON 样例保存 V1 的预期结果，由 Rust 测试直接读取，不依赖 Python 或手动生成步骤。

CLI/UDS/PTY fixture 验证 205 条记录的分页、报告与详情、鼠标/键盘、窗口缩放、
非 UTC 时区、空状态、错误退出与信号后的终端恢复。视觉样式不要求与 Textual 像素一致。

在 `src/agent-sec-core/v2` 下运行受影响组件验证：

```bash
cargo test -p asc-daemon-core -p asc-daemon-handler -p asc-daemon-protocol -p asc-persistence-sqlite -p asc-event-sink -p asc-observability -p asc-cli -p asc-daemon
cargo clippy -p asc-daemon-core -p asc-daemon-handler -p asc-daemon-protocol -p asc-persistence-sqlite -p asc-event-sink -p asc-observability -p asc-cli -p asc-daemon --all-targets -- -D warnings
cargo fmt --all -- --check
```

2026-10-10 在提交 `34433a3d9` 前执行的本地 Rust workspace 检查为 1461 passed、0 failed、3 ignored；
fmt、Clippy 通过，rustdoc 构建成功，存在一条无关文件的既有私有链接警告。

同日 AgentSight 联调以启动时基线 `2903e03bc3c70427b3e915f8df7e0a6ba8c2d523` 的
运行中二进制为对象，62 项接口检查、24 项浏览器检查通过，覆盖 root AgentSight → UDS →
V2 daemon 的 sec/obs 查询与页面展示。数据为真实数据库快照（22 条安全事件、8 条 obs，
均属 UID 1000）；仅在快照中将 obs schema 版本标记从 3 调整为 2，未改原始数据库。
该结果不覆盖后续源码变动、真实多 UID 隔离、同名 session 碰撞、生产认证或原库版本迁移。
详细结果见 [AgentSight 与 AgentSecCore V2 联调测试报告](https://alidocs.dingtalk.com/i/nodes/QOG9lyrgJPPNL2rXIl4d9d7OJzN67Mw4)。

本轮安装态环境检查：宿主机运行 systemd 255，但当前账号执行 `sudo -n true` 返回需要密码，
`rpm -q agent-sec-core` 返回未安装；现有 root 调试容器的 PID 1 为 python3。
因此本轮未执行安装态跨 UID、RPM/systemd、升级/回滚与服务重启验证；
已有源码测试和容器联调结果不等同于上述部署路径通过。
sec PR #6745 已作为本分支基线提供四个 sec RPC 和 events CLI；
仍需结合 obs 与 sec 的功能、部署验证联合评估 #6608，本次 rebase 不声明完整关闭该 issue。

### 查询身份与会话展示

查询授权范围由 UDS peer UID 决定。sec 使用原始 session ID 查询，root 的同名返回标签
仅用于展示归属；obs 的组合 ID 下钻保持不变。详见 daemon 协议第 6 节。

SQLite 列和响应中的数据归属字段仍为 `uid`。系统 obs schema 为 revision 2，
自动升级 revision 1；缺失 UID 的历史记录保留 NULL，不回填为 0。JSONL 格式不携带存储侧 UID。
Review 继续省略页面标题、查询归属和操作提示；查询通过 session_id 下钻。

新增/调整的可执行证据：`owned_queries.rs::root_qualified_sessions_preserve_owner_isolation`、
`unique_sessions_keep_their_ids_and_ambiguous_qualified_names_fail` 验证组合名称与实际 SQL 归属；
`asc-daemon-protocol/src/query.rs::tests::ownership_parameters_are_rejected_even_for_root`
及 CLI UDS/PTY fixture 验证服务端范围隔离与下钻；report JSON 直接与 V1 完整比较，不能先删除额外字段再比。源码测试不替代安装态跨 UID 验收。
