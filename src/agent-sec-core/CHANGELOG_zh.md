# 更新日志

[English](CHANGELOG.md)

## 0.13.2

**构建与打包**

- 使 ANOLISA RPM 适配器安装能够发现分散在 agent-sec-core 子包中的 hook 与 skill 载荷。(#3480)

## 0.13.1

**OpenClaw Hook 集成**

- 更新 OpenClaw 插件部署与运行时兼容性以适配 OpenClaw 2.0，包括能力授权（capability consent）和基于 SQLite 的会话证据。(#3440)

## 0.13.0

**V2 策略与守护进程运行时**

- 新增 V2 daemon 的 Policy、Scope 与 Binding CRUD API，并施加仅 root 的授权。(#3062)
- 新增通过 daemon 进行 Policy、Scope 与 Binding 管理的 V2 CLI 命令。(#3097)
- 新增 AgentSight 策略适配器、客户端集成与 binding 对账。(#3103)
- 新增有界的并发对账 worker，单个 binding 失败不会阻塞 daemon。(#3194)

**V2 安全操作**

- 新增 V2 daemon 支撑的 Bash 与 Python 正则代码扫描器，输出兼容的结构化结果。(#3203)
- 新增 V2 `capabilities` 命令，提供与 V1 兼容的环境能力报告。(#3243)
- 为已接受的 V2 code-scan 事件新增 V1 形态的 JSONL 与 SQLite 持久化。(#3246)
- 为 V2 安全操作新增共享的扫描生命周期记录与 allowlist 内的遥测 sink。(#3300)

**Skill Ledger 运行时**

- 默认批量初始化与扫描期间跳过未托管且只读的 raw user Skill。(#3183)
- 围绕受支持的名称、初始化与策略，统一 Skill Ledger CLI 与配置输入。(#3253)

**构建与打包**

- 将 V2 daemon 作为加固的 systemd 系统服务交付。(#3217)

**技能**

- 新增内置 `pii-checker` Skill，用于 PII 与凭据扫描，支持可选脱敏。(#3241)

**测试与 CI**

- 为 CLI、daemon、插件与服务契约新增已安装 V2 RPM 的端到端覆盖。(#3162)

**文档**

- 新增 AARM 注册表与 OWASP Agentic Top 10 的安全控制映射。(#3227)

**维护**

- 扁平化 V2 crate 布局并更新内部引用。(#3319)

## 0.12.0

**cosh 与 Cosh-NG Hook 集成**

- 在 cosh Skill Ledger hook 中归一化 legacy cosh 与 Cosh-NG 的 Skill 调用载荷，使 Cosh-NG 调用遵循配置的策略，而不是绕过检查。(#2871)

**Skill Ledger 运行时**

- 收紧六条 Skill 扫描器检测规则，消除确定性误报，并将静态扫描器标识升级为 `cisco-static-only-0.1.1`。(#2928)
- 补全 `skill-ledger` 与 `check` 帮助输出中全部八种完整性状态及其生产环境触发条件。(#2707)

**守护进程运行时**

- 新增 POSIX shell 的 `daemon.health` 探针，容器健康检查不再依赖 Python 冷启动。(#2879)

**Rust v2 框架**

- 新增 v2 foundation 与 policy 类型 crate，作为 Rust 安全核心迁移的基础。(#3007)
- 新增 v2 daemon 服务框架，包含 UDS 服务端、请求分发与停机处理。(#3040)
- 新增 v2 PAP 服务，在仓储抽象之上提供 Policy、Scope 与 Binding CRUD。(#3027)

**构建与打包**

- 将 `cryptography` 运行时下限提升到 50.0.1，并刷新 lock 与导出的依赖清单。(#3015)

**文档**

- 新增当前 daemon 协议与 v2 框架需求的设计文档。(#2881)

## 0.11.1

**提示词扫描器**

- 收紧不可见字符注入规则，emoji ZWJ 序列、波斯语 ZWNJ 与开头的 BOM 不再被判为严重注入。(#2900)
- 在 Rust 与 Python 模型服务路径中，拒绝由环境变量派生的非回环模型服务 base URL。(#2893)
- 扫描改为进程内执行后，`daemon.health` 的 prompt_scan 状态如实报告为 untracked，不再无条件报告 ready。(#2892)
- 移除失效的提示词模型预加载环境开关与过时的 scan-prompt 协议文档。(#2886)

**Skill Ledger 运行时**

- 批量 `scan --all` 与默认 `init` 跳过宿主挂载的只读系统 Skill，而不是让其失败。(#2906)
- 拒绝占位的 `set-policy` 与 `rotate-keys` 命令，不再虚报成功。(#2876)

**资产校验**

- `agent-sec-cli verify` 报告显式的 `CHECKED` / `PASSED` / `FAILED` 计数，并为零候选结果提供独立输出。(#2875)

**安全事件与 CLI**

- 对 CLI 创建的文件强制 owner-only 权限。(#2873)

**文档**

- 记录五个隐藏的 `agent-sec-cli` 集成命令及其契约注意事项。(#2887)
- 记录仅规则驱动的 L1 检测边界，以及如何结合降级字段解读 `pass`。(#2895)

## 0.11.0

**提示词扫描器**

- 用 Rust 重写提示词扫描器核心，获得原生性能。(#2409)
- 新增 ATR（Agent Threat Rules）规则包并缩短编译时间。(#2531)
- 新增用于提示词扫描的 Warden-Gen L2 后端选项。(#2699)
- 将 Rust 原生扩展日志接入 Python logging 框架。(#2640)
- 修正 warmup 文档以描述模型可用性检查，并抑制 Codex、cosh 与 Qwen Code 提示词扫描 hook 中每次调用的扫描模式日志。(#2745)

**Hermes Hook 集成**

- 移除 Hermes 的合成警告注入；PII 与 Skill Ledger 策略改用原生的 observe/block 边界。(#2712)

**PII 检查器**

- 改进 PII 检测以减少 JWT 与 Email 误报，并在全部宿主适配器间统一用户提示。(#2443)

**Skill Ledger 运行时**

- 为跨容器 Skill Ledger 部署新增 SkillFS HMAC 对端认证。(#2493)

**安全事件与 CLI**

- 新增 `agent-sec-cli capabilities` 子命令，用于基于环境的 hook 配置检查。(#2356)
- 将可观测性 hook 超时设为 10 秒，CLI 默认超时设为 5 秒。(#2346)
- 新增用于结构化事件与会话报告查询的安全可观测性 skill。(#2245)

**文档**

- 修正并重组用户文档，使其与 0.10.x 实际交付的行为一致。(#2456)

## 0.10.1

**OpenClaw 与 cosh Hook 集成**

- OpenClaw 与 cosh 的提示词扫描 hook 改为通过 stdin 传递提示词文本，不再放在命令行参数中。(#2445)

**安全事件与 CLI**

- 在 CLI 与 daemon 安全查询处理器中校验事件查询参数。(#2451)

## 0.10.0

**Agent Hook 策略控制**

- 新增 agent hook 的代码扫描器启用开关。(#2001)
- 统一各 agent 集成之间的 hook 策略控制。(#2141)
- 新增可观测性 hook 的环境开关。(#2199)
- 恢复扫描器模式的环境变量名。(#2212)
- 在受支持的 agent 集成之间对齐代码扫描 hook 开关。(#2229)
- 新增基于环境的提示词扫描器门控。(#2239)

**OpenClaw Hook 集成**

- 为 OpenClaw 代码扫描 hook 新增 block 模式支持。(#2242)

**提示词扫描器**

- 扩大提示词扫描入站文本字段的覆盖范围。(#2277)

**Skill Ledger 运行时**

- 新增只读 skill 分析。(#2044)
- 将 raw skill 目录纳入 skill ledger 检查。(#2201)
- 在加载 skill 包内容之前先校验 manifest。(#2185)

**安全事件与 CLI**

- 为 agent-sec-cli 事件查询新增会话与运行过滤器。(#2132)

**Raw 打包**

- 新增组件自有的 raw 包构建目标与归档校验。(#2133)
- raw hook 改用捆绑的 Python launcher。(#2255)

## 0.9.0

**Qoder CLI 与 Qwen Code Hook 能力扩展**

- 新增 Qwen 插件与可观测性 hook。(#1473)
- 新增 Qoder CLI hook 框架支持。(#1480)
- 新增 Qoder 提示注入扫描 hook 集成。(#1529)
- 新增 Qwen 提示词扫描 hook 集成。(#1538)
- 为 Qoder CLI 与 Qwen Code 新增代码扫描 hook 集成。(#1535)
- 为 Qoder CLI 的 PreToolUse 新增针对用户与项目 skill 的 Skill Ledger 检查。(#1552)
- 新增 Qwen PII hook。(#1559)
- 新增 Qwen skill ledger hook 集成。(#1561)
- 新增 Qoder CLI 可观测性 hook 集成。(#1580)
- 统一 Qwen Code hook 的 trace 上下文处理。(#1738)

**Codex 与 OpenClaw Hook 集成**

- 在 Codex 插件中新增可观测性能力。(#1495)
- 新增 Codex PreToolUse PII 检查 hook 集成。(#1501)
- 在 hook 响应中展示 OpenClaw 策略提示。(#1525)

**扫描器与策略引擎**

- 模型缓存已存在时跳过提示词模型下载。(#1467)
- 新增自定义 PII 正则规则。(#1522)
- 满足 L1-L3 遥测要求。(#1527)
- 新增中文提示注入与越狱规则，覆盖指令覆盖、权威提升、编码逃逸与角色扮演诱导。(#1554)
- 将模型资源统一为 handle，使提示词扫描的资源生命周期更加一致。(#1553)
- 支持通过环境变量配置提示词扫描模式。(#1620)
- 加固 audit、PII 与 notify hook 行为。(#1649)

**Skill Ledger 运行时**

- 隔离 skill ledger worker，提高 hook 执行的可靠性。(#1492)
- 为 skill ledger 检查解析规范 skill 根目录。(#1558)
- 向 hook 调用方暴露 skill ledger 判定结果。(#1577)

**构建与打包**

- 将提示词扫描基准测试迁移到独立仓库，保持 CLI 包精简。(#1557)
- 打包与运行时校验拒绝过期的 CLI wheel。(#1651)
- 升级 RPM 包时替换并重启 daemon 服务。(#1681)

**测试与 CI**

- 通过排除 ML 包修复 OpenClaw E2E 测试依赖。(#1631)
- 修复 skill-ledger E2E 在 macOS 上的执行。(#1643)

**文档**

- 集中用户指南并新增文档 lint CI。(#1586)

## 0.8.0

**构建与打包**

- 源码构建时安装 Codex 插件，使源码部署获得与打包安装相同的 Codex 集成。(#1302)
- 更新 sec-core 安装流程的源码构建脚本。(#1348)
- 将 uv 管理的 Python 放到共享 sec-core 库目录下，并新增系统安装冒烟检查，修复系统模式源码构建。(#1400)

**代码扫描器**

- 新增针对常见 agent 凭据文件的敏感路径规则，阻止 API key 泄露。(#1401)

**OpenClaw 插件**

- 加固 OpenClaw 部署兼容性处理，并用单元测试覆盖部署边界场景。(#1358)
- 新增 OpenClaw 插件跨版本 E2E 矩阵，在受支持的 OpenClaw 宿主上验证打包插件的加载、Gateway 流程、策略行为与可观测性。(#1372)

**文档**

- 新增双语的 agent-sec-core 用户指南文档及文档维护规则。(#1311)
- 记录 OpenClaw 插件的部署、兼容性与升级指引。(#1370)

## 0.7.1

**提示词扫描器**

- 模型未就绪时将 scan-prompt 降级到 fast 模式；把 DENY 重写为 WARN，并在降级原因中补充诊断信息。(#1258)

**Skill Ledger**

- 澄清 skill ledger 回退警告，并对发现摘要做脱敏处理。(#1240)
- 降低 ledger 对账噪音，并将 live-root 跳过错误类型化。(#1232)

**安全可观测性**

- 为 before_tool_call 与 after_tool_call 新增的 pii_scan 添加可观测性映射。(#1229)

## 0.7.0

**Codex 插件 — OpenAI Codex 的完整安全集成**

- 新增 codex-plugin，包含代码扫描、提示词扫描、skill ledger 与 PII 检查 hook。(#1074)
- 支持将 codex-plugin 打包进 RPM。(#1138)
- 修复 Makefile 与 CI 中 codex-plugin 的路径，保证 RPM 安装校验正确执行。(#1165)

**代码扫描器**

- 新增 code-scanner 的 LLM 模式，用于 AI 辅助安全分析。(#1108)
- 新增 code-scanner 静态规则以扩大覆盖面。(#1033)

**提示词扫描器**

- 新增基于 ollama 模型服务的 L4 多轮意图检测。(#1060)
- 将提示词扫描路由到 daemon，并新增提示词模型预加载以降低延迟。(#786)
- 通过环境变量控制提示词扫描走 daemon 调用。(#933)

**Skill Ledger — 激活守护进程与策略引擎**

- 新增 Skill Ledger 激活守护进程，用于后台完整性监控。(#857)
- 新增运行时激活解析器，用于 skill 信任决策。(#826)
- 新增可配置执行的 Skill Ledger 激活策略。(#944)
- 更新 Skill Ledger 激活与事件契约。(#983)
- 更新 Skill Ledger hook 默认值与对账通知行为。(#1086)
- 对齐各 agent 平台的 skill ledger hook。(#1135)
- 解决 Skill Ledger 的 FUSE 与非托管根目录处理。(#1141)
- 不支持的 Hermes skill ledger 场景采用 fail-open。(#1155)

**守护进程与遥测**

- 新增带 systemd 集成与 RPM 构建支持的 daemon 服务。(#1090)
- 在 daemon 暴露 SQL 查询端点，用于可观测性查询。(#1042)
- 增强 daemon 日志，包含请求与任务。(#871)
- 新增遥测 schema 定义与 SLS JSONL writer。(#977, #1008)
- 向遥测数据传入 agent_name，用于多 Agent 识别。(#1032)
- 为 agent-sec-cli 的结构化输出新增日志系统。(#651)
- 为用户级部署新增 `/run/user/<uid>` 下的安全 daemon socket 回退。(#1129)

**PII 扫描器**

- 扩展 PII 扫描覆盖范围，新增额外的模式检测器。(#925)

**安全可观测性**

- 新增会话报告命令，用于会话结束后的安全摘要。(#703)

**沙箱**

- 收敛沙箱触发规则，保持一致的执行行为。(#979)

**适配器与构建**

- 新增用于适配器 manifest 集成的 ANOLISA CLI component.toml。(#1067)
- 将 systemd-rpm-macros 加入 RPM 构建依赖。(#1156)

## 0.6.0

**自保护 — agent-sec-core 自身的防篡改能力**

- 新增 self-protect 代码扫描规则，阻止在 OpenClaw 与 Hermes 上禁用/卸载 agent-sec 插件。(#692)
- 优化 code-scan 中的 self-protect 规则，消除对前缀匹配插件名的误报，并覆盖 Hermes 卸载/rm 模式。(#710)

**提示词扫描器**

- 在 cosh-extension、hermes-plugin 与 openclaw-plugin 之间统一提示词扫描警告格式，包含结构化字段（威胁类型、风险等级、拦截阶段、模型置信度）。(#709)

**Agent-Sec-CLI**

- 为 agent-sec-cli 新增 daemon 进程，在多次 hook 调用之间摊薄启动延迟。(#677)

**适配器与 Manifest**

- 新增独立的 ANOLISA 适配器入口 `anolisa-for-openclaw`，打包 sec-core 的 OpenClaw 适配脚本，并通过适配器 manifest 驱动 install/detect/uninstall。(#549)
- 新增 Hermes 适配器 runner：将 OpenClaw 入口重构为与目标无关的 `anolisa-adapter-runner`，新增 `anolisa-for-hermes` 包装，并在 sec-core 下按 agent 划分适配器目录布局。(#617)
- 在各适配脚本之间集中 sec-core 适配器 manifest 解析，并将 manifest 移入 cli 包。(#617)

**OpenClaw 集成**

- 归一化 OpenClaw 状态目录处理：适配器文件系统状态使用 `OPENCLAW_STATE_DIR`，调用 OpenClaw CLI 时取消设置 `OPENCLAW_HOME`，并对齐插件安装/列表/卸载处理。(#641)

## 0.5.0

**PII 扫描器 — 个人信息泄露检测**

- 新增 PIIChecker 扫描 CLI，支持文本/文件输入、基于 regex/validator 的检测、脱敏以及安全中间件集成。(#525)
- 新增 cosh 与 OpenClaw 的 PIIChecker hook，输入经 stdin 传递。(#539)
- 新增 Hermes PII 检查 hook。(#556)
- 修复 scan-pii 模块模式检测依赖 subprocess 的问题。(#540)

**安全可观测性 — Agent 运行指标与态势洞察**

- 新增安全可观测性 schema、指标定义与 CLI，并带 agent 运行的 jsonl writer。(#488)
- 新增用于安全可观测性的 openclaw 插件。(#515)
- 新增安全可观测性的 cosh hook。(#528)
- 将可观测性记录持久化到 sqldb，并提供 CLI 审阅命令。(#544)
- 新增 hermes 的可观测性插件。(#553)
- 关联安全事件与可观测性事件，并支持批量查询。(#578)
- 计数查询遵循 trace-id 过滤。(#595)

**Hermes 插件 — AI Agent 集成框架**

- 新增 hermes-plugin 框架，包含抽象 hook 类与代码扫描能力。(#536)
- 新增 Hermes 的 prompt-scan 能力。(#579)
- 新增 Hermes PII 检查 hook。(#556)
- 新增 Hermes skill ledger hook。(#565)
- 新增 hermes 的可观测性插件。(#553)
- 支持 hermes agent 插件中的关联上下文。(#590)
- 新增 hermes 插件的 rpmbuild 与源码构建安装。(#577)
- 稳定 Hermes skill-ledger 在 skill 检查未通过时的警告传递。(#600)

**关联与追踪上下文**

- 在 CLI、OpenClaw 与 cosh 之间统一调用方追踪上下文，支持 `--trace-context` JSON 与 SQLite schema v2。(#569)
- 支持 hermes agent 插件中的关联上下文。(#590)
- 关联安全事件与可观测性事件。(#578)

**Skill Ledger**

- 将 code-scanner 与 skill-ledger 集成，提供统一的安全评估。(#505)
- 更新 skill ledger 的安全交互。(#529)
- 使 openclaw 的 skill ledger 审批可配置。(#575)
- 新增 Hermes skill ledger hook。(#565)
- 完善 skill ledger 扫描流程并对齐文档。(#529)
- 在安装流程中纳入 skill-ledger e2e。(#573)
- 修复 skill-ledger hook 的作用域限制。(#497)
- 修复 skill 托管目录的发现问题。(#510)
- 为 skill-ledger 扩展 home 路径。(#596)
- 加固 skill ledger 恢复与密钥使用体验。(#575)

**代码扫描器**

- 新增 code-scan 的 openclaw requireApproval 配置。(#560)
- 新增 OpenClaw 的 enableBlock hook 策略。(#586)

**安全中间件与事件系统**

- 修复 sqldb 读路径的 TOCTOU 竞态。(#546)
- 为非 DB 子命令延迟导入 SQLAlchemy。(#581)
- 降低 SQL 维护操作的频率。(#546)

**提示词扫描器**

- 通过 hermes 插件新增 Hermes 的 prompt-scan 能力。(#579)
- 将 warmup 检测从错误字符串匹配改为基于文件的检查。(#500)
- 修复提示词文本的传递方式：改用 stdin 而非 argv。(#579)

**工具链与 CI**

- 为 sec-core 新增带本地空间安装的 build-all 支持。(#527)
- 新增 hermes 插件的 rpmbuild 与源码构建安装。(#577)
- 在安装流程中纳入 skill-ledger e2e。(#573)
- 新增用于能力发现的适配器 manifest。(#577)

## 0.4.0

**提示词扫描器**

- 提示词扫描 hook 在模型缺失时改为询问用户，而不是 fail-open。(#463)
- 新增提示注入检测的基准数据集与评测工具。(#464)

**安全中间件与事件系统**

- 将 security_events 的 SQLite 存储重构为 SQLAlchemy ORM，具备多表扩展性与类型化仓储。(#459)

**Skill Ledger**

- 修复 sign-skill 自动注册配置（精确 awk 匹配），并无条件解析 openclaw stdout。(#445)
- 将 XDG 路径统一到 `agent-sec/skill-ledger` 厂商命名空间下。(#445)
- 将单 skill 校验统一为结构化结果，保持输出一致。(#445)
- 将集成测试从 subprocess 迁移到 Typer CliRunner。(#445)

**OpenClaw 集成**

- 在 openclaw gateway 显式注册插件，支持 Gateway 启动规划。(#446)

**重构**

- 移除已弃用的 agent-sec-core skill 目录；将 README 与 spec 对齐到 agent-sec-cli 工作流。(#454)

**工具链与 CI**

- 为 sec-core CI 新增覆盖率报告。(#431)
- 为主分支启用 rpmbuild 与 e2e 测试 CI。(#432)

## 0.3.0

**提示词扫描器 — 多层提示注入与越狱检测**

- 新增提示注入/越狱检测扫描器架构，包含基于 YAML 的 L1 规则引擎与 L2 ML 分类器（Prompt Guard 2）。(#253)
- 将提示词扫描器集成到 cosh hook 与 openclaw 插件，并接入安全中间件生命周期。(#261, #294)
- 新增 `list-scanners` 命令，改进 CLI 帮助，并将 `--scanner-version` 设为可选。(#284)
- 新增提示词扫描摘要与后端测试。(#294)
- 新增 prompt-scanner 的 skill 定义。(#256)
- 新增模型 warmup、审计日志与完整文档。(#253)
- 通过线程安全的模型加载，稳定批量扫描与判定逻辑。(#253)
- 将提示词扫描器响应统一为使用 "ask" 而不是 "block"。(#341)
- 新增 prompt-scanner 的 e2e 测试套件与 Makefile 目标。(#352)

**代码扫描器 — 静态代码安全分析**

- 新增代码扫描器组件，基于规则检测混淆、权限滥用等问题。(#234)
- 将代码扫描器集成到 cosh hook（支持 ask 决策）与 openclaw 插件适配器。(#234)
- 新增代码扫描器的 CLI 入口、错误码与单元测试。(#234)
- 修复代码扫描缺陷并新增 e2e 测试。(#342)

**Skill Ledger — skill 完整性追踪与签名**

- 新增 skill-ledger CLI，并通过中间件集成用于 skill 完整性校验。(#252)
- 新增 skill-ledger 的 skill 定义。(#266)
- 新增 skill-ledger 的 cosh hook（PreToolUse）与 openclaw-plugin 能力。(#292, #281)
- 改进 skill-ledger CLI 并清理导入。(#284)
- 重构 skill-ledger 的配置默认值与文档。(#296)
- 对齐 skill-ledger 工具名并新增路径校验。(#317)
- 重做 skill-ledger 的状态、输出与签名校验。(#335)
- skill-ledger hook 加固、e2e 套件与态势集成。(#339)
- **已知限制：** skill 目录解析假设目录名与 SKILL.md 的 `name` 字段一致；见 #381。

**安全中间件与事件系统**

- 新增安全中间件框架，统一 CLI 入口并集成指标。(#121, #220)
- 新增 sqldb writer 与 reader，并在 CLI 接口提供查询命令，用于安全事件持久化。(#254)
- 修复 SecurityEventWriter 中的跨进程事件丢失。(#226)
- 应用损坏白名单，阻止误判引发的 DB 重建。(#338)
- 新增 e2e 测试，并修复测试中暴露的缺陷。(#330)

**Linux 沙箱**

- 新增沙箱守卫与失败处理 hook。(#362)

**OpenClaw 集成**

- 新增 openclaw 的 hook 插件，集成安全扫描能力。(#242)
- 为 openclaw hook 包新增 jq 依赖声明。(#370)

**Cosh 扩展集成**

- 对接新的 cosh 扩展 API 并新增内置命令。(#302)

**性能**

- 延迟加载 ML 依赖，加速非 ML 子命令。(#318)

**工具链与 CI**

- 将 Python 工具链迁移到 uv 包管理器，并锁定 Python 3.11.6。(#227)
- 新增 sec-core 的 RPM 构建 CI，并适配 nightly 构建流水线。(#295)
- 初始化代码格式检查 CI（python-code-pretty）。(#229)
- 在 RPM 构建 CI 中加入 e2e 测试。(#369)

**缺陷修复**

- 保留 seharden 包装器的默认值。(#236)
- 移除中间件路由处的动态导入。(#277)
- 改进缺失 loongshield 时的指引。(#289)
- 修复构建错误。(#288)
- 移除 openclaw hook 示例并修复文档。(#282)

## 0.2.0

- 新增加固的 skill 签名流水线，并引入 `.skill-meta` 布局。(#129)
- 将 `Cargo.lock` 纳入版本控制。(#149)
- 新增 `make install-sandbox` 目标。(#68)
- 修复 `--argv0` 选项的 bubblewrap 版本兼容性。(#112)
- 变更：重构 SKILL.md 为可执行协议，并对齐子 skill。(#130)
