# 可信证据与有界上下文

[English](../../en/runtime/evidence-context.md)

## 范围与依赖

本文对应 [#7622](https://github.com/agentic-os-org/ANOLISA/issues/7622)，
依赖 [#7626](https://github.com/agentic-os-org/ANOLISA/pull/7626) 提供的只读连接器；
#7626 实现 #7621，现已合入。本阶段基于主线继续实现，并保留
[#7411](https://github.com/agentic-os-org/ANOLISA/pull/7411) 合入后的主线，
其合并提交为 `d217dd6bbab927a9820202de5944adf3d9e4a5d7`。

服务为活动事件提供显式 `freeze_context` RPC，将原始候选、当前 AW 约束和选取的
历史固定为一个不可变版本。`aw run` 不会自动发送该请求。本次不引入模型、语义
评估、动作路由或新的 Provider operation。覆盖准入由 #7623 负责，公开增量审计
导出由 #7624 负责，AgentSight 消费由 #7625 负责。语义评估仍属于二期。

## 自动运行观测

`aw run` 在绑定后、启动 Agent 前开启可选观测。`start_observation` 按配置超时探测
查询兼容性，上限一秒。`report_run` 记录真实启动/退出或启动失败，PID 仍只是 launcher
报告的数据。`status.observations` 只展示元数据。

事件没有已准入动作时，原生注册包含内部 `$aw.observe` 回调。`observe_hook_event`
核对原始输入，不创建 Host 动作事件，并拒绝用这条路径代替已配置动作步骤。已有
动作回调在事件首次打开时安排一次观测。两条路径的应答均不等待外部 HTTP，保留
#7411 的调度。

每个实例持有一个 worker、两个候选的队列和一个有界内存投影，每实例最多接收
256 个观测回调。输入超限、队列溢出、worker/来源故障均报告缺口。自动读取受一秒
及原始回调截止时间约束，选取内容遵守配置条目/字节上限。观测投影不冻结或修改
活动事件，显式 `freeze_context` 保留原语义。

`release_instance` 和 `unbind` 停止接收任务，等待有界观测完成后，仅释放当前实例
的来源关联和缓存。服务退出也等待 worker。清理包含进行中的启动探测，避免中断后
留下晚启动 worker。launcher 最多等待五秒释放并报告清理错误。被强杀的 launcher
可能留下未完成状态，直到服务退出；自动失联 owner 回收尚未实现。

`evidence.mode` 接受默认的 `auto` 与 `off`。可选来源失败保留 AW 动作和审计，off
不读取来源或增加注册。强制能力准入仍由 #7623 实现，当前拒绝未知 mode。查询探测
仅检查 auth-status 响应合同，存储、采集与 eBPF 就绪仍为 unknown；实际轨迹读取再
校验 ATIF。两种传输均不请求 `/health`，不按产品版本准入；来源产品版本始终为
null，并原样进入证据投影与 canonical 摘要。后续审计及语义合同参见[生命周期设计](../integrations/agentsight/agentsight-integration.md)。

## 为二期语义评估提供的基础

后续评估阶段可以将输入绑定到 AW 已准入主体、原始候选及约束，再引用
`aw.evidence/v1` 证据和单个固定的 `aw.evidence-context/v1` 版本。快照记录投影
版本、截止点、限额、来源和缺口，区分原生结果、来源历史和模型回显。重复读取
不能替换候选或延长 deadline；仅含元数据的 Journal 回执让后续决定可关联到准确
的输入版本，而不持久保存上下文正文。

这是输入基础设施，不是语义服务接口。当前 `freeze_context` 要求活动动作事件；
自动观测不会创建此类事件或调用评估。二期仍须单独准入评估阶段、材料要求、
类型化返回合同和动作路由。必需材料缺失或过期时，遵循对应策略的不可用输入行为；
可选 Sight 历史不能成为每次决策的隐式前提。

默认用户态轨迹扫描在两轮之间等待 30 秒。GET 只读取已入库快照，不触发采集，
立即再读一次也不能保证新鲜数据。本投影仍将来源时间标记为未验证。Hook 不等待
下一轮扫描，晚到记录不能改写已固定上下文或已完成决定；及时输入与本地审计仍
以 AW 原生回调为基础。参见[二期合同](../integrations/agentsight/agentsight-integration.md#为二期预留的语义评估合同)。

## 组件归属与信任边界

| 组件 | 职责 |
| --- | --- |
| 可信的同用户 launcher / Adapter | 提供已准入能力与原生回调原始字节 |
| `aw-service` | 签发 generation、绑定实例与事件身份；限制来源；核对原生 ID；每个活动事件只固定并在内存中保留一个有界上下文 |
| `aw-contracts` | 提供离线 `aw.evidence/v1`、`aw.evidence-context/v1` Schema、canonical 摘要与一致性校验 |
| AgentSight 连接器 | 按公共 auth-status / ATIF-v1.7 合同和选定传输认证模式，发送有界 GET 请求 |
| Host / 原生 Adapter | 保留现有准入、调度、替换、取消与采用行为 |
| Journal | 同步提交上下文版本、证据 ID 和缺口，不保存上下文正文 |

私有同用户 socket 仍是现有信任边界。这些检查不能隔离同 UID 下的恶意程序，也不能
认证远程 Agent 进程。Loopback HTTP 不读凭据、不发 Authorization header，记录
`loopback_unauthenticated`；HTTPS 使用私有 token 文件和正常 TLS 校验，
`bearer_supplied` 只表示已发送，不证明服务端实施了认证。两者都不验证 AW 身份。即使会话关联已经核对，来源内容仍是
分析数据，不能改变 AW 策略，也不能授权动作。

## 身份关联

Generation 和配置版本取自当前服务；binding ID 取自准备好的 Host target；instance
ID 由服务准入签发；event ID 来自事件注册表。调用方必须提供事件所属实例，以及
事件句柄返回的 `candidate_revision`。其他实例、旧 generation、过期候选版本、
已关闭事件和已超时事件都会被拒绝。

`open_hook_event` 与纯观测 `observe_hook_event` 均核对原始回调中的身份。证据检查比较解码后的
原始字节与保留的 native 值、会话及调用 ID、事件入口、工具名称、输入和原始结果。
比较方式由已准入 Host 的 Adapter 决定。普通 `open_event` 携带的 ID 保持
unverified；没有 ID 时保持 unassociated。原有 Hook 准入仍要求非空会话/调用 ID，
证据合同允许 null 不会放宽该要求。

Qoder、QwenPaw 保留原生调用 ID。Hermes 沿用 API request ID 与 tool-call ID
的摘要及 `hermes:` 前缀。OpenClaw 沿用 run ID 与 tool-call ID 的摘要。
另一次原生运行中复用的模型调用 ID，不能匹配原回调。本次不使用 Core
runtime-binding 的会话主张，因此不依赖
[#4372](https://github.com/agentic-os-org/ANOLISA/issues/4372) 中尚未校验的省略情形；
该问题仍单独处理。

每个配置 target 指定来源 origin；仅 HTTPS 要求绝对路径的私有 token 文件。
HTTP 可省略该字段，提供时也不读取。RPC 不接受 endpoint 或来源会话选择器。
查询只从已核对的原生回调取得 session，返回的根 session 必须精确匹配。
框架名称采用以下明确合同：

| AW Adapter | 接受的 `agent.name` |
| --- | --- |
| `qoder` | `qoder`、`Qoder` |
| `hermes` | `hermes`、`Hermes` |
| `openclaw` | `openclaw`、`OpenClaw` |
| `qwenpaw` | `qwenpaw` |

Qoder 系统采集器报告小写 ID，本地 discovery 报告显示名。Hermes/OpenClaw 显示名
与 AgentSight 默认配置一致，但不由此证明这些框架的轨迹采集已就绪。其他名称，
包括 `QoderWork`、`QwenCode` 和 `AgentScope`，不推断为别名。自定义来源须同时
满足名称合同与原生 session 匹配。名称匹配不建立原生绑定，也不证明进程身份。

对于同一规范化 origin、Adapter 和原生会话，首次查询的 AW 实例持有关联，直到该实例的
事件与观测 worker 完成收尾。其他活动实例即使来自另一个配置 target，也只得到
`source_session_conflict`，不返回历史。服务最多保留 1024 个活动映射，不提前淘汰。
释放时只移除已收尾 owner 的关联；后续恢复实例可读取旧历史，不把它计为新执行。
服务重启会清空映射，并使所有旧句柄失效。操作者仍须负责选择获授权的来源与范围。

当前 API 不提供已核实的 AgentSight 运行实例，或 host/boot/进程启动身份。
来源实例保持 null，进程关联不可用。v1 Schema 不开放进程关联 basis：只有 PID，
或 PID 加上自述元数据，都不能产生 verified 进程断言。后续增加该关联方式，必须
先核对来源合同，具备 host、boot 和进程启动身份；PID/名称相等不足以关联，PID
复用时也一样。本次通过 `process_identity_unavailable` 明确报告该限制。

## 可执行证据合同

[`evidence-v1.schema.json`](../../../../crates/aw-contracts/schemas/evidence-v1.schema.json) 定义正式结构，
拒绝未知字段；元数据遵循 AW canonical 域。每条记录包含：

| 字段 | 含义 |
| --- | --- |
| `schema_version`、`evidence_id` | `aw.evidence/v1`；`[source, content_ref]` 的摘要 |
| `subject` | 服务 generation、精确配置版本、target binding、实例、事件及已准入 Adapter |
| `source` | 组件、来源范围、可空运行实例及产品版本、记录引用与版本 |
| `association` | `verified`、`unverified` 或 `unassociated`；依据为 `native_callback`、`native_session` 或 `none`；原生 ID 可空 |
| `times` | 可空来源发生时间字符串、本地接收时间和快照截止点，后两者为 Unix 毫秒整数 |
| `kind`、`content_ref` | `native.callback`、`native.tool_result` 或 `trajectory.step`；编码内容的 SHA-256 引用 |
| `coverage` | 显式 `unknown` 与有界原因码，不宣称完整覆盖 |
| `trust`、`derivation` | 可信 Adapter 报告、未核实原生数据或来源自述历史；复制上下文保留为模型回显 |
| `execution_ref` | 只有 verified 原生结果能声明执行身份，其他情况为 null |

即使 `execution_ref` 为 null，也必须校验来源组合。两类原生记录均要求来源为
`aw`、派生类型为 `native_report`。verified 原生记录要求 `trusted_adapter`、
`native_callback` 关联及 tool call ID；unverified 或 unassociated 原生记录保持
`unverified_native`，关联依据为 `none`。轨迹步骤要求来源为 `agentsight`、信任等级为
`source_reported`、派生类型为 `source_history` 或 `model_echo`，且不声明 tool call
或执行身份。verified 轨迹关联使用 `native_session`，不能使用 `native_callback`。
这些规则检查一致性；schema 校验和内容摘要均不能认证来源身份。

AW 来源范围为服务 generation。AgentSight 来源范围为配置
`[source_id, normalized_origin]` 的 canonical 摘要；运行实例未知。历史记录引用
包含选定会话和根轨迹步骤下标。来源版本为完整解码轨迹重新序列化成紧凑 JSON 后的
摘要，是投影摘要，不声称等于原始 wire 字节或来源数据库版本。发生时间字符串
最多保留 128 个 UTF-8 字节，其时钟、格式和新鲜度均未核实。接收时间与截止点不能
证明动作发生时间，因此历史内容不能确立新近执行事实。

执行身份取 `[generation, binding_id, instance_id, session_id, tool_call_id]`
的摘要。Verified after 回调贡献一个执行身份；before 回调不贡献。ATIF 步骤即使
调用 ID 或结果文本相同，也不贡献执行次数。重复回调打开同一事件，快照提供去重的
执行引用。历史记录保留各自来源引用，不按文本或模型 ID 合并。
`is_copied_context: true` 标为 `model_echo`；未标记历史也只是 `source_history`，
不能单独证明又执行了一次工具或累计了一次失败。

## 快照选取与生命周期

[`evidence-context-v1.schema.json`](../../../../crates/aw-contracts/schemas/evidence-context-v1.schema.json)
定义结果结构，包括主体、原始候选版本、约束内容引用、选取证据、内容对象、来源
状态、缺口、限额、截止点，以及用十进制字符串表示的原始单调时钟 deadline。

候选内容包含原始事件名与 tool 对象。约束保留配置中的完整 `execution`、`events`
（包括 guard 和错误动作），以及完整配置版本。Provider 命令、私有设置、回调环境
和凭据不复制到上下文；配置身份固定这些设置，不暴露值。投影版本为
`aw.evidence-projection/v1`，脱敏版本为 `selected-fields-no-redaction/v1`。
选中的工具/历史内容没有自动脱敏。

内容对象包含 `encoding: json-utf8`、精确 `text`，以及该 UTF-8 文本的摘要。
任意 JSON 数字与 Unicode 键保留在字符串中，不直接放进 canonical 元数据。
`context_revision` 对完整结果去掉自身字段后取 canonical 摘要。候选、约束、
选取内容、来源版本、限额、截止点或投影变化，都会改变摘要。离线校验器检查
预期主体/候选、内容摘要和引用、证据 ID、限额与执行身份。它校验一致性，不认证
来源；消费方必须从可信服务取得快照。

选取始终完整保留候选与约束。历史选取条目上限内最新的根轨迹步骤，保持来源顺序；
原生记录占一个条目。嵌入的 subagent 不纳入，并报告缺口。某条历史会使完整序列化
结果超过字节限制时，省略该条并标记 `content_byte_limit`。如果仅候选与约束就
无法容纳，显式失败，不摘要、不静默裁剪。

每个事件只允许尝试固定一次。并发请求返回 `context_busy`；成功后，在事件存活
期间重复请求得到完全相同的快照。失败尝试不能重新获取预算。HTTP 读取不持有
事件锁或 Journal 锁，截止时间取原始 Host deadline、请求 deadline 和查询超时
的最小值。较短的查询超时在事件尚有预算时返回显式缺口；事件超时或关闭则禁止
发布。阻塞传输可能在其已准入 deadline 才结束，晚到结果会被丢弃；关闭事件不用
等待该传输。

晚到来源变更不能重写已有上下文。事件关闭后释放完整内容，旧句柄不能读取或重建
快照。后续分析需要另行显式准入新上下文/事件，不恢复旧事件，也不重放工具。
本次没有后台缓存或内容存储。没有待完成原生步骤的事件可能在请求上下文之前结束；
证据访问不延长其生命周期。

## 审计、兼容与无 eBPF 行为

返回快照前，服务同步追加私有 `phase: context_frozen` 记录，包含候选/上下文/
来源版本、证据 ID、投影版本和缺口，不持久保存候选、历史、凭据或提示正文。
本地审计失败仍在现有服务边界保持致命错误。这不是 #7624 计划提供的公开
`aw.audit/v1` 导出。

本地 RPC 升为 `aw-service/v1alpha3`：事件句柄增加 `candidate_revision`，新增
`freeze_context`。Client、callback 二进制和 daemon 必须一起升级，旧 daemon
由其 owner 重启。Provider stdio 仍为 `aw-provider/v1alpha1`。未配置
`agents.<target>.evidence` 的旧配置继续有效。来源配置、限额和回退操作见
[用户参考](../../../../../../docs/user-guide/zh/aw/observability.md)。

#7411 最终版本中依据可信 Host 能力决定的 Qoder 顺序保持不变。Hermes 最终入口和
OpenClaw replacement/completion 视图保留原始事件身份、deadline 与候选。
后续 completion 视图不改变已固定上下文。本次不修改 Adapter 调度或结果采用。

本次不引入 eBPF 探针、进程发现、特权 worker、安装包或 AgentSight 生命周期操作。
无 eBPF 时，可用的用户态轨迹仍能补充 AW 候选。服务/存储缺失、认证/版本失败和
来源限额都产生显式缺口；AW Hook 和本地审计保持既有行为。合法空轨迹为
`available`，覆盖仍未知；404 为 `unknown`，不能证明没有活动。上文的最小 `auto`/`off` 行为已实现；完整分能力 `required` 准入及 all-in-one 打包
仍属后续工作。

## 验证

HTTP/服务 fixture 覆盖主体隔离、伪造/缺失原生身份、PID 复用不产生进程关联、
来源错配/缺失、重复回显、晚到记录、旧主体/候选、重启、条目/字节/deadline 限制、
并发固定/关闭、审计 claim 失效，以及 OpenClaw completion 兼容性。四种原生
身份格式均有信任与调用范围测试。完整 AW gate 另行覆盖 #7411 的原生排序和替换
测试；这些验证不认证线上 AgentSight 部署、来源签发身份或 eBPF 覆盖。
