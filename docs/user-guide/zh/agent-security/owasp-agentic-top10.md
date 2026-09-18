# OWASP Agentic Top 10 安全控制映射

[English](../../en/agent-security/owasp-agentic-top10.md)

ANOLISA 将 OWASP Agentic Top 10 全部十类风险映射到各组件的安全控制。
本文帮助你确认 Agent 部署可用的控制、执行这些控制的路径，以及剩余缺口。

## 范围与评价规则

- **框架：** [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/)。
- **源码基线：** ANOLISA 提交 [`3f4b22e3d645`](https://github.com/alibaba/anolisa/commit/3f4b22e3d645057270155d905964085ddcf58d5a)。
  下方实现与测试链接均固定到该提交。
- **范围：** ANOLISA 各组件已实现能力的组合，包括 AgentSecCore、cosh-ng/cosh-gateway、
  copilot-shell、agent-memory、AgentSight、ANOLISA 服务和 SkillFS。
  各项判定适用于列明的接入路径及配置；安装单个组件不会启用全部控制。
  仅支持 Linux 的控制需要在受支持的 Linux 环境部署。
- **Full：** 在列明的组件、接入路径和配置条件下，主要缓解控制已经实现。
- **Partial：** 已有相关控制，但主要控制链仍存在已知缺口。

这些分级是 ANOLISA 基于源码的架构自评，不是 OWASP 认证，也不保证覆盖每种部署或攻击。
**Mapped** 表示每类风险都有对应的控制映射，不表示 OWASP 的每条缓解建议都已完整实现。

**证据状态：** 本次映射检查了实现和已有测试用例。引用的测试是仓库内已有证据，
不代表本次文档修改执行过这些测试。本次未运行运行时安全测试或对抗部署验证；
具体部署的执行效果仍需在选定 Host 和平台上验证。文档检查结果记录在 PR 中。

## 覆盖汇总

十类风险合计 **7 Full / 3 Partial**。

| 风险类别 | 判定 | 主要组件与控制 |
|---|---|---|
| ASI01 Agent Goal Hijack（目标劫持） | Full | AgentSecCore Prompt Scanner、已接入输入入口的阻断 hook |
| ASI02 Tool Misuse and Exploitation（工具滥用） | Full | cosh-ng 工具审批、hook 决策及受治理执行 |
| ASI03 Identity and Privilege Abuse（身份与权限滥用） | Full | cosh-gateway 调用者、Task、Run、目标绑定及执行授权 |
| ASI04 Agentic Supply Chain Vulnerabilities（供应链风险） | Partial | Skill Ledger、Skill 签名验证、产物校验和部分发布产物 SBOM |
| ASI05 Unexpected Code Execution（非预期代码执行） | Full | AgentSecCore Code Scanner 与 Linux 沙箱 |
| ASI06 Memory and Context Poisoning（记忆与上下文污染） | Full | agent-memory 注入过滤、可选作用域及检索风险标记 |
| ASI07 Insecure Inter-Agent Communication（不安全的 Agent 间通信） | Partial | 本地调用者认证、ACP 会话绑定及 SkillFS 通道认证 |
| ASI08 Cascading Failures（级联故障） | Full | copilot-shell 循环终止与预算、有限重试、ANOLISA 限流及 SkillFS 重启限制 |
| ASI09 Human-Agent Trust Exploitation（人对 Agent 的信任被利用） | Full | 支持审批的 Host 中的人工确认及风险展示 |
| ASI10 Rogue Agents（失控／恶意 Agent） | Partial | AgentSight 凭证外泄行为监测、事件关联及审计 |

## 逐项控制映射

### ASI01 — 目标劫持 · Full

- **风险说明：** 不可信指令将 Agent 引向偏离用户目标的行为。
- **已实现控制：** AgentSecCore 扫描提示词输入，在支持的 Host hook 中于模型调用前
  拒绝已检测出的注入。
- **生效条件：** 扫描器可用，并启用阻断 hook。例如，Codex 需要
  `PROMPT_SCANNER_MODE=deny`；OpenClaw 需要 `promptScanBlock=true` 才会阻断 `deny`。
- **实现与验证证据：** [Codex 提示词 hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/codex-plugin/hooks-plugin/hooks/prompt_scanner_hook.py#L96)
  在 deny 模式阻断 `warn` 和 `deny`；[OpenClaw hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/openclaw-plugin/src/capabilities/prompt-scan.ts#L76)
  按阻断配置控制是否继续分发。已有 [Codex hook 测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/codex_hooks/test_prompt_scanner_hook.py#L220)
  覆盖 observe/deny 行为和扫描器异常回退。
- **剩余限制：** 这些示例扫描当前用户输入，不覆盖每个工具结果、记忆或检索文档。
  observe 模式不阻断；扫描器异常和 CLI 不可用的路径可能 fail-open。
  检测具有启发式边界，不代表能够抵御所有目标劫持。

### ASI02 — 工具滥用 · Full

- **风险说明：** Agent 在预期操作或审批范围之外调用工具。
- **已实现控制：** cosh-ng 将已支持的工具请求接入审批与策略处理，保留 hook 拒绝；
  即使处于 Trust 模式，hook 的 `ask` 和未知 provider 工具仍需等待决策。
- **生效条件：** Host/provider 必须提供已支持的审批或受治理执行路径；对需要干预的
  工具配置对应 hook 和审批策略。
- **实现与验证证据：** [审批桥接](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L19)
  在执行前检查 hook 决策、已知工具身份和 shell 策略。已有 [审批桥接测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge_tests.rs#L707)
  覆盖被阻断／高风险请求以及未知工具保持待审批的情况。
- **剩余限制：** 审批覆盖依赖 provider 接入。Trust 可以自动批准符合条件的已知工具；
  路径外的工具调用不会因为存在 hook 就自动获得审批保障。

### ASI03 — 身份与权限滥用 · Full

- **风险说明：** 调用者复用其他 Agent 的权限、审批或执行上下文。
- **已实现控制：** cosh-gateway 从对端凭据取得本地调用者身份，将受治理请求绑定到
  当前 Task/Run/目标，并在原子消费执行许可之前检查精确匹配和有效期。
- **生效条件：** 请求经过 gateway 认证 socket 和已实现的受治理执行路径，runtime
  与 driver 均支持该路径。调用者身份局限于本地安装和操作系统账户。
- **实现与验证证据：** [socket 准入](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/server.rs#L85)、
  [调度器准入](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/scheduler/brokered/execution.rs#L27)
  和[执行许可消费](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/storage/ledger/execution.rs#L3)
  检查调用者、Task、Run、目标、runtime fence、输入及有效期。
  已有[受治理调度测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/scheduler/brokered/tests.rs#L1101)
  覆盖伪造成功结果被拒绝、持久化拒绝和过期处理。
- **剩余限制：** [Core/Codex 启动能力声明](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/protocol/launch.rs#L144)
  包含本地委托权限和 `gateway_brokered_effects=false`。
  此判定不代表每个原生工具副作用都经过 gateway，也不把本地账户身份当作跨域 Agent 身份。

### ASI04 — 供应链风险 · Partial

- **风险说明：** 被篡改的 Skill、依赖或发布产物进入 Agent 的可信执行环境。
- **已实现控制：** Skill Ledger 先认证 manifest，再检查文件漂移和扫描状态；
  资产验证检查签名的 Skill manifest 与文件。ANOLISA 校验 raw 产物的哈希。
  cosh-ng 和 tokenless 的预编译发布路径生成 SBOM，并验证产物包。
- **生效条件：** 使用可信签名密钥、受管理 Skill、适用的准入策略，以及执行这些
  检查的发布／安装路径。已记录的扫描结果或 SBOM 本身不会拒绝恶意依赖。
- **实现与验证证据：** [Skill Ledger 检查](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger/core/checker.py#L137)、
  [Skill 签名及文件验证](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/asset_verify/verifier.py#L295)、
  [raw 产物下载校验](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-cli/src/commands/tier1/install/raw.rs#L495)
  和 [cosh-ng](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/.github/actions/build-cosh-ng-prebuilt/build.sh#L200)
  ／[tokenless](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/.github/actions/build-tokenless-prebuilt/build.sh#L263)
  发布检查实现了上述控制。已有[资产验证 backend 测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/security_middleware/backends/test_asset_verify_backend.py#L25)
  通过 mock 覆盖成功、失败、发现跳过及异常处理。
- **剩余限制：** [分发签名字段](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-core/src/distribution.rs#L18)
  属于元数据；检查到的 [raw 索引获取](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-cli/src/commands/tier1/install/raw.rs#L91)
  和产物安装路径未强制验证发布者签名。已检查的发布路径也没有形成完整的依赖漏洞
  检测及修复准入链。见[已知缺口](#已知缺口)。

### ASI05 — 非预期代码执行 · Full

- **风险说明：** 生成或输入的代码以非预期的主机访问权限运行。
- **已实现控制：** AgentSecCore 在已接入的工具调用前 hook 扫描代码；Linux 沙箱
  通过 bubblewrap namespace 与 seccomp 限制文件系统、进程可见性，并按策略限制网络。
- **生效条件：** 在代码扫描 Host hook 启用阻断，并在 Linux 上通过沙箱执行，
  选择限制性的文件系统／网络策略。
- **实现与验证证据：** [Hermes 代码 hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/hermes-plugin/src/capabilities/code_scan.py#L29)
  支持阻断 `warn`/`deny`；[沙箱参数构造](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/src/bwrap_args.rs#L96)
  和 [seccomp 设置](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/src/seccomp.rs#L24)
  施加执行限制。已有 [Linux 沙箱测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/tests/suite/bwrap_seccomp.rs#L94)
  覆盖读取、写入拒绝、可写目录及网络相关限制。
- **剩余限制：** observe 模式和扫描器异常可能放行；在所引用路径中，同时允许完整
  文件系统与网络访问会绕过 bubblewrap 隔离。代码扫描具有启发式边界，沙箱效果依赖
  选定策略和 Linux 设施。

### ASI06 — 记忆与上下文污染 · Full

- **风险说明：** 被污染的存储内容在后续使用中被当作可信指令或事实。
- **已实现控制：** agent-memory 在启发式记忆整理时过滤疑似注入的事实，支持按
  作用域检索，并为关键词检索中的可疑片段提供标记，供消费端 adapter 处理。
- **生效条件：** 使用具有过滤能力的记忆整理路径和带作用域的 `memory_search`；
  采用配置隔离时设置有效 `agent_scope` 和可信 `MCP_CLIENT_NAME`。
  消费端需要处理检索风险标记，并控制作用域覆盖和直接文件访问。
- **实现与验证证据：** [事实过滤](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/consolidation/heuristics.rs#L102)、
  [检索作用域选择](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/tools/memory_search.rs#L41)
  和[带作用域的关键词检索](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/index/store.rs#L300)
  实现了这些路径。已有[注入模式测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/safety.rs#L167)
  和[作用域测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/index/store.rs#L1903)覆盖其局部契约。
- **剩余限制：** 默认作用域为 shared；身份缺失或配置作用域无效时会告警并回退到
  共享检索。直接记忆写入并非全部经过注入过滤，`suspicious` 标记也不是阻断决策。
  这些控制不认证任意记忆写入者，也不保证存储事实真实。

### ASI07 — 不安全的 Agent 间通信 · Partial

- **风险说明：** 伪造、替换或绑定错误的消息被作为可信对端消息接受。
- **已实现控制：** cosh-gateway 认证本地调用者，将 ACP 权限请求绑定到会话及当前
  执行上下文。Skill Ledger 与 SkillFS 使用绑定会话 nonce、方向和 payload 的
  共享密钥证明，认证私有 socket 通道。
- **生效条件：** 使用带认证的本地 socket／ACP 路径，正确配置 SkillFS 通道两端及
  受保护的密钥文件。
- **实现与验证证据：** [gateway 对端凭据](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/server.rs#L85)、
  [ACP 权限绑定](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/runtime/acp_port/permission.rs#L1)
  和 [SkillFS 通道证明](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger/skillfs_peer_auth.py#L95)
  执行本地通道检查。已有[对端认证测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/skill_ledger/test_skillfs_peer_auth.py#L134)
  覆盖错误密钥、篡改和方向错误；仓库中的[容器对端认证探测脚本](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/skillfs/scripts/container-peer-auth-probe.py#L275)
  包含旧证明重放用例，不代表本次文档修改产生了运行结果。
- **剩余限制：** 这些控制保护 Host/runtime 和组件通道。已检查路径没有提供通用的
  Agent 间信任层，用于验证独立运营 Agent 的对端身份及委托能力。
  本地通道认证不能单独补齐该缺口。

### ASI08 — 级联故障 · Full

- **风险说明：** 重复调用、失控委托、重试或重启循环放大故障。
- **已实现控制：** copilot-shell 终止检测到的循环，限制轮次并检查 subagent 时间预算；
  重试具备有限次数及退避。ANOLISA helper 服务按 UID 限流；SkillFS 在 managed
  worker 连续快速失败达到阈值后停止重启。
- **生效条件：** 保持 copilot-shell 循环检测开启，配置适用的 session/subagent
  预算，并使用有限重试、helper 及 managed SkillFS 路径。限制在各自组件边界生效。
- **实现与验证证据：** [轮次与循环终止](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/core/client.ts#L660)、
  [subagent 预算](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/subagents/subagent.ts#L345)、
  [有限重试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/utils/retry.ts#L98)、
  [helper 限流](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-core/src/daemon_server.rs#L252)
  和 [SkillFS 重启终止](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/skillfs/crates/skillfs-cli/src/managed.rs#L734)
  执行上述限制。已有[循环检测测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/services/loopDetectionService.test.ts#L71)
  和[重试测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/utils/retry.test.ts)
  覆盖检测阈值及重试行为。
- **剩余限制：** subagent 时间预算在轮次边界及模型流结束后检查，预算耗尽不会
  立即中断正在执行的模型或工具调用。这些控制位于组件内部，不是覆盖所有外部
  服务和 Agent 网络的全局熔断器。仅输出警告的路径不计作终止。
  停止循环不会补偿已经完成的外部操作。

### ASI09 — 人对 Agent 的信任被利用 · Full

- **风险说明：** Agent 的展示掩盖了实际影响或诱导过度信任，使用户批准有害操作。
- **已实现控制：** 支持审批的 Host 提供明确审批决策，并展示工具／命令预览及风险。
  在 Trust 模式下，cosh-ng 仍保留 hook 要求的审批，以及执行判定为 Block、
  或 High 且涉及系统控制或无法解析的启动链的人工确认。
  其他 High 风险请求在 Trust 模式下仍可能自动批准。
- **生效条件：** 使用具备原生审批能力的 Host/provider，并通过策略要求审核相关
  操作；用户需要在批准前检查展示的操作。
- **实现与验证证据：** [审批详情](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/approval/cards.rs#L187)
  展示请求、预览、风险和评估；[审批桥接](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L44)
  保留明确审核路径，[评估判定条件](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L138)
  明确哪些命令评估需要人工审批。已有 [Trust 模式审批测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge_tests.rs#L802)
  检查强制审批与自动批准的边界：`reboot` 保持待审批，而 High 级别的 shell 语法
  风险请求会自动批准。
- **剩余限制：** 人工审批不能保证判断正确。adapter 的 `ask` 配置不等于原生审批：
  [Codex Skill Ledger hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/codex-plugin/hooks-plugin/hooks/skill_ledger_hook.py#L444)
  在不支持审批时回退为警告；该回退不计作人工确认。

### ASI10 — 失控／恶意 Agent · Partial

- **风险说明：** Agent 出现需要检测和干预的危险行为。
- **已实现控制：** AgentSight 监测已支持的凭证外泄行为，将安全事件关联为风险案例，
  并持久化审计证据。限制请求流程会记录进程身份、来源策略和待执行意图，但当前
  真实后端尚不能通过该流程形成有效限制。
- **生效条件：** 部署支持的 Linux 监测栈，使用有效的 Audit 策略，配置匹配的凭证
  来源和目标范围，并确保事件采集及审计持久化正常工作。
- **实现与验证证据：** [凭证事件处理](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L860)
  与[审计接收及关联](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-audit/src/service.rs#L211)
  实现监测及证据处理。[限制请求流程](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/src/security/containment.rs#L226)
  已存在，但真实后端[拒绝凭证 Enforce 模式](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L784)，
  且[尚不支持运行中的策略切换](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L496)。
  已有[限制 adapter 测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/src/security/containment_adapter_tests.rs#L269)
  使用测试 enforcer 检查旧确认的拒绝和审计绑定保留；[后端切换测试](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L1664)
  检查不支持切换时的返回结果。这些测试不证明运行时阻断生效。
- **剩余限制：** 当前源码基线下，该限制路径尚缺凭证阻断及运行中的策略切换。
  因此，待执行请求和有限时长字段不能计作已生效的限制或临时限制。
  检测和审计也不代表通用目标漂移检测，或自动隔离每个失控 Agent。

## 已知缺口

| 类别 | 已有保护 | 保持 Partial 的原因 |
|---|---|---|
| ASI04 | Skill 签名检查、产物哈希和部分发布产物 SBOM | 已检查的 raw 安装路径未强制执行发布者签名验证；已检查发布路径的依赖漏洞检测及修复尚未形成完整准入链。 |
| ASI07 | 本地对端凭据、ACP 会话／上下文检查及 SkillFS 认证通道 | 这些本地通道尚未形成针对独立运营 Agent 对端及其委托能力的通用信任机制。 |
| ASI10 | 凭证外泄行为监测、事件关联及审计 | 真实后端拒绝凭证 Enforce 模式，且尚不支持运行中的策略切换；限制请求流程尚不能形成有效阻断。 |

补齐任一缺口都需要已实现的执行路径及其接入证据。元数据字段、传输连通性或计划中
的机制本身不会改变判定。

## 参考资料

- [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/)
- [ANOLISA 用户指南](../README.md)
- [AgentSecCore 快速开始](agent-sec-core/QUICKSTART.md)
- [Prompt Scanner](agent-sec-core/prompt-scanner.md)
- [Code Scanner hook 配置](agent-sec-core/code-scanner.md)
- [Skill Ledger](agent-sec-core/skill-ledger.md)
- [资产验证](agent-sec-core/asset-verification.md)
