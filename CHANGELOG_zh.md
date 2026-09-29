# 变更日志

[English](CHANGELOG.md)

本文件记录项目所有值得注意的变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，项目遵循[语义化版本](https://semver.org/lang/zh-CN/spec/v2.0.0.html)。

## [1.4] - 2026-09-10

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.12.0 |
| agentsight | 0.12.1 |
| tokenless | 0.8.1 |
| agent-memory | 0.2.7 |
| os-skills | 0.6.3 |
| anolisa | 0.3.11 |
| skillfs | 0.4.2 |
| ws-ckpt | 0.4.5 |
| cosh-ng | 0.24.1 |

> **说明：** copilot-shell、os-skills、skillfs 与 ws-ckpt 自 v1.3 起未更新；版本表保留这些组件以展示完整组件组合。

### 重点特性

- **cosh-ng**：更新到 v0.24.1，在 Enhanced Assisted Zsh 中将自然语言和路径类输入交给 Agent，同时保留普通 Shell 行为与用户的终端定制，用户无需重建现有配置即可获得更自然的 Zsh 交互（#3004、#3156）
- **agent-sec-core**：更新到 v0.12.0，使 legacy cosh 与 cosh-ng 的 Skill 调用一致接受已配置策略校验，并降低误报，用户可在两种 Shell 中获得一致的 Skill 策略执行与更少的错误安全告警（#2871、#2928）
- **agentsight**：更新到 v0.12.1，新增无 eBPF 采集、DashScope 原生支持、Kubernetes 打包和更可靠的长期采集，运维人员可在受限环境与生产环境中持续获得准确的可观测数据（#2954、#2976、#3135、#3147）
- **tokenless**：更新到 v0.8.1，新增 QwenPaw 支持，以及大型 JSON、构建日志和 CSV/TSV 的可恢复压缩，Agent 可压缩更多类型的上下文并按需精确取回省略内容（#3047、#3052、#3067、#3075、#3089）
- **anolisa**：更新到 v0.3.11，新增 QwenPaw Adapter 管理，并可在已配置仓库提供 ktuner 时发现该组件，同时改进安装、恢复和审计行为，管理员可获得更可信的生命周期状态，并可在无需 root 的情况下预览组件索引（#2084、#3075、#3127、#3141、#3142、#3158）
- **agent-memory**：更新到 v0.2.7，改进 OpenClaw 安装、语料检索、配置校验和源码构建准备，Agent 可通过更可靠的安装和召回能力共享持久化记忆（#2560、#3149、#3155、#3177、#3187）

### 组件更新

- **cosh-ng**：更新到 v0.24.1，改进提示词路由、终端重绘、历史召回、自定义 Tab 行为、登录 profile PATH 加载、凭据脱敏和 Hook 信任信息，并移除系统 OpenSSL 依赖，用户可保留现有 Shell 配置并稳定使用 Agent 协助，不会丢失命令、输出或敏感历史保护（#2967、#2983、#2996、#2999、#3004、#3030、#3050、#3058、#3156）
- **agent-sec-core**：更新到 v0.12.0，在 legacy cosh 与 cosh-ng 中执行相同的 Skill 策略，降低扫描误报，通过 `skill-ledger` 与 `check` 报告全部完整性结果，并加快容器健康检查，用户可获得一致执行、更清晰的校验结论和更快的就绪检查（#2707、#2871、#2879、#2928）
- **agentsight**：更新到 v0.12.1，新增无 eBPF 采集、DashScope 原生与 Kubernetes 部署支持、Agent 资源监控和 cosh-ng 流量采集，并改进归因准确性、工具结果保留、策略同步、Token 计量、流式请求收尾、重复记录抑制、启动恢复和采集自动恢复，运维人员可信任时间线与用量统计，并让监控和防护在长期负载及短暂故障后持续运行（#2916、#2954、#2976、#2979、#3005、#3011、#3016、#3081、#3087、#3135、#3147、#3191）
- **tokenless**：更新到 v0.8.1，新增 QwenPaw 集成、大型 JSON 集合、构建日志和 CSV/TSV 输出的可恢复缩减、通过现有 Shell 工具取回省略内容以及 Trace 关联，并修复各支持宿主的安装与兼容问题，Agent 可对更多工具结果减少上下文占用，并在需要时恢复精确原文（#2249、#3009、#3047、#3052、#3067、#3068、#3075、#3085、#3089、#3094）
- **anolisa**：更新到 v0.3.11，新增 QwenPaw Adapter 生命周期管理和从已配置仓库发现 ktuner，允许已声明 config 文件修改而不产生错误损坏告警，正确记录 Hook 修改后的文件，避免包冲突后产生虚假 pending 操作，支持无需 root 的组件索引预览，并改进清理、日志、dry-run 和审计指引，管理员可依据更贴近系统实际状态的信息检查和恢复安装（#2084、#2618、#2922、#2926、#2994、#3075、#3118、#3127、#3141、#3142、#3158）
- **agent-memory**：更新到 v0.2.7，协商 OpenClaw 安装能力，区分权限与配置故障，返回可读取且窗口范围准确的语料结果，在查找二进制前校验配置，并为源码构建准备 Node.js 与 npm 依赖，Agent 与运维人员可获得更可靠的安装、诊断和记忆召回（#2560、#3149、#3155、#3177、#3187）

### 兼容性

- **Tokenless Protocol v2**：`tokenless compress` 现在仅接受 `before_model`、`pre_tool`、`post_tool` 和 `retrieve` 生命周期请求；Protocol v1 与 `tokenless mcp serve` 已移除。应同步升级 Tokenless Core、CLI、SDK 与 Adapter，并显式声明恢复能力，避免协议或检索能力不匹配（#2978、#3068）。
- **Tokenless Rust API**：使用 Runtime 生命周期方法替代 `TokenlessRuntime::compress`。直接调用响应压缩的代码须从已移除的 `tokenless-pipeline` Crate 和 `tokenless_schema::ResponseCompressor` 迁移至 Runtime 或 `tokenless-compressors` API（#2974、#2978）。
- **Tokenless Python 与 AgentScope API**：使用类型化的 `before_model`、`pre_tool`、`post_tool` 和 `retrieve` 生命周期请求，替代已移除的 `ModelRequest`、`ToolCall`、`ToolResult` 和 `ToolResponseCompressor` 类型。AgentScope 集成须为自定义工具补充 `ToolContract` 元数据，并暴露一个静态恢复工具（#2986、#3029）。
- **Tokenless 恢复配置**：自定义 `retrieve_tool_name` 必须符合工具名规则。生命周期 Schema 压缩现在要求具备已授权的静态恢复工具和可用 Stash；缺少任一条件时，Hook 会保留原始 Schema。取回省略内容还要求 CLI 或静态工具可访问，且宿主能够替换当前工具结果，其中 Claude Code 要求 2.1.121 或更新版本。已有 `<<tokenless:HASH>>` 标记仍可读取（#2978、#2995、#3029、#3052、#3068）。
- **Tokenless Adapter 配置**：OpenClaw 现在使用 `post_tool_enabled` 控制 PostTool 优化；原有的响应、TOON、skip-tool 和 shell-tool 策略设置已移除，不再控制压缩行为。升级时应删除这些废弃设置，并按需配置 `post_tool_enabled`。DeepSeek Harness 用户也须移除 Adapter 专用阈值和工具列表，改用共享压缩策略。OpenClaw 仍仅支持无损 transcript 更新，不支持同轮替换或检索（#3009、#3036）。
- **Tokenless 统计字段**：Dashboard 和集成须使用 `content_origin`、`applied_operations` 与 `recoverability` 替代 `seam` 和 `compressor_chain`。旧 SQLite 列仍为兼容性保留，但新记录不再写入这些列；升级后继续查询旧字段可能得到空值或错误报表（#2978）。
- **AgentSight 时间戳**：内核事件时间戳改为校准至主机墙钟时间，不再基于容器 uptime 计算。若内核事件仍在队列中时 NTP 或管理员跳变主机墙钟，受影响事件的历史墙钟时间将无法重建；已持久化记录不受影响。跨越墙钟调整分析时间线时，应标记每次调整点（#3128）。
- **agent-sec-core Python 依赖**：Python 部署和源码构建现在要求 `cryptography` 50.0.1 或更新版本。自定义安装在升级前应根据导出的 requirements 刷新环境（#3015）。
- **agent-memory 源码构建**：从源码构建 OpenClaw Adapter 现在要求 Node.js 20 或更新版本以及 npm。统一用户模式构建会自动准备这些工具；系统模式构建会在预检时报告缺失工具（#3187）。
- **anolisa RPM 预览**：在 DNF 4 系统上，RPM 安装的 `--dry-run` 需要使用 `sudo`，并可能在检查依赖冲突时刷新仓库元数据。该检查现在会在创建恢复日志前完成（#3158）。
- **anolisa 可编辑 config 与状态格式**：允许修改 raw 安装的 `type = "config"` 文件内容不会改变 update 或 uninstall 语义，因此执行任一操作前都应备份已定制配置。新的 `kind = "config"` 状态记录无法由旧版 CLI 读取；降级前还应备份状态，并将相关自有文件条目改回 `kind = "file"`，以恢复内容摘要校验（#3142）。

## [1.3] - 2026-08-31

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.11.1 |
| agentsight | 0.11.2 |
| tokenless | 0.7.14 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.3 |
| anolisa | 0.3.8 |
| skillfs | 0.4.2 |
| ws-ckpt | 0.4.5 |
| cosh-ng | 0.22.2 |

> **说明：** copilot-shell 与 agent-memory 自 v1.1 起未更新；版本表保留这两个组件以展示完整组件组合。
>
> **说明：** agent-sec-core 采用发布分支流程，`main` 仍显示 0.11.0；1.3 组件栈的实际发布物取自 `sec-core/v0.11.1` tag，下文条目描述的是该 tag 上的行为，而非 `main` 上的行为。

### 重点特性

- **cosh-ng**：更新到 v0.22.2，新增 Native Shell 集成模式与 `Shift+Tab` Shell-only 切换，终端输出以卡片前缀区分归属，用户可使用无 Hook 的 Shell，同时一眼看出每行输出来自哪个子系统（#2759、#2832）
- **agent-sec-core**：更新到 v0.11.1，用 Rust 重写提示词扫描内核，引入可更新规则包与可选的深度分析后端，同时收窄不可见字符规则，用户获得更快的提示词扫描，合法的 emoji 与多语言提示词不再被误判为严重注入（#2409、#2531、#2699、#2900）
- **agentsight**：更新到 v0.11.2，模型流量抓取失效后可自行恢复，并支持识别 Bun 构建的 Claude Code，用户无需重启采集器即可保持连续观测（#2782、#2792）
- **tokenless**：更新到 v0.7.14，新增统一的 `tokenless compress` 入口，`stats summary` 补齐净节省与 Retrieve 归因，Adapter 最多只发起一次子进程调用，用户可看到估算的净 Token 节省（#2844、#2885）
- **ws-ckpt**：更新到 v0.4.5，新增 k8s Sidecar 部署（#2034、#2965）与受保护的 checkpoint 协议（快照按存储身份围栏），用户可在容器化工作区做快照，并在崩溃后校验 checkpoint 状态
- **skillfs**：更新到 v0.4.2，新增 Kubernetes Sidecar 部署与可选的双向 HMAC-SHA256 认证保护 control 与 notify socket，非特权工作负载可跨容器 namespace 使用 FUSE Skill View（#2057、#2449）

### 组件更新

- **cosh-ng**：更新到 v0.22.2，新增通过 `cosh agent task|doctor|run` 暴露的本地 Gateway 控制面、有界的 transcript 内存与 32 MB `run_command` 输出上限、亚毫秒级交互回显、包管理目录外系统扩展的自动发现，以及 `/hooks enable|disable` 的层级消歧；修复终端显示与输入路由（已批准命令与斜杠命令执行后终端残留内部标记行、批量粘贴的斜杠输入误路由、含路径的中文提示词、斜杠命令历史召回、中断后终端滞留 raw 模式）、安全与审计缺口（trust 模式下被 Hook 拦截的命令仍被执行、审批批次竞态、Hook 输出畸形时工具调用被静默放行、中断的 `precmd` 标记退出码被伪造）以及打包问题（RPM 卸载残留悬挂登录 Shell、systemd 255 上 Gateway 启动失败、`dnf --dry-run` 误报、代码扫描漏检 awk `system()` 调用），用户获得输出归属可见、内存有界且审批可审计的原生 Shell（#2125、#2400、#2402、#2405、#2529、#2599、#2603、#2605、#2622、#2655、#2667、#2682、#2709、#2843、#2880、#2909、#2914、#2917、#2918、#2938、#2943、#2949、#2955、#2968）
- **agent-sec-core**：更新到 v0.11.1，新增 SkillFS HMAC 对端认证、`agent-sec-cli capabilities` 子命令以及 `verify` 的 `CHECKED`/`PASSED`/`FAILED` 显式计数；只读系统 Skill 不再让批量扫描失败，占位的 `set-policy`/`rotate-keys` 不再虚报成功，daemon 健康检查不再夸大就绪状态，非回环模型服务地址不再被接受，用户可在跨容器部署中审计 Skill 并信任 CLI 的校验结论（#2356、#2493、#2875、#2876、#2892、#2893、#2906）
- **agentsight**：更新到 v0.11.2，新增历史 Agent 活动视图、会话语义搜索、双语 Dashboard、LLM 延迟指标与存储大小上限；修复模型流量抓取失效后无法自行恢复、采集器因内存占用被终止后不自动重启、事件突增时内存不受控，以及中断事件分组计数与总数不一致，用户在长时间运行中获得存储有界、抓取可自愈的可观测能力（#2578、#2612、#2644、#2733、#2792、#2796、#2817、#2925）
- **tokenless**：更新到 v0.7.14，新增提供框架中立生命周期的 `anolisa-tokenless` Python Wheel、AgentScope 与 DeepSeek Harness 集成、Gemini `functionDeclarations` Schema 压缩以及可配置的数组尾部窗口；修复 Codex 集成重复压缩与小负载 TOON 处理不一致，更多框架上的 Agent 可节省 Token，并通过截断标记内嵌的可执行命令恢复被截断内容（#2433、#2507、#2581、#2627、#2663、#2866、#2869、#2885）
- **anolisa**：更新到 v0.3.8，新增 Linux x64/arm64 与 macOS arm64 的已验证预编译 CLI 归档、原生 DSH Adapter 驱动、容器运行时 Telemetry 与基于 schema v2 的目标可用性判定；修复 raw 安装误展开渲染内容中的 `${VAR}`、`--quiet` 下 Adapter 输出不干净、`--dry-run` forget 与 restart 预览失真，以及卸载后 systemd 模板实例仍在运行，用户可按平台安装独立 CLI，并在无副作用的前提下预览操作（#2533、#2580、#2603、#2642、#2752、#2762、#2774、#2883、#2903）
- **os-skills**：更新到 v0.6.3，RPM 补充 `anolisa-component(os-skills)` 声明，仓库侧组件索引不可用时用户仍可执行 `anolisa upgrade` 升级 OS Skills（#2576）
- **ws-ckpt**：更新到 v0.4.5，新增 k8s Sidecar 部署（含中英文指南，#2034、#2965）与受保护的 checkpoint 协议；修复 daemon 内存泄漏最终耗尽内存导致进程被终止（#2554）、并发 IO 下 loop 设备后端 checkpoint 延迟（最多降低至原来的 1/5，#2523）、bootstrap 失败遗留悬空镜像与 loop 设备及启动失败静默退出（#1956）、`config --global` 写入未被 daemon 实际加载（#2813），以及 loop 设备全部被占用时的间歇性 bootstrap 失败（#2965），用户可在容器中以更低延迟做快照并获得明确的启动诊断
- **skillfs**：更新到 v0.4.2，新增 Kubernetes Sidecar 部署、双向 HMAC-SHA256 socket 认证、可选的 Alibaba Cloud Linux 4 Sidecar 镜像，以及启动 reconcile 对晚启动 notify daemon 的有界退避重试；修复 flat normal 模式挂载下分类 Skill 无法被找到，非特权工作负载可使用经认证且在 daemon 重启后自动收敛的 Skill View（#2057、#2449、#2777、#2787、#2790、#2901）

## [1.2] - 2026-08-14

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.10.1 |
| agentsight | 0.10.1 |
| tokenless | 0.7.6 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.2 |
| anolisa | 0.2.19 |
| skillfs | 0.4.0 |
| ws-ckpt | 0.4.2 |
| cosh-ng | 0.16.1 |

> **说明：** copilot-shell、agent-memory、skillfs 与 ws-ckpt 自 v1.1 起未更新；版本表保留这些组件以展示完整组件组合。

### 重点特性

- **cosh-ng**：更新到 v0.16.1，将一次性 Agent 请求统一到 `/agent`，并收敛 cosh-core 与 cosh-shell 运行时路径、引入显式协议协商，用户用一个命令即可发起单次 Agent 请求，两条运行时入口行为一致（#2403、#2441）
- **agent-sec-core**：更新到 v0.10.1，统一各 Agent 框架的 Hook 策略开关，代码扫描、提示词扫描与可观测性均可独立由环境变量控制，用户无需改动 Hook 脚本即可按部署启用各项防护（#2141、#2199、#2239）
- **agentsight**：更新到 v0.10.1，修正回合边界与 cosh 重启后的会话连续性，将暂停事件重新归类为正常结束而非异常中断（#2320），并新增 Codex 轨迹转换与跟随浏览器语言的 Dashboard，用户可获得准确的跨运行时轨迹并以本地语言查看
- **tokenless**：更新到 v0.7.6，新增 OpenCode Adapter，并将 Qoder Adapter 迁移到原生插件与 Hook 机制，两种运行时上的 Agent 都能获得命令重写以及就地替换原始工具输出的 Schema/响应压缩
- **anolisa**：更新到 v0.2.19，新增 raw 安装的包体系后端映射、更新后的 Adapter 变更提示与 2 GiB 完整性降级判定，管理员可在精简 RPM/DEB 主机上安装，大体积组件不再被误判为损坏（#2018、#2271、#2314）

### 组件更新

- **cosh-ng**：更新到 v0.16.1，新增带跨目标构建校验与可移植 macOS 启动器的 raw 打包接口；修复时钟跳变导致的输入停滞、流式响应解码不严格、敏感文件写入、退出后残留 raw 模式、临时文件路径可预测、斜杠命令只提示首个匹配项与 CJK 折行，用户获得可复现归档，以及能正确折行东亚文本且不残留终端状态的 Shell（#2176、#2209、#2211、#2357、#2361、#2410、#2411、#2446）
- **agent-sec-core**：更新到 v0.10.1，新增 OpenClaw 代码扫描拦截模式、更广的提示词扫描入站字段覆盖、只读 Skill 分析、把未打包的 Skill 目录纳入账本检查、加载 Skill 包前先验证清单签名，以及事件查询的会话与运行过滤，用户可拦截风险代码、检查未打包 Skill 并按会话查询安全事件（#2044、#2132、#2185、#2201、#2242、#2277）
- **agentsight**：更新到 v0.10.1，新增 Codex 轨迹转换为 ATIF、抓取到的模型流量附带进程归属信息且进程号在观测者命名空间内解析（#2360）与 Dashboard 本地化；修复工具调用结束后回合被提前关闭、暂停事件被误判为异常中断（#2320）、流式响应被截断、QwenCode 轨迹数据不准、cosh 重启后会话丢失，以及 cosh 会话临时文件写入未被映射（#2080），用户可在浏览器语言环境下获得准确的跨运行时轨迹
- **tokenless**：更新到 v0.7.6，`TOKENLESS_DATA_DIR` 支持用户 Home 之外的绝对目录，硬关闭 Tool Ready 调用前检查与阻断；修复 JSON Schema 被重复 Stash、Dry-run 配置被环境变量覆盖与 `retrieve` 额外添加换行，Agent 一次 Retrieve 即可恢复内容，且不再被错误的就绪判定阻塞（#2380、#2386、#2396、#2399、#2425、#2434、#2487）
- **anolisa**：更新到 v0.2.19，新增 `anolisa update` 后的 Adapter 变更提示、Qoder 原生插件生命周期支持、Codex Hook 信任持久化、`OPENCLAW_STATE_DIR` 处理与遗留命令的标准 JSON 信封，并将 Telemetry 迁移到 `SLS_PROJECT_PREFIX`，用户可跨框架管理 Adapter，并以同一方式解析所有 JSON 输出（#2018、#2221、#2260、#2281、#2319、#2337）
- **os-skills**：更新到 v0.6.2，新增用于确定性内核诊断、调优与回滚的 `ktuner` 技能，移除遗留的 OpenClaw 与 Hermes 适配器脚本，并补齐技能账本的认证恢复说明，用户可获得基于规则的调优建议并一键应用与回滚（#1172、#1278、#2185）

## [1.1] - 2026-08-08

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.9.0 |
| agentsight | 0.9.1 |
| tokenless | 0.7.3 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.1 |
| anolisa | 0.2.15 |
| skillfs | 0.4.0 |
| ws-ckpt | 0.4.2 |
| cosh-ng | 0.14.0 |

> **说明：** os-skills 保持 v0.6.1，本次发布未更新；版本表保留该组件以展示完整组件组合。

### 重点特性

- **cosh-ng**：更新到 v0.14.0，新增可恢复的 Workspace Session、MCP 管理、运行时状态查询和 DashScope Prompt Cache，Agent 可恢复长时间任务、扩展能力并降低重复 Prompt 成本（#1546、#1592、#1778、#1949、#2046）
- **agentsight**：更新到 v0.9.1，新增优化与 Trajectory 分析以及 Case Containment、System Audit 和 ActPlane 风险执行，用户可诊断 Agent 质量与成本并调查、遏制风险行为（#1728、#1789、#2051）
- **agent-sec-core**：更新到 v0.9.0，将 Prompt、PII、Code 和 Observability Hook 扩展到 Qoder CLI、Qwen Code 和 Codex，用户可在受支持的 Agent Runtime 间应用一致的安全策略（#1473、#1480、#1495、#1501、#1529、#1535）
- **tokenless**：更新到 v0.7.3，新增带 MCP 检索的可逆压缩以及 Cosh-NG 响应与命令压缩，Agent 可减少 Model Context 并按需恢复被截断的内容（#1285、#1376、#1669）
- **anolisa**：更新到 v0.2.15，新增精确版本 RPM/Raw 安装、文件元数据修复和交互式进度，管理员可选择已发布版本、修复安装漂移并查看操作阶段（#1700、#1740、#1987、#2036）

### 组件更新

- **copilot-shell**：更新到 v2.8.0，新增需用户同意的 `/ktuner` 命令、导出 `COSH_SESSION_ID` 并在切换时复用兼容的 cosh-ng 认证，用户可调优主机、关联子进程活动并以更少配置在 Shell 间切换（#1279、#1491、#1951）
- **agent-sec-core**：更新到 v0.9.0，新增 Qoder CLI 与 Qwen Code Hook 覆盖、Codex PII 与 Observability Hook、自定义 PII 规则及中文 Prompt Injection 检测，用户在 Prompt、Tool Call、Skill 和 Agent 输出上获得更广泛的保护（#1473、#1495、#1501、#1522、#1554）
- **agentsight**：更新到 v0.9.1，新增 ATIF v1.7 Trajectory 分析、准确性/性能/成本 Workspace、Case Containment、System Audit 和风险 Dashboard，用户可追踪多 Agent 行为并处理优化或安全发现（#1728、#1789、#1828、#2051）
- **tokenless**：更新到 v0.7.3，新增基于 Stash 的可逆压缩、MCP 检索服务器、Cosh-NG 压缩和 macOS/Qwencode Adapter 支持，Agent 可在更多 Runtime 中节省 Token 而不永久丢失压缩内容（#1285、#1376、#1669、#1894、#1964）
- **agent-memory**：更新到 v0.2.6，新增同步索引以及聚焦 Query 和 OR 排序 Recall Fallback，Agent 可从冗长或包含较多停用词的 Prompt 中检索刚捕获的记忆（#1520、#1574、#2047）
- **anolisa**：更新到 v0.2.15，新增精确版本 RPM/Raw 安装、Telemetry 控制、macOS arm64 npm 交付、文件元数据修复和分阶段进度，用户可跨 Linux 与 macOS 选择已发布版本、控制上报并修复 Linux 安装漂移（#1619、#1700、#1740、#1962、#1987、#2036）
- **skillfs**：更新到 v0.4.0，新增 Hermes 嵌套 Skill 兼容、可配置读取时转换、认证的 Live Source 解析和强化的权限边界，Agent 可使用适配后的 Skill View，同时 Source Mutation 仍受安全控制（#1146、#1484、#1517）
- **ws-ckpt**：更新到 v0.4.2，新增 Telemetry Gate 和孤立 Pre-init Backup 自动恢复，用户可在初始化中断后恢复 Workspace 而不受陈旧备份状态影响（#1509、#1601）
- **cosh-ng**：更新到 v0.14.0，新增 Session 恢复、MCP Tool、Slash Command 状态查询和 Prompt Cache 可观测，Agent 可恢复复杂任务、扩展能力并诊断 Cache 节省效果（#1530、#1546、#1592、#1778、#1949、#2046、#2075）

## [1.0] - 2026-07-06

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.6.1 |
| agent-sec-core | 0.7.0 |
| agentsight | 0.7.1 |
| tokenless | 0.6.1 |
| agent-memory | 0.2.1 |
| os-skills | 0.6.1 |
| anolisa | 0.1.20 |
| skillfs | 0.3.2 |
| ws-ckpt | 0.4.1 |
| cosh-ng | 0.11.0 |

### 重点特性

- **anolisa**：更新到 v0.1.20，交付统一 CLI 网关提供组件全生命周期管理与适配器编排，用户可通过一条命令安装/更新/诊断所有组件
- **cosh-ng**：更新到 v0.11.0，完成 Core/Shell 分离与 AI 增强终端，Agent 可跨发行版确定性执行结构化系统操作
- **agent-memory**：更新到 v0.2.1，新增用户数据主权与 4 类记忆分类，用户可查询/遗忘/控制自动捕获的记忆
- **tokenless**：更新到 v0.6.1，新增压缩开关与 A/B 对比及 QwenCode 适配器，用户可量化各策略的 Token 节省效果而不影响任务执行

### 新增组件

- **anolisa**：首次发布 v0.1.16，构建统一 CLI 网关管理组件安装/更新/卸载（RPM + Raw 双后端），用户可通过 `anolisa install --all` 一键部署全部组件
- **cosh-ng**：首次发布 v0.11.0，实现确定性 Agent-OS 接口（5 crate workspace），Agent 可通过稳定 API 跨发行版执行结构化系统操作
- **skillfs**：首次发布 v0.3.2，构建 FUSE 虚拟文件系统实现基于视图的 SKILL.md 暴露，Agent 可从挂载目录发现并加载技能

### 组件更新

- **agent-memory**：更新到 v0.2.1，新增主权工具集（about/forget/consent）、AMA 导入导出、4 类分类和抗 SIGKILL 增量聚合，用户可自主控制记忆留存并跨 Agent 迁移
- **tokenless**：更新到 v0.6.1，新增压缩开关（dry-run + 按模式统计）、SLS JSONL 遥测默认开启和 QwenCode 适配器，开发者可 A/B 测试压缩策略并在 SLS 大盘监控 Token 节省
- **agentsight**：更新到 v0.7.1，新增 Token 节省可视化（策略饼图 + 行级 diff）、安全大盘和容器/K8s 全面支持，用户可直观评估各优化策略的节省贡献
- **copilot-shell**：更新到 v2.6.1，新增 `/model` 多 Provider 切换对话框和 SLS 会话遥测（32 字段 JSONL），用户可自由切换 LLM Provider 而不丢失配置
- **agent-sec-core**：更新到 v0.7.0，新增 Skill Ledger 完整性链（GPG 签名工作流）和 Prompt Scanner，用户可审计 Skill 安全状态并在危险操作前收到确认提示
- **os-skills**：更新到 v0.6.1，新增 ANOLISA Guide 知识库 skill（13 份官方文档）和 OpenClaw 安装预检引导，Agent 可在回答中引用准确的产品文档
- **ws-ckpt**：更新到 v0.4.1，新增自动清理调度和 TOML 配置热重载，用户可设置保留策略并即时生效无需重启 daemon

### 变更

- 文档治理规范通过 `specs/documentation-standard.md` 建立
- 双语命名约定统一为 `_zh.md`（从遗留 `_CN.md` 迁移）

## [0.6] - 2026-06-12

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.4.1 |
| agent-sec-core | 0.5.0 |
| agentsight | 0.5.0 |
| tokenless | 0.4.1 |
| agent-memory | 0.1.0 |
| os-skills | 0.5.0 |
| cosh-ng | 0.1.0 (MVP) |

### 重点特性

- **agent-memory**：首次发布 v0.1.0，交付沙箱化文件系统 MCP 记忆服务器，Agent 可跨会话持久化存储并通过 BM25 检索上下文
- **tokenless**：更新到 v0.4.1，新增 Hermes Agent 插件和 Tool Ready 4 阶段预检，Agent 工具执行前自动验证环境就绪避免无效重试
- **agentsight**：更新到 v0.5.0，新增 Skill 维度 Token 指标和 Hermes 支持，用户可精确定位哪些 Skill 消耗最多 Token

### 新增组件

- **agent-memory**：首次发布 v0.1.0，构建 19 工具 MCP 服务器（命名空间隔离 + BM25 后台索引），Agent 可在沙箱化文件系统中读写/检索持久记忆
- **cosh-ng**：首次发布（MVP），完成确定性 OS 操作的生产可用功能，Agent 可获得格式可预测的结构化命令输出

### 组件更新

- **tokenless**：更新到 v0.4.1，新增 Hermes adapter runner 和 Tool Ready 机制（4 阶段环境预检集成为 cosh extension），Agent 工具调用前自动校验环境减少因环境故障浪费的 Token
- **agentsight**：更新到 v0.5.0，新增 Skill 维度 Token/调用指标和 Hermes matcher（含 SSL 支持），用户可在 Dashboard 中按 Skill 查看 Token 消耗明细
- **agent-sec-core**：更新到 v0.5.0，新增 PIIChecker（输出 PII 检测 + 脱敏引擎）和 Skill Scanner（文本/代码扫描 + 生命周期触发），Agent 输出中的敏感信息被自动拦截
- **copilot-shell**：更新到 v2.4.1，新增跨 Session 自动记忆提取和 hook reason UI 可见性，用户可看到安全 hook 拦截操作的具体原因

## [0.5] - 2026-05-28

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.4.0 |
| agent-sec-core | 0.4.0 |
| agentsight | 0.4.0 |
| tokenless | 0.4.0 |
| os-skills | 0.4.0 |

### 重点特性

- **tokenless**：更新到 v0.4.0，新增 Hermes 插件和 Tool Ready 环境机制，Agent 工具执行前依赖缺失被提前拦截避免 Token 浪费
- **agent-sec-core**：更新到 v0.4.0，交付 PIIChecker 和 Skill Scanner 首版，Agent 输出被扫描防止敏感信息泄露

### 组件更新

- **tokenless**：更新到 v0.4.0，开发 Hermes Agent 插件（Tool Ready 4 阶段环境预检 + History 压缩），Agent 运行时依赖在执行前被自动校验
- **agent-sec-core**：更新到 v0.4.0，新增 PIIChecker 输出 PII 检测和 Skill Scanner 基线能力，用户免受 Agent 无意泄露敏感数据的风险
- **agentsight**：更新到 v0.4.0，新增 Skill 维度指标展示，用户可按 Skill 查看 Token 消耗分组
- **os-skills**：更新到 v0.4.0，纳入 Nightly 自动化测试覆盖，Skill 质量持续验证

## [0.4] - 2026-05-13

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.3.0 |
| agent-sec-core | 0.4.1 |
| agentsight | 0.4.0 |
| tokenless | 0.3.0 |
| os-skills | 0.3.0 |
| ws-ckpt | 0.2.0 |

### 重点特性

- **agent-sec-core**：更新到 v0.4.1，建立 Skill 安全全生命周期管理（含 Prompt Scanner ask 策略），用户在 Agent 执行危险指令前收到确认提示
- **tokenless**：更新到 v0.3.0，搭建 4 套 Benchmark 对比基线，开发者可量化评估不同 Skill/OS 环境的 Token 消耗差异
- **ws-ckpt**：更新到 v0.2.0，扩展快照管理命令集，用户可按数量或时间维度自动清理历史快照

### 组件更新

- **agent-sec-core**：更新到 v0.4.1，集成 Prompt Scanner 至 cosh hook 和 OpenClaw 插件（ask 策略），用户在危险操作前获得交互式确认
- **tokenless**：更新到 v0.3.0，构建批量并发 Benchmark 平台并生成对比报告，开发者可一键跑分横向对比 Token 节省效果
- **agentsight**：更新到 v0.4.0，优化常驻进程内存占用，2C2G 小规格实例可稳定运行可观测服务
- **copilot-shell**：更新到 v2.3.0，适配 SWEBench 评测框架，开发者可通过 cosh 执行代码修复任务并验证通过率
- **ws-ckpt**：更新到 v0.2.0，丰富快照增删查能力，用户可按策略自动保留最近 N 份快照

## [0.3] - 2026-04-30

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.2.1 |
| agent-sec-core | 0.3.0 |
| agentsight | 0.3.1 |
| tokenless | 0.2.0 |
| os-skills | 0.3.0 |
| ws-ckpt | 0.1.0 |

### 重点特性

- **tokenless**：更新到 v0.2.0，交付命令重写和 TOON 上下文压缩，CLI 输出 Token 消耗降低 60–90%
- **agentsight**：更新到 v0.3.1，新增 Token 节省 Dashboard 和 Agent 异常诊断，用户可可视化节省趋势并检测 Agent 中断
- **agent-sec-core**：更新到 v0.3.0，新增 Skill Ledger 完整性追踪和 Prompt Scanner，每个 Skill 的签名链可端到端审计

### 新增组件

- **ws-ckpt**：首次发布 v0.1.0，构建基于 btrfs 的工作区快照守护进程，Agent 可毫秒级创建检查点并即时回滚文件系统状态

### 组件更新

- **tokenless**：更新到 v0.2.0，新增通过 RTK 的命令重写和 TOON 上下文压缩，Agent CLI 交互 Token 消耗减少 60–90%
- **agentsight**：更新到 v0.3.1，新增 Token 节省 Dashboard（Session/时间段统计）和 Agent 中断检测（drain 机制），用户可监控节省趋势并在 Agent 故障时收到告警
- **agent-sec-core**：更新到 v0.3.0，新增 Skill Ledger 全生命周期（check/certify/bypass/status/audit）和 Prompt Scanner 越狱检测，用户可追踪并强制执行 Skill 完整性策略
- **copilot-shell**：更新到 v2.2.1，新增 Extension 架构（command extension + system Hook + 即时激活）、Skill 市场对接和会话导出（Markdown/HTML/JSON），用户可通过插件扩展 cosh 能力并导出对话历史
- **os-skills**：更新到 v0.3.0，新增 Skill 市场上架和实用技能（xlsx/pdf-reader/image-gen/humanizer），用户可从市场发现并安装技能

## [0.2] - 2026-04-15

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.0.4 |
| agent-sec-core | 0.2.0 |
| agentsight | 0.2.2 |
| os-skills | 0.2.2 |
| tokenless | 0.1.0 |

### 组件更新

- **agentsight**：更新到 v0.2.2，新增 Token 消耗可观测（精确 Tokenizer 计量），用户可实时查看每条消息的 Token 明细
- **copilot-shell**：更新到 v2.0.4，新增独立鉴权（STS/ECS RAM Role）和 Skill 市场浏览，用户无需 AK/SK 即可认证并发现可用技能
- **os-skills**：更新到 v0.2.2，新增 SysAdmin 技能（Linux IO/网络/负载诊断），Agent 可独立诊断常见 OS 性能问题
- **tokenless**：首次发布 v0.1.0，构建 Skills 级 Benchmark 测试用例，开发者可跨 Skill 量化对比 Token 消耗

## [0.1] - 2026-03-30

### 组件版本

| 组件 | 版本 |
|------|------|
| copilot-shell | 2.0.1 |
| agent-sec-core | 0.1 |
| agentsight | 0.1 |
| os-skills | 0.1 |

### 新增组件

- **copilot-shell**：首次发布 v2.0.1，构建 AI 驱动终端助手（Tab 补全、/bash 模式、sudo、Hook 安全），用户开机即获得 AI 原生 CLI 交互体验
- **agent-sec-core**：首次发布 v0.1，交付 Skill 签名校验、安全沙箱和系统加固，Agent 操作在受控最小权限环境中运行
- **agentsight**：首次发布 v0.1，构建基于 eBPF 的零侵入可观测探针，用户无需修改 Agent 代码即可监控 LLM API 调用和 Token 消耗
- **os-skills**：首次发布 v0.1，整理系统管理、SysOM 运维、DevOps 和云技能库，Agent 可自主执行常见 OS 操作

### 安全

- Skill 全链路安全加密与数字签名
- 硬件级安全沙箱风险隔离
- Skill 调用身份认证与完整性校验

---

各组件详细变更日志请参阅：

**用户入口**
- [copilot-shell](deprecated/copilot-shell/CHANGELOG.md)
- [cosh-ng](src/cosh-ng/CHANGELOG.md)
- [anolisa](distribution/anolisa/CHANGELOG.md)
- [os-skills](src/os-skills/CHANGELOG.md)

**Token 节省**
- [tokenless](src/tokenless/CHANGELOG.md)

**运行时**
- [agent-memory](src/agent-memory/CHANGELOG.md)
- [skillfs](src/skillfs/CHANGELOG.md)
- [ws-ckpt](src/ws-ckpt/CHANGELOG.md)

**Agent 可观测**
- [agentsight](src/agentsight/CHANGELOG.md)

**Agent 安全**
- [agent-sec-core](src/agent-sec-core/CHANGELOG.md)
