> **爬取时间**: 2026-06-17 16:29:14
> **原文链接**: https://help.aliyun.com/zh/alinux/agentic-os
> **文档更新**: 2026-05-28T10:31:10+08:00

---

Alibaba Cloud Linux 4 Agentic Edition，别名ANOLISA ，是阿里云一款 Agent-first 操作系统，专为 AI Agent 设计

## **镜像介绍**

ANOLISA是阿里云基于自研的操作系统Alibaba Cloud Linux为Agent设计的衍生操作系统，它提供Agent最佳的运行环境，提升阿里云客户使用Agent的体验。ANOLISA完全兼容 Alinux4 所有能力（内核优化、云原生支持等），围绕 Agent 的认知方式和工作模式，构建全新的OS架构。

| **层级** | **组件** | **说明** |
| --- | --- | --- |
| 封装交互层 | Copilot Shell（cosh） | 替代默认 Shell，支持自然语言 + bash 双模交互 |
| OS Skills | 内置技能包机制，Agent 通过 Skill 说明书与运行时层、基础系统层交互获得部署、运维、诊断、可观测等"动手能力" |
| 运行时层 | AgentSecCore | AgentSecCore 是专门为 AI Agent 打造的安全产品，聚焦Prompt注入、动态代码执行、Skill安全、意图偏离、系统环境风险等Agent生命周期的核心威胁，构建“感知-决策-阻断-溯源” 的OS 侧多维度、纵深、闭环防御机制，支持无侵入式集成到Cosh、OPENCLAW 等 Agent 框架。 |
| AgentSight | 一款基于eBPF技术的 AI Agent 可观测性工具，能够在无侵入、零修改前提下**，**对运行在 Linux 系统上的 AI Agent 进行实时监控，捕获其LLM API 调用、Token 消耗及进程行为。 |
| Tokenless | Tokenless 是ANOLISA的 Token 优化组件，旨在不侵入业务逻辑的前提下，通过自动压缩工具定义与模型响应内容，显著降低 LLM 推理 Token 消耗。 |
| ws-ckpt | AI Agent 工作区快照与回滚工具，用户可在关键操作前手动创建快照，或开启每轮问答结束自动快照，支持一键回退至任意历史状态，保障执行过程的可回退性 |
| Skill Optimizer | Skill 智能优化引擎，通过环境感知按需加载与离线预编译，减少无关 Skill 干扰，提升 Agent 任务完成率并降低 Token 消耗 |
| 基础系统层 | Alinux4 | 兼容 Alinux4 所有能力（内核优化、云原生支持等） |

## 适用范围

ANOLISA适用范围说明：

-   适用于多种实例规格族，包括弹性裸金属服务器。更多信息，请参见[实例规格族](https://help.aliyun.com/zh/ecs/user-guide/overview-of-instance-families#concept-sx4-lxv-tdb)。
    
    -   仅支持X86的CPU架构
        
    -   支持实例内存建议>=2GB
        
-   适用于各种 Agent 场景工作负载，包括 OpenClaw、QwenPaw、Claude Code 等主流 Agent 框架
    

## 费用

ANOLISA是免费的操作系统镜像，但使用镜像时，需要支付其他资源产生的费用，如大模型调用，vCPU、内存、存储、公网带宽和快照等。

## 核心优势

-   **极致Token经济型**  
    将复杂的OS专家知识封装为标准化 Skill ，大幅减少执行环境理解以及试错探索的Token开销，实现从意图到执行的零延迟闭环。
    
-   **自然语言重新定义人机交互**  
    首次将 cosh（Copilot Shell）作为默认交互入口，用户通过自然语言即可驱动操作系统完成环境部署、工具安装等日常运维操作，告别复杂命令行记忆，带来操作方式上的根本性变革。
    
-   **Skill 全链路安全加密，构筑内生安全防线**  
    对每个 Skill 实施数字签名与加密保护，调用前强制身份鉴权与完整性校验，结合硬件级安全沙箱隔离异常行为，从 OS 内核层面确保 Agent 在受控、可审计、最小权限的环境中安全运行。
    

## 核心组件简介

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）包含四大核心组件：Copilot Shell、AgentSecCore、AgentSight、OS Skills，目前均已开源，开源链接[https://github.com/alibaba/anolisa](https://github.com/alibaba/anolisa)。

### **Cosh（Copilot Shell）**

Copilot Shell（cosh）是 Alibaba Cloud Linux 4 Agentic Edition（ANOLISA） 的默认交互式 Shell，替代 bash 作为系统登录后的第一入口。

cosh 的核心设计理念是「双模交互」。自然语言模式下，用户直接用中文或英文描述意图，系统借助大模型将其转化为可执行的系统操作；命令模式下，用户可通过 ! 前缀快速执行 Shell 命令，或通过 /bash 回退到全功能交互式 bash。两种模式自由混合，无需切换环境。

在保留完整 bash 兼容性的基础上，cosh 增加了自然语言理解、Skill 调用、MCP 工具集成和多级审批控制等能力。cosh 将复杂的系统级能力抽象为自然语言交互，又集成了OS Skills 说明书，降低了操作系统的使用门槛，使人类用户和 Agent 智能体都能以简单的方式驱动操作系统完成任务。

### OS Skills

OS Skills是ANOLISA 为 AI Agent 编写的操作系统使用手册。

传统操作系统文档面向人类用户，依赖自然语言描述、截图示例和行业潜在共识。Agent 在阅读这类文档时，需要消耗大量 Token 进行理解。OS Skills 说明书将操作系统知识重新组织为 Agent 可直接理解和执行的结构化格式——SKILL，Agent 不再需要「读懂文档再操作」，而是「读到即能做」。

OS Skills 说明书已覆盖两大领域：

| **说明书领域** | **对应知识域** | **覆盖内容** |
| --- | --- | --- |
| system-admin | 系统管理 | 用户与权限管理、系统服务管理、内核升级等基础系统管理操作 |
| security | 系统安全 | 系统安全基线检查、漏洞扫描与修复等 |
| system-ops | 系统运维 | 提供Linux常见性能以及稳定性问题的诊断能力 |

Agent 在接收到用户意图后，自动匹配对应的 Skill 并执行，无需人工指定调用路径。

### AgentSecCore

AgentSecCore是面向 AI Agent 运行平台的操作系统级安全内核。在 AI Agent 逐步获得操作系统级别的执行能力（包括文件读写、网络访问、进程管理等）的背景下，传统应用安全边界已不再适用。AgentSecCore 从 OS 层面为 Agent 构建纵深防御体系，确保 Agent 在受控、可审计、最小权限的环境中安全运行。

AgentSecCore 围绕"意图安全"和"系统级兜底"两大支柱，构建了三层纵深防御体系——即使前一层被突破，后续层仍能兜住。架构自下而上为：

| **层级** | **防护能力** | **技术实现** |
| --- | --- | --- |
| **第一层：执行前画边界（预防）** | Prompt Scanner Code Scanner Skill Ledger | 提示注入与越狱检测引擎（规则+ML+向量检索三层递进） 代码执行前安全拦截器（28条检测规则，支持Shell/Python） Skill 完整性防篡改引擎（快照签名 + 只追加版本链 + 四阶段安全扫描） |
| **第二层：执行中做感知（检测）** | 安全可观测 | 覆盖沙箱隔离、系统加固、资产完整性三域，结构化安全事件持久化，按需生成安全汇总报告 |
| **第三层：底层做兜底（遏制）** | 安全基线巡检 OS级隔离与监控 | OS级安全加固规则库自动扫描，检测Agent对系统安全水位的破坏，生成偏离报告和修复建议 依托Linux内核安全原语（Namespace/Cgroup/seccomp/Capability），提供进程级沙箱隔离、系统调用监控与拦截、细粒度权限控制 |

### AgentSight

AgentSight 是面向 AI Agent 运行平台的操作系统级可观测组件，解决 Agent 运行中 Token 消耗远超预期、用户缺乏感知与追溯手段的问题。它在零侵入业务逻辑的前提下，实现对 Agent 运行全链路的细粒度数据采集与关联分析。

AgentSight 主要提供以下三项能力：

-   **Token消耗分析**：对 Agent 运行过程中的 Token 消耗进行全方位度量与归因。支持按时间段或最近 N 小时灵活查询，支持按智能体、任务、角色等多维度拆分消耗来源，分析粒度可精确至单次 LLM 调用。
    
-   **行为审计**：全链路记录 Agent 的 LLM 调用与进程执行行为。完整留存每次调用的提供商、模型版本等元数据，同步捕获进程命令行参数，支持按时间、会话等多维度进行筛选与可视化汇总统计。
    
-   **Dashboard 可视化：**提供 Web 可视化界面，支持远程部署后通过本地浏览器直接访问。可实时查看 Token 消耗趋势、监控 Agent 进程状态并提供异常重启能力，同时支持逐层深入查看每次 Session 的完整 Trace 链路，包括用户输入、模型提示词、推理过程及每一步 Token 消耗分布。
    

### **ws-ckpt**

ws-ckpt 是ANOLISA 面向 AI Agent 工作区的文件级快照与回滚工具（AI Agent Workspace Checkpoint）。

AI Agent 在执行任务时会对工作区文件进行大量修改，一旦操作失误或结果不符预期，用户往往面临难以恢复的困境。ws-ckpt 快照机制为工作区提供轻量级的快照管理能力，用户可在关键操作前手动创建快照，或开启每轮问答结束自动快照，需要回退时一键恢复至任意历史状态，让 Agent 的操作可撤回、可追溯。

ws-ckpt 的核心设计理念是给「 Agent 的工作上保险」。用户在执行危险操作前通过自然语言或 CLI 手动创建快照，或进行文件频繁修改任务时开启每轮问答结束自动快照，首次创建快照时系统自动完成初始化，无需额外配置。

ws-ckpt 主要提供以下能力：

| **能力** | **说明** |
| --- | --- |
| 手动快照 | 用户在关键操作前通过自然语言或 CLI 命令手动创建工作区快照 |
| 自动快照 | 用户在进行文件频繁修改任务时开启每轮问答结束自动快照开关（目前已支持 OpenCalw 和 Hermes ） |
| 一键回滚 | 支持回退至任意历史快照，恢复工作区文件到指定时间点的完整状态 |
| 快照管理 | 提供快照列表查看与删除能力，支持用户自定义快照标识与描述 |
| 双模交互 | 同时支持自然语言交互（通过 Agent 对话）与 CLI 命令两种操作模式 |

### **Skill Optimizer**

Skill Optimizer 是ANOLISA的 Skill 智能优化引擎，从加载和执行两个维度提升 Agent 使用 Skill 的效率与质量。

随着 Skill 生态不断丰富，Agent 面临两个挑战：一是每轮对话加载全量 Skill 列表会引入大量无关上下文，增加 Token 消耗并干扰决策；二是不同模型对同一 Skill 的理解和执行能力差异显著，导致 Skill 在跨模型场景下表现不稳定。Skill Optimizer 从这两个维度同时优化：智能过滤让 Agent 只看到当前任务最相关的 Skill 子集，预编译优化让高频 Skill 更好地适配目标模型的能力特征。

Skill Optimizer 旨在让 Agent 用更少的 Skill 做更对的事。在加载侧，系统自动识别运行环境与工作空间类型，智能匹配并展示最相关的 Skill 子集，整个过程对 Agent 框架完全透明。在执行侧，ANOLISA从社区筛选出多个高频热门 Skill，经过离线编译优化后预置于系统镜像中，用户命中这些 Skill 时可获得更高的任务完成率与 Token 节省。

Skill Optimizer 主要提供以下能力：

| **能力** | **说明** |
| --- | --- |
| 智能过滤 | 根据运行环境与工作空间类型，自动筛选并展示与当前任务最相关的 Skill 子集，屏蔽无关 Skill |
| 预编译优化 | 内置多个经过离线编译优化的高频 Skill 变体，适配目标模型能力特征，提升执行成功率 |
| 透明集成 | 对上游 Agent 框架完全透明，无需修改代码，通过配置或对话启用 |

### **Tokenless**

Tokenless 是 ANOLISA的 Token 优化组件 ，从上下文压缩和命令过滤两个维度降低Agent 与 LLM 交互的 Token 消耗。

随着 Agent承接的任务日益复杂，工具定义的膨胀、结构化响应的冗余、以及命令输出的噪音会快速填满上下文窗口，既推高推理成本，也挤占有效信息的传达空间。Tokenless 在 Agent 与 LLM 之间构建一条智能优化管线：在前端，自动精简工具定义的描述信息，识别并过滤响应中的低价值字段；在中段，对结构化数据进行紧凑编码，进一步压缩体积；在后端，智能过滤命令执行输出中的干扰内容。三者协同工作，在不改变 Agent 行为语义的前提下显著降低 Token 开销。

Tokenless 旨在让 Agent 用更少的 Token 完成相同的任务。整个优化过程通过插件和 Hook 机制自动介入，对上游 Agent 框架完全透明，无需修改业务代码。所有压缩效果均有量化记录，为评估优化收益提供数据支撑。

Tokenless 主要提供以下能力：

| **能力** | **说明** |
| --- | --- |
| 上下文压缩 | 精简 Function Calling工具定义、过滤 CLI 命令响应中的干扰信息、紧凑编码压缩结构化数据 |
| 统计追踪 | 自动记录压缩前后对比，按类型汇总节省量 |
| 透明集成 | 通过插件和 Hook 自动介入，对 Agent 框架零侵入 |
