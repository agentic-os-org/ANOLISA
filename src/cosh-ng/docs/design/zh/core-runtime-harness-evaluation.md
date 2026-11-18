# Cosh AgentRuntime 评估依据与决策记录

[English](../en/core-runtime-harness-evaluation.md)

日期：2026-11-14。状态：设计研究结论；未实现、未通过运行或发布验收。

配套：[架构设计](core-runtime-harness-design.md)、[分阶段计划](core-runtime-harness-implementation-plan.md)。

## 1. 核心判断与必要性

Cosh 需要的是适合 Linux Shell 的小型 Rust agent runtime，不是另一套 Coding CLI。推荐保留已有 provider/tool/Hook/Skills/MCP/session/compaction/audit，通过同进程官方 ACP 前端连接 Shell；只为当前持久 PTY 增加可协商的 host-shell 操作。内部整理限于宿主交互、事实投影、run 内取消和收尾，不引入第二个执行循环或通用运行平台。

ACP 标准化的是客户端与 Agent 的交互，不规定内部循环、压缩、数据库或沙箱的唯一实现。不能将“标准 AgentRuntime”误解成必须替换为某个框架；采用官方协议实现、明确能力与执行责任、用跨实现互通证明边界，才是这里可验证的标准化。

| 改造 | 是否必要 | 不做的实际影响 | 采用条件 |
|---|---|---|---|
| 具体结果配对、最终动作校验、取消缺口 | 复现后应优先修 | 可能出现不完整历史、假成功或审批绑定失真 | 每项独立夹具 FAIL→PASS，不以静态推断代替复现 |
| ACP Shell/Core 边界 | 对用户明确的双向替换目标必要 | 私有耦合继续存在，接入不同客户端/Agent 要重做适配 | 官方 SDK 接线、基线符合性、前台保真和分级真实互通 |
| 最小宿主交互接口 | 接 ACP 所需，非全量重构理由 | 直接 stdin 等待与异步双向调用冲突 | 原路径等价，接口只服务已有调用方 |
| 短 prompt、最终工具面一致、停滞作用域 | 值得先试，非自动收益 | 冲突提示与无谓调用可能继续 | 静态矩阵及小规模真实模型对照，不退化再采用 |
| 新 runtime crate、事件总线、策略 DSL、工作流序列化 | 当前不需要 | 无已证明损失 | 将来有两个真实消费者及具体收益才重审 |
| 受限 helper、跨轮 scope、额外 review model | 不属本轮基础必要项 | 某些自动诊断仍需前台评审 | 独立真实需求、运行边界与净收益证明 |

预期效果是协议耦合减少、执行归属更清楚、取消与结果更可信、提示和工具能力一致，且默认仍是现有 Shell/Core 两个进程。审批次数、费用、延迟或内存下降只是待测收益，不承诺百分比；ACP 自身不提高模型能力、不保证只读、不提供 exactly-once。

## 2. 评估方法与证据等级

四个独立评估域分别负责标准/SDK、runtime/参考源码、PTY/安全、任务体验/成本；先独立判断，再交叉质询反例，最后由主评估者依据源码与需求取舍。Agent 赞成票不是证据，也不代表产品接受。既有 Qwen、Pi/Rig、Goose/OpenCode 调研作为输入，没有将已有草案设为必须证明正确的结论。

源码基线为 ANOLISA `1300cfcd50938af9323c0c5df4faaaf00ff6be60`，不是旧本地 checkout 或安装 RPM。参考仓库已拉到本地并固定 revision；清单见工作区 `third-part/cosh-runtime-review-20261113/source-manifest.json`。研究路径与临时快照仅供本地追溯，长期依据为固定 revision 的仓库相对路径，不依赖临时目录存活。

| 证据 | 可以支持 | 不可以支持 |
|---|---|---|
| 固定版本源码与静态反例 | 责任边界、潜在路径、下一步复现方案 | 已运行 FAIL、实际泄漏或完整安全证明 |
| 现有局部测试 | 对应夹具的行为 | 新架构、Linux PTY、其他 peer 或安装正确 |
| 独立 Agent 质询 | 暴露假设、缩小范围 | 模型一致即真实性或验收 |
| 后续真实 Linux/模型/peer/RPM | 指定候选及环境的行为 | 未测镜像、权限、版本或发布范围 |

研究轮历史检查：macOS ACP 定向 33 通过、4 项在启动前因 Linux 描述符固定要求失败，退出码 101；前台序列化 2 通过；另一测试过滤器匹配 0，不算通过。未调用真实 provider，未跑新架构 Linux PTY/双向互通/RPM。当前新反例均为静态审阅，尚无完整链路复现。

## 3. 源码风险、独立优化与保留的既有机制

下表路径均相对基线 `src/cosh-ng/`，行号用于定位该 revision，实施时重新核对。

| ID | 静态事实与前提 | 影响与建议 | 验收 oracle |
|---|---|---|---|
| S01 | `crates/cosh-core/src/core.rs:1852–1869` interrupted 后记录当前结果并直接返回 Completed；1195–1204 是 fatal 后剩余调用补结果，1874–1887 是批次后终态。`headless.rs:878–889` 的成功映射需要一起核查 | 助手一次声明 A/B，在 A 决策时中断可能遗漏 B 配对并报成功。复用已有批次 finalizer，先复现 | 每个声明 ID 有结果；后续工具不启动；中断与任务成功不混淆 |
| S02 | `core.rs:1346` 先 classify，1424–1437 merge Hook patch 沿用旧 outcome，1445–1449 可直接执行；1247–1255 审计 input hash 也早于 patch | 可信 Hook 修改最终参数后分类/审计可能陈旧。重新校验、分类并绑定最终动作，保留 Ask/Block 优先级 | 该最终敏感写入在未批准前次数为零；卡片与审计均对应最终参数；拒绝不降级 |
| S03 | `core/provider_access.rs:699–700` 无 trial access 时直接 await；`core.rs:692–699,762–763` 使用该路径；Shell 外层仍有 `cosh_core_service/runtime.rs:195–215` 的 grace/kill | 现有产品有外层兜底，但不能替代共享内核的协作取消。最小宿主任务传递取消 | 各 provider 等待均能取消，外层 kill 仍有效；不宣称已派发动作未执行 |
| S04 | `core.rs:2282–2304` host 结果解析只保留部分内容/exit；1569–1577 missing exit 不产生 is_error；`provider/mod.rs:127–134` 接口丢弃 `_is_error` | 优先贯通已有 signal/coverage/cleanup，不以缺失退出码证明成功。领域事实和 provider 支持的 wire 字段分开 | missing exit/signal/截断/未完成都保留真实状态，模型和 UI 不伪造成功 |
| S05 | `loop_detect.rs:3–5,18–35` 用参数前缀；detector 为 Core 字段，新用户入口 `core.rs:388–393` 未见重置；1858–1860 仅 warning | 改请求作用域、完整最终动作摘要、真实结果摘要及限次告警；不引入同结果硬停 | 不同参数尾部不混淆，新请求重置，有限轮询不误停 |
| S06（纠正） | `hook.rs:1494–1523` 强制 BeforeModel 同数量/同名称/同顺序，1531–1547 强制估算 token 不增长；原声明预算是保守上界 | 撤回此前“Hook 删除工具或扩大声明导致失配”的缺口判断；保留现有压缩契约。最终计量和可信 profile 筛选是独立需求，不以此新增过滤能力 | V03：非法更新拒绝、合法压缩保持集合及上界；如另采用 profile 筛选，在提示/预算前完成；不将旧误读写成 FAIL→PASS 缺陷 |

S02 的精确安全范围：Auto 的 ShellExec 本来通常 RequireApproval，不能宣称“只读 shell 被 patch 成写命令直接绕过”。可用普通 `write_file(note.txt,"safe=true")` 在 legacy Auto 下原为 Allow，再由已启用可信 Hook patch 成假数据 `MII...\n-----END RSA PRIVATE KEY-----`；前提为 allowed_tools 不含 write_file，聚合 Hook 决定为 Allow/Passthrough，没有 Ask/Block/HookFailure。孤立 END 标记被 `redaction.rs:137–141` 判敏感，`redaction/private_key.rs:45–67` 不会按 BEGIN 块脱敏，WriteFile 在 `tool/write_file.rs:75–94,623–649` 对该假数据 warning 后仍可写。重分类应 Ask，现路径可能沿用 Allow。普通 AK 或完整私钥会先脱敏且写工具拒绝 placeholder，不能用来证明实际秘密泄漏。本项重要性是最终动作绑定不一致，不是已证实的不可信提权或泄漏。

以下不是待从零建设的缺口：现有 max_turns、Stop Hook 预算、参数纠正预算、缺 MessageEnd 拒绝、未完成 tool 参数拒绝、fatal 后补齐剩余工具、BeforeModel 同集合及不增长校验、transcript/压缩投影分离、checked emit、owner receipt 和落盘顺序应保留。新增方案首先复用，不借重构削弱。

## 4. 从 harness 借什么、拒绝什么

| 参考实现 | 精选机制与本地落点 | 不采用及原因 |
|---|---|---|
| Qwen Code `packages/core/src/core/agent-loop.ts` | model/tool 批次共享入口，丢弃废弃尝试，工具进度有序；用于 ACP/legacy 共用循环 | Node 和 coding 会话/ACP 分支整套迁移；与 Linux Shell 运行成本及现有 owner 不匹配 |
| Pi `packages/agent/src/agent-loop.ts,types.ts` | 模型投影与 UI details 分离；短 prompt 依据工具；用于最终请求和 host metadata | 不默认并行、不迁入 Node；Cosh 已有压缩与 transcript，不重建 |
| Goose `crates/goose-agent/src/machine.rs` | 推进决定/effect 边界，取消 finalizer；用于宿主交互和收尾 | 不采用 alpha GDK/SessionManager/新数据库，不搬完整状态机平台 |
| Rig `crates/rig-agent/src/run/mod.rs` | 小阶段及 provider 可局部复用的接口观念 | 可序列化 run 不解决副作用重放；MSRV 1.97.1 及依赖升级有成本，不整套采用 |
| OpenCode `packages/core/src/session/runner/step.ts` | 协作取消、输出 drain、终态分离；用于有限收尾 | 不迁入 Bun/Effect、Job/Inbox/Location，不把超时动作自动后台化 |

各 revision 及固定源码链接见架构设计第 5、14 节。采用的是少量机制及其失败边界，不把参考项目功能量作为完整度标准。现有 ContentGenerator、工具注册、进程清理和证据存储仍是实现主体。

## 5. 经过质询修正的决定

| ID | 初始候选/异议 | 反例与最终决定 |
|---|---|---|
| D01 | 标准 permission 之后再调用 host execute，追求消息尽可能标准 | 引入批准与执行之间的丢失/取消/重复状态，标准也不要求客户端执行先经过该 permission。采用一次 execute：Shell 本地检查、卡片、单次派发、实际结果；owned 只登记负责。native 工具保留标准 permission |
| D02 | 将标准 terminal 加 currentShell metadata 代替扩展 | create 是新 terminal、命令后台运行，release 须终止未结束命令；共享用户 Shell 的 top-level eval、串行和 OSC 仍需约定。首期明确 host-shell 扩展更薄；标准 terminal 只用于独立 profile |
| D03 | command/cwd/env digest 冻结完整批准环境 | alias/函数/脚本/外部文件不可由有限摘要冻结。绑定真实连接/PTY/action，批准实际当前 Shell 的展示文本，必要时 expected cwd；不新造全状态 generation |
| D04 | 同动作同结果持续则硬停 | activating 连续相同可能合理，时间戳改变可能无进展。首期只限次 warning + max_turns；非法参数/拒绝/未知副作用依原硬边界，不建语义进展引擎 |
| D05 | 所有晚批准拒绝、Core 死亡即中断或重试 | 现有有效本地卡片存在 late-decision recovery。未派发/未取消/同 PTY 可明确告知后本地执行，旧 RPC 不复活；运行中 Shell 保持 owner，Core 死亡不自动重放 |
| D06 | 唯一终态前清空所有后代 | 正常后台 job 和 Hook 服务合法。`crates/cosh-core/src/process.rs:225–237,755–803` 明确保留成功后台后代。PTY 以前台命令边界结算；helper 全后代清理若采用仅限其域 |
| D07 | drop future 后立刻返回标准 cancelled | ACP 要求返回 cancelled 前 ongoing operations 成功中止。UI 可先取消显示，但运行事实独立；无法证明中止须准确失败/未知，不伪装正常取消 |
| D08 | SDK 最新主版本即全 workspace 升级；复用 SDK 即天然有界 | 2.0/5.2 均有串行回调及行读取/无界内部通道。先用现有官方连接验证，对照 5.2 局部采用及必要修复收益；不自行写 wire，不静默全局升级 |
| D09 | 为互通全部建 evidence/read，resource_link 自带读取 | ACP 无通用 Agent resources/read；fs/read_text_file 方向也不是 Core ledger。先有界内容+引用+本地 ledger，按需再研究读取 |
| D10 | 大迁移统一验收后才做行为 eval | 原路径小修和 prompt 可独立落地；H1 早做真实模型成对 smoke，H2 单真实 peer 开发出口，H4 广泛兼容/切默认才要求两个独立实现及 RPM |
| D11 | auto 草案已经在开发，直接作为新默认 | 开发投入不证明必要或正确；先确定性流程，helper 单次明确输入独立实验，scope/revoke/用途平台与 reviewer 另凭收益决定 |
| D12 | 最小 host execute 只传命令和 owner | 现有 Hook Ask 还通过 hook_requires_approval 阻止 auto/trust/turn consent 覆盖；扩展必须保留可信 mustAsk 语义。缺失/未知不按 false 解释，false 也不授予自动许可；V01 验证 |
| D13 | OSC token 可证明当前 Shell 的真实执行边界 | 同 Shell 代码可读取 token 并调用 emitter；首期只提供协作边界，不承诺抵抗同 Shell 伪 marker。V02 表征有效 precmd 后继续阻塞的实际行为；未防护不计通过，需求超出边界则拒绝该 profile |
| D14 | BeforeModel 可删工具/增长，所以必须重排预算 | 此前判断错误；现有校验已禁止，预算保持保守。保留保护，精确计量与独立 profile 筛选按需求采用，不能为错误 oracle 放宽限制；V03 验证 |
| D15 | A 直接 ACP 必多一次 IPC；B 内部接口天然快且标准接入已齐 | A/B 均可保持两进程、一个内核；B 无需代理但长期多一套 wire。Shell 接其他 Agent 均需 ACP Client。两者成本须实测，近期渐进接线不预先保证切 A 默认；长期 B 的目标差异单独决定 |

本轮三项修订的源码依据：强制 Ask 在 `crates/cosh-core/src/core.rs:1455–1489` 写入请求，Shell 的 `approval/requests.rs:217–276` 保留，`agent/approval_bridge.rs:44–46,209–211` 和 `approval/resolution.rs:135–140` 阻止自动/批量批准；同 Shell marker 的可见性见 `shell_host/marker/bash.sh:12–13,334–363`，解析与 precmd 结算见 `shell_host/osc.rs:309–319,410–470`。这些是静态依据；本轮未运行新的完整链路夹具，不宣称已复现绕过或已完成防护。

再审补清两处兼容细节：BeforeModel 多 Hook 后续非法更新保留此前最后有效声明（`crates/cosh-core/src/hook.rs:852–873`）；旧 Core 的 false 合法省略（`protocol.rs:488–489`），由已识别 legacy 适配器按旧版本契约归一（Shell `adapter/control_protocol.rs:411–414`），不能将新 host-shell 必填规则直接套到旧普通请求。V01/V03 均覆盖对应正例和反例。

侧会话输入经独立核对后纳入 A/B 对照，但不构成实现授权。[Qwen 官方架构](https://qwenlm.github.io/qwen-code-docs/en/developers/architecture/)说明 TUI/headless 直接调用 runtime，ACP/daemon 是另一入口；[Qoder ACP](https://docs.qoder.com/cli/acp)只足以确认其 stdio 对外接入，不能推断全部内部 UI。[ACP transport](https://agentclientprotocol.com/protocol/v1/transports)确认 JSON-RPC/换行 framing，不能提供 A/B 性能结论。Cosh 尚未运行本次对照，不声称任一路径更快。

## 6. ACP 采用关口与扩展边界

官方依据采用固定仓库 [ACP v1](https://github.com/agentclientprotocol/agent-client-protocol/tree/3e4ce6468d68d89536b847628f6ff55239565583/docs/protocol/v1) 和 [Rust SDK](https://github.com/agentclientprotocol/rust-sdk/tree/91d368ad8b6761f314da9095c4a030d365c079a9)。SDK 5.2.0、Rust schema crate 1.17.0、规范仓库 schema package 1.31.0 是不同发布轴；wire 仍选稳定 v1，不推断为 v2。

同时核对现有锁定的 registry 源码：SDK 2.0.0 的 `src/concepts/ordering.rs:9–21,43–58` 也规定串行回调及重入死锁风险，`src/jsonrpc.rs:1703–1705,5576` 也有 unbounded 与行读取问题。旧 schema 1.5.0 的 `src/v1/content.rs:39–43,52–55` 已规定 text/resource_link 必选，`src/v1/agent.rs:2834–2838,4975–4982` 已规定 stdio MCP 和新会话责任。因此必选项不是从最新文档倒推的新升级要求。先以现有官方连接验证，再对照 5.2 局部接入成本与必要修复；稳定 elicitation 在 5.2 可用，不能把它说成 2.0 的稳定基础保证。

| 关口 | 必须核对的官方源与判断 |
|---|---|
| 基线符合性 | `initialization.mdx` 的 text/resource_link 必选，`session-setup.mdx` 的 stdio MCP；按锁定版本逐 MUST 建测试，未实现只称受限 profile |
| 连接调度 | SDK `concepts/ordering.rs`：接收回调串行。短回调注册，模型/UI/PTY 等待移出接收循环，官方 responder 关联；cancel/反向调用不死锁 |
| 内存/任务上界 | `jsonrpc.rs` ByteStreams `.lines()` 和 transport actor 无界通道；帧读取前限制、准入/队列/任务预算及超载退出要用真实慢消费者验证 |
| 新旧 SDK | 2.0 与 5.2 API/features/批量支持不同；ACP stdio 单消息行为为新边界，旧 Gateway codec 兼容单独评估，不能顺手复制或删掉 |
| 用户提问 | 所选稳定 schema 的标准 elicitation 优先；不因旧 SDK 需要 unstable 就另造私有通用 ask 协议，也不向旧 peer 虚报稳定支持 |
| 取消 | `prompt-turn.mdx` 的 UI 取消投影与 prompt cancelled 的实际中止要求分开；未中止的异常失败不计正常取消符合性，不自定义 Unknown stopReason |
| terminal | `terminals.mdx` 的 create/output/wait/kill/release 保真，不伪装当前用户 Shell |
| 命名 | `extensibility.mdx` 的 _ 方法与受控反向域名 namespace；命名权威须登记，暂定责任名不作为发布 API |

权限请求是合作协议，不能约束外部 Agent 自有 executor。只分析/禁执行硬 profile 必须在发送 session/prompt 前确认执行限制，不能从 mode、prompt、metadata、关闭 terminal/fs 推断。需要该保证而对端无可验证约束时明确拒绝该 profile；普通互通仍可在其实际独立执行语义下使用。

## 7. 实施顺序、收益与停止条件

1. 对应夹具先行：固定实际候选与既有行为。S01–S05 分别验证、分别小修；S06 保留已有保护，仅按独立优化需求验证，不再作为已知缺陷修复。
2. 提示词和请求投影独立做小样本真实模型对照；相同输入、工具、模型参数和预算，不把字节减少当成本收益。
3. 仅整理 ACP 必须的宿主交互，验证单读者、取消、领域投影，保留一个循环。
4. 官方 ACP 采用实验、必选输入/MCP、单真实 peer 加异常 peer；仍不切默认。
5. 单 host execute/owned 接原 PTY，跑批准、输入等待、信号、部分写入、晚决定、断线和死亡矩阵。
6. 扩大任务样本、两个独立实现互通、目标安装/升级回退及 A/B 对照后，记录采用决定；具备实际交付授权才切明确范围默认，否则保留原路径，后续凭数据清退 legacy。

具体范围/风险/oracle 见计划 H0–H6。任一框架或抽象不能证明 Cosh 场景净收益就不采用；未知 SDK 边界、未复现反例和模型波动保留为 gap，不用更多组件掩盖。helper 或 reviewer 收益不足可以放弃，不影响 ACP 主线。性能目标在测量前冻结，不事后改门槛。

## 8. 未决项与交付状态

| 未决项 | 最晚解决时点 | 当前状态 |
|---|---|---|
| S01–S05 完整链路可复现性、范围与修法 | 对应 H1 修改前 | 静态证据，非运行缺陷验收；S06 已纠正为独立优化与既有保护 |
| mustAsk 跨协议保真及 OSC 信任域限制 | H3 规格及 G3 前 | 文档边界与 V01/V02 已明确；夹具未实现、未运行，同 Shell 防伪不属于首期保证 |
| SDK5.2/旧2.0 依赖接线、重入和全链路有界性 | H2 选型出口 | 推荐目标及 adoption gate，尚未实测 |
| 最终 namespace、错误码、字段上限、实例身份复用 | H3 wire 规格前 | 责任已定，命名/字段未发布 |
| Esc/前台信号的产品策略与 ACP 收尾映射 | H3 行为出口 | 保留现状，不能偷偷合并 Ctrl-C 语义 |
| peer 版本、镜像/身份/ARM64/安装覆盖与数值预算 | 对应阶段及切默认前 | 需执行记录，不虚构完成 |
| prompt 净收益与 auto 是否值得增加后端 | H1c/H6 独立出口 | 待真实样本，保留收窄/放弃选项 |

本轮产物是中英文架构、计划和评估依据。未更改产品代码、依赖或默认行为；文档检查与评审不能替代实现、互通、Linux PTY、RPM 和发布验收。

技术未决项不等于等待用户逐项授权；当前实现、可选实验采用及实际交付的范围，统一见[计划第 11.1 节](core-runtime-harness-implementation-plan.md)。本轮文档工作没有未解决的用户输入依赖。
