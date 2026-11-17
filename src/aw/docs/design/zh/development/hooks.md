# AW Hook 与 Source 开发规则

[English](../../en/development/hooks.md)

这些规则由 [AGENTS.md](../../../../AGENTS.md) 按任务索引；修改前必须阅读适用章节。

## 语义、效果和证据

沿用配置 schema 中的 16 个名称：`session.start`、`input.submit`、`tool.before`、
`tool.after`、`permission.request`、`compact.before`、`compact.after`、
`subagent.start`、`subagent.stop`、`turn.stop`、`session.end`、`model.before_request`、
`runtime.observed`、`runtime.exited`、`security.violation`、`coverage.changed`。
名称、效果或负载变化需要兼容且版本化的合同；识别名称本身不代表能力可执行。

支持声明同时记录事件阶段、效果、目标范围、入口、运行时 profile 与采纳保证。
区分观测输入、实际执行输入、回调结果和最终交付结果。before 检查只覆盖实际检查的
输入；更强保证需要验证后续转换及最终副作用边界。结果检查只覆盖已验证字段与交付
路径，包括 profile 宣称的流式路径。保留取消和期限，`withhold_result` 等故障动作
与 Provider 效果分开。

保留现有 `security.violation` 最终结果检查语义：支持的 gate 将候选结果交给策略
判断，允许结果也经过该入口。确认违规与成功采纳阻断分别记录。审计生命周期事实不
自动建立公共 Provider Hook。原生 Session、模型 Turn、请求结束、任务排空和进程
退出分别需要身份及完成证据；框架事件对照归双语 Adapter 设计与原生测试。

## 生命周期绑定

共享生命周期要求覆盖实例／任务身份、准入、任务内固定策略修订、Provider 执行、
审计和有界清理。保留[Service 登记合同](../runtime/local-service.md)及
[原生启动流程](../architecture.md#业务自管原生启动)中已有的两种绑定：

- 普通 `Register` 工厂，包括 SDK 工厂和 QwenPaw，确认根实例 Ready 后，通过已确认的
  child Scope 执行各任务；原生清理完成前保留该 Scope 的路由快照和策略修订。
- Qoder、OpenClaw 和 Hermes 经准入的 `RegisterNative` 桥接在 Ready 后直接使用
  native root 执行。完整原生 request/tool 占用持续到清理完成，期间阻止策略更新。
  保留 owner 鉴权和回调 peer 校验；native root 无法打开 SDK child Scope。

后续统一生命周期表达时，须明确原生 request/task 身份、准入、更新占用和清理到
Scope 的映射，并在改变任一绑定前提供迁移方案和回归覆盖。

## Source 选择与生命周期

统一 Source 接口和每任务 Resolved Hook Plan 属开发目标。当前 Adapter 提供准备后的
能力和原生接线；引入共享抽象时保留现有可执行合同。计划中的 SPI、解析器、wire 字段
和 CLI 不得写成已可用功能。

新增解析按语义 Hook、效果和保证计算需求，优先选择满足要求的原生 Hook，以适合
实际运行时且已验证的插桩补缺。一个实例可组合多种来源。Python 可复用 monitoring 或
wrapper 机制，dowhen 为可选；Node/Bun 复用共享 module-patch 和 request-gate。
ptrace/BPF 观测保持已验证范围，运行时拦截本身不证明 Agent 语义关联。

Source 合同描述探测、安装、验证、激活、卸载，以及运行时／代码身份、阶段、字段映射、
效果采纳、范围、成本、失败和取消。公共原生 API 需要兼容声明及验收证据；私有代码
补丁额外验证目标指纹与接点。通用 SDK 传输不继承私有补丁的精确版本限制，核心导入
不强制加载可选插桩依赖。

每个逻辑事件／阶段及准入策略步骤选择一个派发 owner，包括纯观测步骤。执行控制
边界最多一个控制 owner，辅助来源补充证据，不重复派发同一步骤。关联经核实的
代际、Instance、task/request 与 invocation 身份，采用 Scope 的生命周期绑定同时
关联 Scope 身份；保留来源自身身份与明确的关联缺口。
不按近似时间或工具名称去重。复用 Service ledger，保留未知结果，不重试不确定的
Provider 效果或重放工具来承诺无条件恰好一次完成。

受保护工作准入前解析并验证来源。在目标模型中，每个任务固定来源分配和策略版本。
来源重配置在安全边界重新探测、安装和验证；普通策略更新不授权热补丁。分别确认
App Ready、已安装、已验证、AW Ready 和实际采纳。attach 不将已在途工作标记成
受保护。适用且启用的事件，以及适用且启用的步骤所请求的效果或失败动作缺少能力支持时，
无论 `required` 取值如何都拒绝准入；启用且没有步骤的事件仍须得到支持。只有不承载任何配置要求的
额外辅助观察 Source 可以降级并显式报告。显式原生回退管理 Service 可用性，不能
放宽静态能力准入；须与策略兼容并保留拒绝、Provider 错误和取消。
来源失联沿已准入故障合同处理，任务内不换 owner。
机制无法撤除时，卸载可能需要重启。

纯需求解析归共享准入，进程探测和安装归 Adapter/Source。Service 管理 Instance、
Ready、Scope 激活、预算和审计，Host/executor 保留 Provider 执行。
Resolved Hook Plan 表达接入绑定，与独立 `Core::prepare/execute` 计划 API 分开。
新增 Agent 不向 Service、Core 或 Host 引入框架专属代码。

## 输入替换与原生授权

安全拒绝和原生权限拒绝优先。经准入的 `replace_input` 是首个适用的 `tool.before`
步骤，只声明该效果，并要求 before 步骤失败关闭。Host 绑定候选输入，不刷新身份、
claims 或预算；Service 对候选输入执行后续 Provider 和原生 Hook，只在最终回调
成功时发布。Adapter 在同一调用内采用输入并保留原生授权。旧 Tokenless `block`
建议仍为显式重试模式，输入替换不产生原生批准。

## 新增 Agent 或调整支持

新集成在三种一等入口内扩展现有合同，同一接入模式不得新增平行 launcher。
启动、检查和清理沿用该入口的既有流程。

统一 Hook、受管 MCP 和策略更新是共享 AW 能力。配置、准入、修订、生命周期所有权与
审计归公共框架，Adapter 提供原生边界和效果采纳。框架可按不同顺序交付能力；原生
边界缺失时在准入中显式声明不支持。

支持变化同一 PR 更新双语[能力参考](../../../../../../docs/user-guide/zh/aw/capabilities.md)，
记录 Adapter/Source、运行时 profile、入口、执行边界、限制与可运行验收证据。
设计决定、实现、合同测试、原生采纳验证、发行产物分别表述。静态网格覆盖默认 profile，
有条件的准备后能力另需 profile 专项测试与文档；探测成功不扩大默认支持，也不证明
原生采纳。README 与能力页展示生成网格，后者补充 MCP 和更新摘要，Agent 页面保留
具体接线说明。

策略更新复用候选准备、修订校验、持久审计和原子激活。原生请求／Scope 准入与激活
共享门控；准备中、未确认交接和未完成清理都算占用。任务的 before/after 与 MCP
检查使用同一策略修订。派发前确认原生路由采纳；更新拒绝保留旧策略，不确定清理保持
失败关闭。说明各 Adapter 已验 Ready、入口和切换边界，不从 PID 或配置监听推断 reload。
外部 PID attach、内部进程身份和更新目标选择分开。预热 worker 在首任务前确认 Ready。
控制面可为更新暂停派发，运行时 busy gate 保留；活动任务切换需要显式合同。激活后
审计失败停止派发，不能报告仍使用未变更的旧策略。当前支持归能力参考，开发顺序归设计。

在同一变更中共同更新以下各项：

1. 在 `crates/aw-adapter/src/adapter/catalog.rs` 登记原生方言和默认事件／效果 profile，
   构造准备后能力时复用。条件 profile 的新增和限制显式测试。共享来源尽量复用现有
   方言；新公共语义入口需要经准入、版本化的合同，配置 schema 同步。
2. 在 Adapter/Source 实现归一化、登记、效果采纳和卸载。启动的子进程归 `aw-exec`，
   应用自有登记与 Scope 归 Service，各入口沿原调用方编排，复用准入和回调 helper。
3. 每个事件／效果及生命周期、安全、Tokenless 组合在 `tests/regression-matrix.json`
   引用精确可运行 case；新增原生负载扩展 `crates/aw-cli/tests/regression/native.rs`。
   共享 PreTool/PostTool 测试遍历矩阵且无兜底方言，未实现夹具必须失败。
4. 从 `src/aw` 执行 `python3 scripts/check_matrix.py --write-docs`，在双语能力参考
   说明采纳限制，并同时生成两个 README 网格。
5. 在 `tests/native-matrix.json` 加入固定版本原生命令、报告、owner 和精确验收边界。
   单元 suite 声明计数，定制 suite 声明 JSON 结果与精确 case 名或观测计数；执行及
   产物汇总都验证报告。每个已登记 Agent 都必须纳入夜间验收。
6. 从 `src/aw` 执行 `python3 scripts/check.py`。门禁核对编译目录、矩阵和 schema，
   通过真实 libtest 可执行文件解析 case，拒绝缺失／忽略测试，并检查生成网格。

相关推送使用确定性的 Service/CLI/Provider 夹具。真实 SDK/CLI 采纳检查固定版本并明确
实际原生接点；传输测试不代表采纳验收。不得以忽略、opt-in 或需凭据测试替代必需矩阵
case。包验收另覆盖真实 Tokenless 二进制。
