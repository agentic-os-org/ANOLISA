# Cosh 首屏推荐统一展示执行计划

[English](../en/startup-suggestion-unification-plan.md)

状态：2026-11-16 已形成计划并于同日执行任务一至五（focused lib/logic/protocol
门禁通过，layout 与 inventory 审计通过）。ALinux4 真 PTY 验收与 provider 绑定
验证仍未完成。[设计文档](startup-suggestion-unification.md)为合同，面向
`cosh-shell` 实现者与评审者。

## 进入条件与范围

1. 为统一体验创建后续 tracking issue；#10179 和 PR #10182 只覆盖上一轮品牌样式修正，
   不能视为本方案验收。
2. 阅读当前组件规则及按改动选测试的指引；fetch 官方 main，检查与本文已审代码基线
   `ce4c5724ec17be30e197f12c19768b1e4bc139c1` 的差异。
3. 实现使用隔离 worktree；保留本会话已有证据和无关未跟踪文件，不直接在已合入旧分支
   上继续提交而跳过基线更新。
4. 先固化 ALinux4 真 PTY 基线和 focused 失败断言。确定性夹具的选择／渲染不需要模型
   调用；真实 provider 的上下文验证另列结果。

编写本计划不执行安装、建资源、建 PR、commit 或产品实现。后续获准执行时遵循既有
issue-fix 与 test-acceptance 流程。复用既有依赖，不修改 `.env`、锁文件、版本、打包或
CHANGELOG。

下文路径相对 `src/cosh-ng/`，仅授权对应职责的文件。确有调用依赖才扩展，并在 issue
记录；不能向超过 1000 行的 production 文件追加行为。
`runtime/startup/recommendations.rs` 通过将变更的选择／展示职责放入既有 owner 目录
保持在 700 行阈值以下，避免继续膨胀首屏编排文件。

## 任务一：固化身份与曝光回归

先冻结设计中的“共用底座／分场景”矩阵。同一 report 夹具分别验证 Bash／Cosh 的
展示投影，另行验证 `LoginBanner`／`Startup`／`OnDemand` 的采集准入。断言切换展示
target 本身不会触发采集或修改原报告。覆盖 Cosh 内部运行 Bash、banner on/off 不
影响内部 health、登录指南与会话健康正常紧凑行的区别，以及 Bash 路径不读取个人
数据、不新增推荐模型请求。使用既有 health-plan、guidance 和 health-banner 测试
owner，不复制采集器或公共指标渲染器。

Owner：`crates/cosh-shell/src/runtime/startup_tests.rs`、既有
`runtime/startup/recommendations/health.rs` component tests、
`recommendation/personal_planner.rs` tests 和 `ui/guidance/tests.rs`。

- 构造健康／个性化混合的 0／1／2／3 项、首条健康被抑制、准备与发布间引导变化、
  相同问题文字但 ID 不同的用例。
- 断言输出准备、ghost route、绑定及可计曝光项使用完全相同的有序 ID，保持
  `MAX_VISIBLE = 3`。
- 证明只 claim 首条不足以支撑多健康候选展示；覆盖并发 claim 竞争失败，不给省略项
  计账，不偷偷补位。
- 覆盖不同健康 ID 共用 finding fingerprint、整批一次持久化、写入失败和回复超时。
  在 40×24／80×24 下用三条达到现有上限的个人问题证明高度溢出，不能只用短预览。
- 固化失败输出与日志，测试放既有 private 模块，不新增 root 测试实现文件。

出口：新增断言因目标问题失败，不能拿无关失败冒充 FAIL 基线。

## 任务二：准备统一的有界会话快照

Owner：`runtime/startup/recommendations.rs`、
`runtime/startup/recommendations/health.rs`、必要时新增小型私有
`runtime/startup/recommendations/selection.rs`、
`diagnostics/health/suppression/claim.rs`。claim 在进程内、既有非阻塞 try-lock 下
完成；旧 claim worker flag、分派与 IPC transport 连同测试一并退役。

- 保持既有 planner 排序、范围、认证和容量。
- 将健康候选资格准备与首条邀请展示分开；planner 选择后，仅对最终快照中的健康 ID
  批量原子 claim，最多三项，进程内复用非阻塞 try-lock，不设子进程预算。
- 发布前复验引导，只将竞争成功的 ID 留在可见／可选集合。Bash 单条 claim 和抑制
  格式不变；退役 transport 的消费者（flag、分派、decoder、测试）先枚举再删。
- 先判断整批资格再记账，持久 finding key 去重，返回竞争成功的候选 ID，一次落盘。
  不能循环调用 `record_and_persist` 冒充原子批处理。会话 worker 的旧首条回复用
  进程内批量结果替换，worker flag、分派与 decoder 连同测试退役；锁竞争或落盘
  失败整批保守丢弃并记录 warn，成功／竞争／落盘失败路径都有确定性见证。
- 展示项、绑定和 ghost 都由同一快照生成，Cosh 首屏不再调用仅个性化的展示投影。
- 随调用方迁移删除旧仅个人投影和首条邀请字段；引导有效性与绑定清理由共享发布 owner
  承接，不保留旧包装器来维持这些职责。
- 同时覆盖初次就绪和延迟就绪的健康路径，保留无报告、关闭、compact、read-only
  等现有行为。

出口：任务一身份／曝光测试通过；既有 Bash claim 测试通过。

## 任务三：绘制确认后的两行推荐区

Owner：新私有 `ui/guidance/suggestions.rs` 及 private component tests、
`ui/guidance.rs`、Cosh 首屏展示调用点、既有 `i18n/en/health.rs` 与
`i18n/zh/health.rs`，以及从其定义定位到的必要 MessageId 登记 owner 文件。

- 零条不画；单条不加前缀；两／三条带编号与恰好一个选中标记；不加第二张卡或来源标签。
- 按显示宽度折行、续行对齐正文、操作提示按组折行；编号、箭头和排版空白不进入问句。
- 计算包含实际提示符与分隔空行的总行数，验证完整区域仍在可见屏幕。几何未知或溢出时
  采用设计中的静态完整列表／提示符局部路径，不裁切、不隐藏候选。初次与晚到入口都接入
  新 writer 后删除旧 Cosh 邀请追加／样式分支，保留事实清理。
- 增加 Cosh 专用本地化健康短问句，保留范围和目标参数；个人候选原问题完整保留。
- 保持既有资源／发现绘制、Bash 邀请和 `/health`；不向 `ui/agent_render/` 兼容层添加新功能。
- Cosh 选择器展示放在共享健康渲染器之外，保留场景自己的指南／紧凑行标志。确需改动
  共享渲染时，同时验证同 report 的 Bash／Cosh 成对输出和正常／警告／严重／部分完成，
  不能只看 OOM 预览。
- 添加 40／80／100 列、中英文、长服务名、plain／NO_COLOR、相同文字的语义排版断言，
  不只复写 format 字符串而绕过可见／可选一致性。

出口：统一展示测试通过，Bash 输出／引用回归保持通过；此任务单独通过不代表动态选择验收。

## 任务四：选择同步与安全终端所有权

Owner：`raw_input/relay.rs`、`raw_input/mod.rs`、既有 raw relay tests、
`shell_host/raw_relay/input_events.rs`、相关 prompt 所有权调用点，必要时新增小型私有
shell-host selection-surface 模块。

- cycle 通知携带稳定身份与快照代次，保持 relay 同步选择行为，覆盖同一输入块内连续的
  Shift+Tab／Tab／Enter。
- shell host 单独拥有活动推荐区和锚点；runtime 与 relay 不能各自重绘同一块区域。
- 明确实现 Live／PromptLocal／Deferred／Draft。选择装饰与 ghost 同步更新，验证代次
  与所有权。只有空提示符仍可证明时才局部降级；否则延迟全部绘制、提示符恢复和候选
  路由安装，下次安全边界复验待展示状态，不盲目重放旧 RestorePrompt。
- Tab、编辑、取消、晚到结果和后台输出不能改变已填草稿的 ID／文字／绑定。保留推荐
  选择优先级，以及退出推荐状态后的正常 Assistance 模式切换。
- 覆盖同文异 ID、陈旧 cycle、长折行提示符、按键拆包／合包、resize 和前台输出。新增
  纯逻辑放 private component，进程／PTY 放既有 `raw_cli`／`shell_host` target。
- 卡片或前台进程占用时验证推荐写入为零且不抢路由。覆盖只缩小高度、长列表顶部已
  滚出屏幕；恢复保留候选身份，但必须拒绝已经失效的待发布快照。

出口：当前指示项、直接 Enter 与 Tab 接受项始终一致，不按猜测的光标位置擦屏或改用户草稿。

## 替换与退役检查点

任务二至四执行设计中的迁移清单，不作为无关的全仓清理，也不留作可选后续工作。
修改前记录每项退役符号的 production、测试、re-export 和 worker 分派消费者；接入后
逐项写明删除、替换或保留及保留消费者。

- 一个快照接管职责后，删除旧 Cosh 单条邀请展示、仅个人首屏卡／投影、facts／invited
  双次渲染和被替代的渲染后 ghost 补偿。
- 保留 Bash 登录／失败／引用、`default_login`、显式 `/health`、命令后推荐面板、共享
  notice 和健康渲染，不误删为首屏专用 helper。旧 Cosh 邀请分支删除后仍保留显式场景投影。
- helper 只剩一个场景时，可在保持准入／安全检查的前提下简化闲置抽象。旧函数测试改为
  新入口的整体行为断言，保留抑制、隐私、绑定和输入回归意图。
- 核对废弃 import／export、enum 字段、本地化 ID、夹具与 worker flag，结合仓库搜索和
  Linux 上受影响 target 编译验证。public 导出和测试专用调用可能掩盖 production 孤儿，
  仅无 warning 不够；不新增 dead-code 豁免、停用旧实现或没有实际支持消费者的兼容开关。

出口：清单每项都有结论，Cosh 新路径只有一个 owner，有意保留的旧外观代码有已验证消费者。

## 任务五：focused 验证与 ALinux4 证据

在 `src/cosh-ng/` 执行，完整输出和每条退出码存入 issue 证据目录，不以 `| tail` 结果
作为成功条件。

```bash
cargo fmt --all -- --check
cargo test --locked -p cosh-shell --lib ui::guidance
cargo test --locked -p cosh-shell --bin cosh-shell recommendation::personal_planner
cargo test --locked -p cosh-shell --bin cosh-shell runtime::startup
cargo test --locked -p cosh-shell --lib selection_surface
cargo test --locked -p cosh-shell --bin cosh-shell selection_surface
cargo test --locked -p cosh-shell --lib diagnostics::health::suppression
cargo test --locked -p cosh-shell --lib raw_input
cargo clippy --locked -p cosh-shell --all-targets -- -D warnings
bash crates/cosh-shell/scripts/check-layout.sh
bash scripts/check-test-inventory.sh
```

先列出编译 target 的测试，确认过滤条件确实运行相关用例而非零条。新增 selection-surface
测试全部运行，模块落点若不在上述范围则补 exact filter。通过 `-- --list` 确认既有
raw_cli／shell_host 用例的注册全名，再用 `-- --exact` 执行，不编造用例名称或接受零测试。
只因所有权契约变更或已发现回归扩大测试；全 workspace 门禁需要明确请求。

ALinux4 真 PTY 验收矩阵：

| 场景 | 必须观察到 |
|---|---|
| 0／1／2／3 项 | 健康事实保留；一个推荐区；数量与顺序准确 |
| 混合来源／首条被抑制 | 无隐藏可选项、重复区域或对省略项错误计账 |
| Shift+Tab／Tab／直接 Enter | 编号稳定；标记、ghost、文字、绑定一致 |
| 已填入／已编辑 | 选择或结果变化不覆盖、不改绑 |
| 40／80／100 列、中英文、长目标 | 不裁切、不丢标识、不残留旧折行、不漂移光标 |
| resize／输出／卡片／前台所有者 | 安全重排或明确局部退路，不猜测擦屏 |
| 提示符所有权失效 | 新空提示符可证明前，不降级绘制、不恢复提示符、不接管候选路由 |
| 40×24／80×24／高度缩小／最长问题 | 活动区域装不下时完整静态列表；所有已显示 ID 在安全提示符可选 |
| 同 finding 多问句／写入失败／回复超时 | 保留竞争成功 ID，finding key 去重，一次原子落盘和保守超时语义 |
| guide off／AI off／compact `--ask` | 既有策略与首屏行为不回归 |
| Bash／显式 `/health` | 邀请和主动健康展示不变 |
| Cosh 内部运行 Bash／banner off | 仍走 Cosh 会话选择；Bash banner 开关不关闭 Cosh health |
| 场景采集与时限 | 不扩大检查项，不多扫描，不在 Bash 读取个人数据，不放宽截止时间 |
| color／NO_COLOR／plain／read-only | 可交互时选中清晰；其他情况不输出无效快捷键 |

使用修改后的真实二进制、真实 Bash／PTY 和真实按键。健康夹具与合成个人候选明确标注，
保存 cast、全尺寸截图、二进制 checksum／基线 SHA、逐项结果和清理记录。声称 provider
绑定验收时须经真实 adapter 核实所选请求与上下文，否则明确未验证。不要为了视觉演示
制造宿主真实 OOM。

出口：focused green 和上述真 PTY 证据齐备；缺失的 PTY／绑定 scope 如实列为验收缺口，
不能从静态稿或 CI 推断通过。

## 任务六：文档、评审、交付与清理

- 实现生效时同步更新 `docs/design/{en,zh}/shell-entry-guidance.md` 和受影响的 canonical
  推荐用户指引。本设计与计划也需跟进实际落点决策。
- 评审候选身份、批量 claim、草稿所有权和重绘出口，而非只看截图；基线失败单独记录。
- 评审附完整退役清单与搜索／编译证据，确认旧函数测试已迁移而非简单删除，Bash／
  主动查看／命令后推荐的保留消费者仍正常。
- 获准后创建关联后续 issue 的聚焦 PR，附真实前后 PTY 证据与未验证范围。本次不包含
  版本、打包或 CHANGELOG 变更。
- 原则上保持一个逻辑实现提交；只有可独立编译的职责确需拆分时才拆。遵循仓库归因规范，
  rebase 新基线后重跑适用门禁。
- 确认合入后保留最终证据，只清理本任务资源和分支；托管 worktree 用托管归档工具，
  保留 PR 仍引用的资产，实际复查后才报告清理完成。

回退：撤销实现变更，保留用户状态和既有抑制格式。版本／RC／RPM 验收需要独立制品与
环境证据，不由本源码变更计划代替。
