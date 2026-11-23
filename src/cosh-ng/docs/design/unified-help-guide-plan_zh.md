# Cosh 统一帮助与配置向导执行计划

[English](unified-help-guide-plan.md) · [设计](unified-help-guide_zh.md)

工作区：独立 worktree `cosh-unified-guide/anolisa`，分支 `feat/cosh-unified-guide`。
基线：`1300cfcd50938af9323c0c5df4faaaf00ff6be60`。不在原 main 工作目录实现。
本计划是可逐项执行的交付清单；开始落实不等于整套功能已完成。

## 执行与证据规则

每阶段按“失败测试 → 最小实现 → focused 通过 → 代码/交互评审 → 文档更新”推进。
Cosh 测试经 `cosh-lab` 执行，先 doctor、test plan；macOS 仅小范围精确过滤。
公共 API、进程、协议和真实终端分别放 logic、raw_cli、protocol、shell_host 层。
不新增依赖，不读取真实凭证，不在宿主修改默认 Shell，不改原工作区未跟踪文件。
本文列出的后续改动均在本次已确认设计范围；发现业务契约冲突时先记录并收敛 owner，不能旁路重写。

## P0：设计与基线

- [x] 记录三入口职责、五项配置、独立入门、终端交互、状态和安全边界。
- [x] 原生工具创建独立 worktree，并建立功能分支。
- [x] 获取 doctor；当前宿主有竞争，不能据此给性能结论。
- [x] 对首批根帮助执行原行为基线及新增行为失败测试，保留日志。

产物：双语设计和本计划，首批测试 red/green 证据；原型仍仅作视觉参考。

## P1：先让根帮助有用（本次首批实现）

文件：`runtime/invocation.rs`、新 `runtime/help.rs`、`runtime/mod.rs`、`main.rs`、
`i18n/message_id/help.rs` 与其尾段接线、`i18n/{en,zh}/help.rs`、`tests/raw_cli/startup.rs`，对应双语 README/user-guide。
`lib.rs`/`main.rs` 仅提高 i18n 宏递归上限；分类器新增测试放私有 `runtime/invocation/help_tests.rs`，避免跨越大文件阈值；既有 `runtime/invocation_tests.rs` 同步枚举穷举分支。

1. 先测试安装身份的 `cosh --help` 在无 TTY 下返回有效 Cosh 内容，不启动内部 Shell。
2. 覆盖管道 stdout、stderr 为空、无 ANSI、退出 0、临时 HOME 不产生配置/日志。
3. 覆盖 saved language、环境覆盖、不可读配置回退；沿用有界只读语言解析。
4. 分类器只接管精确根帮助；显式 Bash 帮助、带额外参数及内部身份仍保持原行为。
5. 新增纯输出模块，双语内容进入现有 i18n，不初始化 runtime/provider。
6. 索引只列已经存在的能力，不能提前宣传尚未实现的交互 guide/--plain。

完成条件：新增行为先 red 后 green；参数兼容单测、legacy help 和布局检查通过；实际用户文档与已实现行为一致。

## P2：guide 入口、静态内容与 Ratatui 外壳（已实现）

文件：`config/guide/cli.rs`、`config/guide/wizard.rs` 与 `wizard/content.rs`（共享内容/能力判定）、
`runtime/guide.rs` 与 `guide/{session,status}.rs`（交互控制器与只读状态）、`ui/guide.rs` 与
`guide/{screen,view}.rs`（终端生命周期与视图）、既有 Markdown 渲染边界、i18n；对应测试和文档。

架构修正：`config` 同时编进 lib 与 bin，而 `runtime`/`auth` 只在 bin。交互控制器需要登录 owner，
因此放 `runtime/guide`；`config/guide/wizard` 只保留双 target 共享的内容与 TTY/TERM/尺寸判定。
`main.rs` 在 `Invocation::Guide` 处先判定能力再分流，静态/旧子命令仍走 `config::guide::run_cli`。

1. 测试无参数、--plain、--help、on/off/status 和非法组合；静态路径不打开写 store。
2. 实现共享入门 Markdown 与五项入口描述；静态输出用已有精确命令，不暗示可交互。
3. 实现 TTY/TERM/尺寸判定、终端恢复 guard、单焦点列表、返回位置与小尺寸视图。
4. 先实现只读状态加载（Unknown 不当 Off），再接已有语言/健康/引导开关 handler。
5. 普通设置一次 Enter 保存；Esc 取消草稿；保存结果明确生效范围。

完成条件：80×24、60×20、40×16 以及静态路径快照/缓冲区检查；真实 Linux PTY 验证进入/退出、resize、Ctrl-C 和终端恢复。

落实说明：80×24 与 40×16 用 TestBackend 缓冲区断言（单列、居中、仅焦点反色、长值截断、过小提示）；
60×20 走同一单列路径。macOS 临时 HOME 的真实 PTY 已验证进入/退出、resize 到过小再恢复、Ctrl-C、
语言保存与终端恢复（退出后离开备用屏并恢复光标）；Linux PTY 与真实 VNC 仍待 P6 受控环境补跑。
AI 服务与默认登录 Shell 在 P2 为只读页（指向现有 /auth 与 cosh login），交互流程分别在 P3/P5 接入。

## P3：AI 服务复用现有认证

文件：`auth/` 既有 owner、最小 controller 调用边界、guide AI 状态/视图、i18n 与测试。

1. 固定现有认证的输入/状态/提交/结果契约，识别对交互会话的依赖。
2. 抽取必要调用边界，保留现有 /auth 路径，禁止复制模板、凭证写入或 provider 管理器。
3. guide 直接进入相同管理流程；返回后读取当前服务、配置来源和零配置偏好。
4. 测试系统只读、同名覆盖删除、保存并激活、取消、部分成功、重试和网络错误。
5. 用隔离凭证/假 provider 运行回归，真实 provider 单独记录授权与验收范围。

完成条件：同一底层实现服务两入口；配置≠连接；所有错误/取消不丢焦点，不打印密钥；原 /auth 回归通过。

## P4：提示与推荐收敛

文件：guide 子项、既有 config/mode/recommendation/GuideStore owner、i18n，logic/raw_cli 测试。

1. 复用引导提示发布与持久化结果，包含运行时发布成功但保存失败等状态。
2. 为分析方式增加经过验证的持久化入口，只改变新会话默认值；保留 /mode 当前会话行为。
3. 为分析方式、个性化推荐和 Bash 历史导入统一保存值/实际值/来源，沿用现有覆盖优先级；允许保存但被覆盖与 handler 拒绝保存分别反馈，不绕过强制关闭。
4. 对齐 manual 暂停推荐、关闭并清理、正常数据仅清理和固定保留策略说明，不新增保留时长设置；损坏恢复单独显示“重置数据并关闭推荐”，默认取消，不承诺保留未知偏好。
5. 历史导入单页按状态直接“允许导入并保存”或“停止导入并保存”，默认返回；有覆盖时改为明确的偏好保存文案，说明实际状态仍受环境控制；停止不清理已有数据，不增加前置页或第二层确认。
6. 解释实际历史基线读取和数据发送边界；确认后仅调用原 handler，清理/恢复失败与部分成功不得显示为全部成功。

完成条件：开关与数据结果分开验证；覆盖以下场景，且新用户能用一步进入目标子项：

- `COSH_SHELL_ANALYSIS_MODE` 覆盖新会话默认值，不误报已改变当前会话。
- `COSH_RECOMMENDATIONS_BASH_HISTORY=1` 下保存关闭偏好仍显示实际开启；环境关闭时保存开启偏好也不误报已启用。
- `COSH_RECOMMENDATIONS_ENABLED=0` 下开启请求被原 handler 拒绝且不持久化；非强制关闭时验证用户偏好优先级。
- 正常数据仅清理保持偏好；损坏数据重置后明确关闭推荐；权限/锁错误不自动重置；取消、部分成功和重读结果均有覆盖。
- 历史导入开启 → 关闭 → 重开，以及各步取消；停止后已有数据保留，覆盖未解除时不宣称已停止。

## P5：默认登录 Shell

文件：guide 登录页、`runtime/login/` 和已有 `shell_host` effect 最小接口、i18n、隔离 Linux 测试。

1. 先固定并测试复用 login owner 的私有提权请求/结果协议，再接页面；仅允许 Query/Set/Restore，绑定 username+expected UID，定义有界版本化状态快照和动作结果，不解析本地化 CLI 文本、不提权整个 guide。
2. 默认当前账号，按需查询其它用户；展示 username/UID/实际路径/支持状态。当前账号只读查询不额外提权；其它账号经前台 sudo 在提权子进程中查询，不能只运行 sudo 授权后在原进程调用 query_status。
3. 接入可信安装与记录检查，分别显示 Set、Restore、不可用/未知及原因；修改确认与子进程复核使用同一目标身份。
4. 离开全屏后交给系统前台 sudo 读取密码，协议通道不接收密码；恢复终端后核对结构化结果并通过同一权限边界重读。区分查询完成、写入前取消/失败、变更已验证和写入未确认；无有效结果或仅退出码 0 不作成功凭据。
5. 超时、异常退出、损坏或缺失结果按未确认处理，禁止盲目重试；身份不符停止操作。首设后立即刷新恢复能力，不修改当前终端。
6. 在受控 ALinux 4 RPM 环境测当前用户、其它用户、root、无 sudo、取消、冲突、写后验证失败、协议超时/无效结果/UID 不符和恢复，验证终端恢复与密码不进入协议或日志。

完成条件：提权协议及状态分类先通过 focused 检查；真实账号状态、目标 UID 与可信记录一致；未知结果必须可经同一提权边界重查，未确认前不能再次修改。
宿主无系统改动，临时账号/机器清理可验证。仅单测不能宣称登录 Shell 验收。

## P6：三入口统一与整体验收

文件：现有 Slash registry/notices/i18n 及 guide/root 内容，不重写 HelpPanel renderer。

1. 保留 /help 原分组、样式、scope 与当前状态，仅补必要入口提示和已核实的短说明。
2. 对比实际 registry，避免独立硬编码命令表；中英一致，内部命令不进入公开根索引。
3. 完整走无配置用户与已有用户路径，检查是否有重复选项、返回重定位、额外确认和术语歧义。
4. 真终端覆盖浅色/深色/无色、窄屏、resize、重定向和模拟受限 TERM；条件允许时补真实 VNC。
5. 执行 layout/fmt/focused 与受影响 crate/Linux gates；包级与 x86_64/ARM64 发布验收单列。
6. 更新双语用户文档、证据索引和剩余风险，评审后再决定提交/PR/发布步骤。

完成条件：五项配置及入门逐项验证，三入口没有重复业务实现；证据明确区分 prototype、focused、PTY、provider、RPM 和 release。

## 当前进度与交接

P0、P1、P2、P3、P4、P5 已完成 focused 验证，尚未提交。P6 尚未完成，不得在用户指南中写成已上线。
P2 已落地 guide 入口、`--plain`/`--help`、共享入门内容、TTY/TERM/尺寸降级、Ratatui 单焦点外壳、
只读状态（Unknown≠Off）与语言/健康/引导提示保存；AI 服务与默认登录 Shell 暂为只读页。
P3 已落地 AI 服务只读摘要（复用 `auth state` 查询、不触发 ECS 探测）与交接式 /auth：guide 暂停全屏→
子会话在首个就绪提示自动打开 /auth→退出后恢复全屏并重读；父进程交接期间忽略 SIGINT/SIGQUIT。
默认登录 Shell 仍为只读页（P5 接入）。
P4 已落地提示与推荐四子项（使用引导提示/命令分析方式/个性化提问推荐/推荐数据与隐私）：
新增 `config/shell_prefs.rs` 提供 analysis_mode 与 bash_history 的 saved/effective/source 与写入；
guide 复用 PersonalStore 做开启/关闭/仅清理/损坏重置，强制关闭（COSH_RECOMMENDATIONS_ENABLED=0）拒绝开启且不持久化；
只读状态不创建推荐目录。历史导入单页主操作按实际值与环境覆盖切换文案，停止不清理已有数据。
P5 已落地默认登录 Shell：新增私有版本化有界提权协议 `runtime/login/guide_protocol.rs`
（请求/结果走私有临时文件，密码由前台 sudo 在终端读取、不进入协议；仅 Query/Set/Restore，绑定 username+expected UID；
退出码 0 不作成功凭据，父进程重读结构化结果并核对目标 shell，无效/缺失/身份不符=未确认禁盲重试）。
guide 登录页当前用户 Set/Restore 走该协议，其它用户交接现有 `cosh login`；worker 复用 `cli::run` 提权逻辑不复制状态机。
2026-11-14 已同步修订双语设计和 P4/P5 计划，落实四项评审意见；此次仅调整文档，不代表覆盖处理、损坏恢复、导入关闭或提权协议已经实现。
每次落实后更新本节和勾选项，并附实际命令、退出码、测试数量及日志位置。
回退按阶段撤销代码，不删除用户凭证、配置、推荐数据或登录恢复记录。

## P2 验证记录（2026-11-14）

证据在工作区根 `.cosh-lab/runs/<运行编号>/`。退出码为底层测试进程退出码。

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| `config::guide::wizard` 共享内容/能力/静态分流 | 6 passed（lib+bin 各 6），退出 0 | `20261113T163029Z-cab2db22` |
| `runtime::guide` 控制器/只读状态/保存 | 9 passed，退出 0 | `20261113T163126Z-721d77d5` |
| `ui::guide` 终端视图缓冲区 | 5 passed（lib+bin 各 5），退出 0 | `20261113T163156Z-b8538614` |
| `config::guide` 全量（含旧 CLI 回归） | 52 passed（lib+bin 各 52），退出 0 | `20261113T163216Z-8ee0831d` |
| `i18n::tests::all_messages_have_en_and_zh_values` | 1 passed（lib+bin），退出 0 | `20261113T163856Z-d6438f0a` |
| `runtime::invocation` 参数与入口回归 | 47 passed，退出 0 | `20261113T163906Z-32400d8c` |
| 根帮助安装入口回归（含 guide 索引更新） | 1 passed，退出 0 | `20261113T164014Z-9a1d929e` |
| 旧内部 `--help` stdout 回归 | 1 passed，退出 0 | `20261113T163929Z-5e8c03cc` |

`cargo fmt --all -- --check`、`bash crates/cosh-shell/scripts/check-layout.sh` 退出 0。
新增 `markdown_styled_lines` 曾使 `ui/agent_render/mod.rs` 超 700 行，已改为直接暴露 `MarkdownRenderModel`，布局复检通过。
macOS 临时 HOME 真实 PTY 手工验证：进入/离开备用屏、Esc/Ctrl-C 退出 0、resize 到 30×10 显示过小提示再恢复、
语言保存写入 config.toml 且光标恢复；`COSH_SHELL_LANG` 覆盖时界面语言保持环境值（符合设计）。
crate/workspace 全量、Linux PTY、真实 VNC/provider/sudo、RPM 与发布 gate 均未运行，不能从 focused 通过推断。

## P3 验证记录（2026-11-14）

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| `adapter::cosh_core_auth`（含 2 个新增 guide 摘要纯函数测试） | 9 passed，退出 0 | `20261113T165853Z-67c47911` |
| `guide` 全模块（config/runtime/ui/invocation 等） | 76 passed，退出 0 | `20261113T165928Z-d589834a` |
| `i18n` 完整性与尾段顺序 | 15 passed，退出 0 | `20261113T170045Z-24453620` |
| `runtime::` 大范围回归 | 801 passed、3 failed，退出 101 | `20261113T170113Z-8b221e49` |

3 个失败均与本次改动无关：2 个 `controller::hook_tests::smart_mode_*` 为既有失败（单跑仍失败，
`20261113T170145Z-6ae90085`，调用未改动的 `render_inline_guidance`）；1 个 login lifecycle 锁测试为并发
flaky，单跑通过（`20261113T170136Z-483a96e2`）。i18n 尾段顺序测试原硬编码 `LoginRecoveryMissing` 为尾，
追加 `cosh_root_help`/`guide_wizard` 尾段后更新尾断言（修复前 `20261113T165937Z-2c0bb9aa` 失败）。
`cargo fmt --all -- --check`、`bash crates/cosh-shell/scripts/check-layout.sh` 退出 0。
真实会话交接（TTY+cosh-core）留 P6/Linux，focused 通过不能推断产品/发布验收。

## P4 验证记录（2026-11-14）

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| `guide` 全模块（含 4 个新增 P4 控制器测试） | 80 passed，退出 0 | `20261113T171722Z-6fbac17d` |
| `i18n` 完整性与尾段顺序 | 15 passed，退出 0 | `20261113T171738Z-4cd6d2b0` |
| `config::` 全量（含 shell_prefs 间接覆盖） | 110 passed，退出 0 | `20261113T171756Z-38b8e62f` |

新增 P4 测试：analysis 保存报新会话、强制关闭拒绝开启不持久化、导入切换保存偏好、仅清理保留开关。
修复只读导航因 `PersonalStore::open` 创建目录的回归（目录存在才读），`start_page…without_saving` 复跑通过。
`cargo fmt --all -- --check`、check-layout.sh 退出 0；session.rs 674 行、tips.rs 361 行，均 <700。
环境覆盖优先级、损坏恢复与导入关闭的完整矩阵留 P6 真终端/Linux 复核，focused 不能推断产品验收。

## P5 验证记录（2026-11-14）

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| `login::guide_protocol` 分类/版本/身份（query worker 为 Linux 条件） | 3 passed，退出 0 | `20261113T173219Z-a279c981` |
| `i18n` 完整性与尾段顺序 | 15 passed，退出 0 | `20261113T173220Z-4543f285` |
| `guide` 全模块 | 83 passed，退出 0 | `20261113T173221Z-04a8a928` |
| `runtime::invocation` 含新私有 worker 入口 | 47 passed，退出 0 | 见 P5 批次 |

macOS 宿主 `query_status` 仅 Linux 支持，query worker 测试加 `#[cfg(target_os="linux")]`，真实 sudo/账号验证留 P6/Linux。
`cargo fmt --all -- --check`、check-layout.sh 退出 0；session.rs 658 行、login.rs 108 行，均 <700。
未修改宿主账户/登录 Shell；提权协议仅单测与分类验证，不能宣称登录 Shell 产品验收。

## P6 验证记录（2026-11-14，代码/文档/focused 部分）

三入口统一：保留 /help 现有 HelpPanel renderer 与 registry 分组（不硬编码命令表），仅页脚补
`cosh guide` 入口提示；根帮助索引已含 guide/health/login 与会话内 /help//auth//recommendations//mode。
双语用户文档 overview 增补“配置向导与使用入门”，明确三入口职责、降级与 sudo/新登录边界，不夸大验收。

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| `i18n` 完整性与尾段顺序 | 15 passed，退出 0 | `20261113T173502Z-9e5a68cc` |
| `guide` 全模块 | 83 passed，退出 0 | `20261113T173512Z-7aec613b` |
| `runtime::invocation` | 47 passed，退出 0 | `20261113T173515Z-d0012a50` |
| 根帮助安装入口回归（raw-cli） | 1 passed，退出 0 | `20261113T173525Z-3f9862c8` |

`cargo fmt --all -- --check`、check-layout.sh 退出 0。
未完成（另行验证，不得从 focused 推断）：真终端浅色/深色/无色矩阵、x86_64/ARM64 发布验收；
真实 VNC、Linux 真实 sudo/账号与 x86_64 RPM 安装见下节真机记录。窄屏/resize/重定向/受限 TERM 已由 macOS PTY 与真机 VNC 覆盖。

macOS 真实 PTY 走查（临时 HOME、`COSH_SHELL_LANG=en-US`，2026-11-14）：Home/Tips 菜单/命令分析/个性化推荐/
默认登录 Shell 页均正常渲染；30×10 显示过小提示、恢复 80×24 复原；Esc/q 退出码 0；非 TTY 重定向与 `TERM=dumb`
均打印静态说明、不进入备用屏。推荐数据页与导入/清理由 focused 单测覆盖。浅色/深色/无色仍待复核；真实 VNC 见下节真机记录。

## P6 真实环境验证记录（2026-11-14，ALinux4 x86_64 真机 ECS）

环境：cn-hangzhou 临时 ECS（x86_64 ecs.g7.xlarge / ARM64 ecs.c6r.xlarge，ALinux4 `aliyun_4_*_20G_alibase_20261103`）。
官方 Rust 1.100.0 工具链与官方 crates.io 依赖缓存经 OSS 内网传入真机离线构建（全程未用镜像源）；
RPM 由未改动的 `scripts/rpm-build.sh cosh-ng` 产出 `cosh-ng-0.29.1-1.alnx4.x86_64.rpm` 并 yum 安装。
VNC 为 tigervnc-server + matchbox-window-manager + xterm，客户端用 vncdotool 走真实 RFB 协议截屏与按键。

| 验证 | 结果 |
| --- | --- |
| Linux 门控 `login::guide_protocol`（bin target，含 `query_worker`） | 4 passed，退出 0 |
| RPM 构建 + 安装 | 退出 0；`/usr/bin/cosh`、capability proof 与 preflight 就位；`/etc/shells` 登记 |
| 安装后 `cosh --help` | 退出 0；索引含 guide/health/login 与会话内 /help//auth//config//recommendations//mode |
| 安装后 `cosh guide` 非 TTY | 退出 0；打印静态说明，不进入备用屏 |
| 临时用户 `login set` | 退出 0；getent 由 /bin/bash 变 /usr/bin/cosh；status managed=yes |
| 临时用户 `login restore` | 退出 0；getent 恢复 /bin/bash |
| 真实 sudo worker query（临时文件协议） | 退出 0 且结构化 ok，身份/UID/shell 与 getent 一致 |
| 非 root worker set | 退出 0 但结构化 error（UID/euid 边界）；退出码 0 不作成功凭据 |
| ALinux3 边界 | status/set/restore 退出 1，提示仅支持 ALinux4 RPM（平台/降级边界证据） |
| 真实 VNC 交互 | Home/AI/语言/使用入门页渲染；↑↓/Enter/Esc/q 有效；/help 分组面板含 `cosh guide` 页脚提示 |
| VNC 降级（40×12、TERM=dumb） | 打印静态指南文本，不进入 TUI/备用屏 |
| ARM64 RPM 构建+安装+门控单测+set/restore | 退出 0；`cosh-ng-0.29.1-1.alnx4.aarch64.rpm`；4 passed；getent 复核 set/restore |

清理：临时账号已删除；ALinux3、ALinux4 x86_64、ALinux4 ARM64 临时 ECS 均已释放复核；OSS 传输对象已删除。
仍未完成（单列，不得由此推断）：正式镜像上架/发布操作；其余真机验收见下各节。
说明：VNC 截屏存在少量客户端解码黑块（vncdotool 编码伪影），不归因于 TUI；使用入门页经新开会话连续 ↓ 到达
（Esc 返回后的按键时序在 VNC harness 下不稳定，属 harness 限制，不作产品结论）。

### 发布验收包测试（2026-11-14，ALinux4 x86_64 + ARM64 真机）

环境：cn-hangzhou 临时 ECS x86_64 `i-bp15n4ew3p63f3tjjk5q`（ecs.g7.xlarge）/ ARM64 `i-bp15tj425p2kov64np6j`（ecs.c6r.xlarge），
ALinux4 `aliyun_4_*_20G_alibase_20261103`。源码与官方依赖缓存经 OSS 内网传入；四项包测试与 `scripts/` 均原样运行，未改代码或打包逻辑。
`test-package-raw.sh` 依赖仓库布局 `<repo>/src/cosh-ng` 与 `<repo>/scripts/build-all.sh`（校验统一入口拒绝 legacy `--uninstall`）；
首轮源码置于 `/root/cosh-ng`，`$ROOT/../../scripts` 物理路径落空致 raw=1，属环境布局问题而非产品缺陷；按仓库布局摆放后复跑通过。

| 包测试 | x86_64 | ARM64 |
| --- | --- | --- |
| tests/test-package-rpm.sh | 退出 0（%postun 矩阵 + scriptlet 通过） | 退出 0 |
| tests/test-package-raw.sh | 退出 0（仓库布局复跑） | 退出 0（仓库布局复跑） |
| tests/test-system-install.sh | 退出 0（prebuilt install 通过） | 退出 0 |
| tests/test_rpm_split.py | 退出 0（3 tests OK） | 退出 0（3 tests OK） |

调用：首轮双架构 `t-hz0731gj8e6zgu8` / `t-hz0731gj9zf5ssg`（raw=1，布局）；raw 布局复跑双架构 `t-hz0731hf23cw4qo`（raw=0）。
清理：两台验收 ECS 已 DeleteInstance 并复核；OSS `cosh-guide/release-accept-20261114/` 传输对象已删除。
仍未完成（单列，不得由此推断）：正式镜像上架/发布操作。

### 浅色/深色/无色矩阵（2026-11-14，ALinux4 x86_64 真机 VNC）

方法：真机 `i-bp1ih82fh5fpt3357h39`（ecs.g7.xlarge，ALinux4）。二进制为本次工作区源码经官方 Rust 1.100.0 + 官方 crates 缓存
离线 `cargo build --release -p cosh-shell` 产出；源码 tar 与 worktree `crates/`、`Cargo.toml`、`Cargo.lock` 逐文件一致，
未复用旧 RPM/制品，未改代码。VNC = tigervnc-server + matchbox-window-manager + xterm（800×480，Noto Sans Mono CJK SC 12pt）；
vncdotool 走真实 RFB 协议按键与截屏。三组终端：dark（黑底白字）、light（白底黑字）、mono（白底黑字 + `NO_COLOR=1` + `TERM=xterm-mono`）；
每组截 home / AI 详情 / 使用入门三页。

| 终端 | 结果 |
| --- | --- |
| dark | 文字与选中行（整行反色 + `>`）清晰；黄底行内代码、青色 bullet 为补充语义色 |
| light | 黑字白底清晰；选中行整行反色（黑底白字）；黄底行内代码黑字可读 |
| mono | 不依赖彩色：选中仍靠反色 + `>`；行内代码高亮退化为纯文本，内容完整可读 |

调用：构建 `t-hz0731ibde6jny8`（BUILD_RC=0）；VNC/字体 `t-hz0731itb7cheyo` / `t-hz0731j72sv8cg0`；三组走查 `t-hz0731j9jwad0jk`（9 张截屏）。
说明：浅色/无色截屏顶部黑带为 vncdotool 客户端解码伪影（深色下不可见），不归因 TUI；首轮 480×340 未装 CJK 字体致中文豆腐块，
装 Noto CJK 后消除（环境字体问题，非产品缺陷）。
观察（单列、本轮不改代码）：低列数下使用入门长行按列截断而非换行（约 45–66 列），属窄屏排版行为，留产品后续评估；颜色矩阵本身通过。
清理：验收 ECS 已 DeleteInstance 并复核；OSS `cosh-guide/color-matrix-20261114/` 传输对象与截屏已删除。
仍未完成（单列，不得由此推断）：正式镜像上架/发布操作。

### 发布/镜像集成与真实 provider 认证交接（2026-11-14，ALinux4 双架构真机）

发布制品：本次工作区源码经未改动 `scripts/rpm-build.sh cosh-ng` 产出 `cosh-ng-0.29.1-1.alnx4.x86_64.rpm`
（sha256 `1e2daea2ab4f3ea14c5fdb365517d2721846357a3b0898163f2b822f7ae7a1fc`）与 `cosh-ng-0.29.1-1.alnx4.aarch64.rpm`
（sha256 `9d53f0c3edae96404540d0b1536be07611b1d41a1f5d41958cf6cf4854a904d9`）及 src.rpm。
真机 `i-bp11ptfsibohitusqyld`（x86_64）/ `i-bp104w4cs9rfkbm2q93p`（ARM64）。

镜像集成（以镜像 owner 身份安装未改动资产）：

| 检查 | x86_64 | ARM64 |
| --- | --- | --- |
| RPM 安装 | 退出 0 | 退出 0 |
| RPM 单独安装不改 /etc/bashrc 与 /etc/profile.d | 是 | 是 |
| owner 装 profile.d/cosh.sh + bashrc 保护 include 后干净登录 | 登录引导横幅（Health check + Type cosh guide for help），SEEN=1 | 同 |
| 冲突登录（用户 ERR trap + PROMPT_COMMAND） | 用户 trap/PC 完整保留，Cosh 退让不覆盖 | 同 |

真实 provider 认证交接（x86_64，免配置自动接入，无需 /auth、凭证或挂角色）：
- guide AI 页：Current AI service: Not configured；Zero-config auto use: on。
- Enter「Configure AI service (opens /auth)」交接进入真实 cosh-core 会话，Provider Management 显示
  `[1]* [active] 操作系统控制台（免配置接入） auto`（中文渲染正常）。
- 交互终端 `cosh --ask 'Reply with exactly: COSH-REAL-PROVIDER-OK'` → Agent 面板返回 `COSH-REAL-PROVIDER-OK`（真实模型回答）。
- 非 TTY `cosh --ask` 正确拒绝（exit 2，requires an interactive terminal）。

调用：构建 `t-hz0731lwb7xss1s` / `t-hz0731lwe5uzwn4`；集成 `t-hz0731mjobrxwxs` / `t-hz0731mjr3bec5c`、激活复测
`t-hz0731mq4jzlse8` / `t-hz0731mq7hrsw00`；交接/问答 `t-hz0731mqaj0v75s` / `t-hz0731mvzjnc9vk`。
说明：PTY 探针首轮回显竖排为未设窗口尺寸的 harness 限制，设 80×24 后横幅正常；截屏黑带为 vncdotool 解码伪影。
清理：两台 ECS 已 DeleteInstance 并复核；OSS `cosh-guide/final-accept-20261114/` 传输对象与截屏已删除。
仍未完成（单列，不得由此推断）：正式镜像上架/发布操作（发布运维步骤，非本次测试范围）。

## 首批验证记录（2026-11-13）

证据在工作区根 `.cosh-lab/runs/<运行编号>/` 的 `result.json`、`commands.log` 和 `report.md`。
表中的退出码是底层测试进程退出码；失败基线不会计入通过数量。

| 验证 | 结果 | 运行编号 |
| --- | --- | --- |
| 原内部 `cosh-shell --help` 基线 | 1 passed，退出 0 | `20261113T154651Z-2a71a953` |
| 新安装入口回归，修复前 | 1 failed，退出 101；子进程错误为内部 Shell 不存在、退出 127 | `20261113T154744Z-97d9a85c` |
| 新安装入口回归，最终修复后 | 1 passed，退出 0；含无配置、中/英偏好、环境覆盖、FIFO 回退和无写入四组场景 | `20261113T155324Z-4a02e073` |
| `runtime::invocation` 参数与入口回归 | 47 passed，退出 0；库 target 为 0 命中，不计入通过 | `20261113T155229Z-ee608476` |
| `i18n::tests::all_messages_have_en_and_zh_values` | 库与二进制各 1 passed，退出 0 | `20261113T155316Z-29ca851a` |
| 既有内部帮助，最终回归 | 1 passed，退出 0 | `20261113T155406Z-3232bcae` |

`cargo fmt --all -- --check`、`bash crates/cosh-shell/scripts/check-layout.sh`（均在 src/cosh-ng）、
`git diff --check`（仓库根）退出 0。原有 raw_cli/external_hook 两条 dead_code 警告未改。
过程中新增 i18n 尾段触及宏递归上限，已仅提高 lib/main 上限，保留 MessageId 编号；
新增测试曾使 invocation.rs 超过 700 行，已拆到所属私有测试文件，布局复检通过。
设计与生产改动分别做了只读评审，四项文档问题及一项 `--context` 文案问题已修正并复核。

`test plan` 因文档路径未分类给出 crate wider gate；本次仅执行上述 host focused 检查。
crate/workspace、Linux PTY、真实 VNC/provider/sudo、RPM 和发布 gate 均未运行，不能从 focused 通过推断。
未创建临时云资源、未修改宿主账户、未提交或发布；worktree 保留用于后续阶段。

## PR 评审修正（2026-11-15）

本轮替换此前 v1 worker 实现，旧临时文件 sudo 证据不适用于 v2。
v2 请求移除结果路径，限制 4096 字节，并将账号绑定到 `SUDO_UID` 和双向 NSS 查询；
父进程预先打开结果 FD，结果限制 16384 字节。向导 worker 不选择其它账号，其入口仍交给已有 login CLI。
平台测试分别检查实际状态或结构化拒绝，包含 Ubuntu 不支持 fixture，不再假设所有 Linux 均为 ALinux4。

仅清理增加独立确认页，默认取消；配置不可读时默认焦点为隐私说明。
其它用户入口的可执行文件定位/启动失败增加双语提示。
仅交互 guide 初始化已有私有日志，失败日志只记分类；静态 help/plain 不初始化日志。
认证子进程会在 `run_tui` 重装终端 handler，继承忽略只覆盖启动间隙，不持续整个子会话。

新增共享配置/视图测试通过 `library_tests!` 仅编译到库，重叠上限仍为 744。
终端信号恢复与实际屏幕生命周期协调，覆盖跨线程退出归属。
当前验证证据见 PR 逐条回复；上述历史 focused/RPM 记录不等于本轮源码通过，发布/provider 验收仍单列。

## 复跑入口

先按测试技能设置 PYTHONPATH 指向其 `scripts/cosh-lab/src`；以下命令在仓库根执行。
将 `COSH_TEST_WORKDIR` 设为当前 worktree 下 src/cosh-ng 的绝对路径；过滤器必须采用下述完整测试名。

```bash
python3.12 -m cosh_lab doctor --output json
python3.12 -m cosh_lab test plan --base 1300cfcd50938af9323c0c5df4faaaf00ff6be60 --output json
python3.12 -m cosh_lab test run --on host --scope unit --filter runtime::invocation --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope unit --filter i18n::tests::all_messages_have_en_and_zh_values --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope raw-cli --filter startup::installed_cosh_help_is_readonly_localized_and_independent_of_shell --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope raw-cli --filter startup::help_flag_writes_to_stdout_not_stderr --workdir "$COSH_TEST_WORKDIR" --output json
```
