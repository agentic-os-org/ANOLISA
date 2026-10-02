> **爬取时间**: 2026-06-17 16:29:19
> **原文链接**: https://help.aliyun.com/zh/alinux/how-to-use-agentsight
> **文档更新**: 2026-05-20T21:02:26+08:00

---

AgentSight 是基于 eBPF 的 AI Agent 可观测性工具，在零侵入业务逻辑的前提下，实现对 Agent 运行全链路的细粒度数据采集与关联分析。

## 如何使用 AgentSight

## 产品简介

AgentSight 是基于 eBPF 的 AI Agent 可观测性工具，在零侵入业务逻辑的前提下，实现对 Agent 运行全链路的细粒度数据采集与关联分析。

## 核心能力

AgentSight 主要包含以下能力：

-   **Token 消耗分析**：对 Agent 运行过程中的 Token 消耗进行全方位度量与归因。支持按时间段或最近 N 小时灵活查询，可自动环比对比。支持按智能体、任务、角色等多维度拆分消耗来源，分析粒度可精确至单次 LLM 调用。
    
-   **行为审计**：对 Agent 的 LLM 调用及进程执行行为的全链路记录与追踪。在数据采集中，完整留存每次 LLM 调用的提供商、模型版本等关键元数据，并同步捕获进程的命令行参数。此外，系统支持按时间维度、进程标识及事件类型进行多维度灵活筛选，并提供可视化的汇总统计分析能力。
    
-   **Dashboard 可视化**：Web 可视化界面，提供 Token 消耗、Agent 状态监控与 Session 详情的直观展示。支持在浏览器中实时查看数据刷新，可将 Dashboard 部署在远程服务器上，通过本地浏览器直接访问，无需登录服务器。通过 Dashboard 可按时间段查看机器的 Token 消耗趋势，实时监控 Agent 进程状态并提供异常重启能力，同时支持深入查看每次 Session 的完整 Trace 链路，包括用户输入、模型提示词、推理思考过程及每一步的 Token 消耗分布，帮助精准分析模型调用效率与优化成本控制。
    

* * *

## **使用范围**

本工具适用于 OpenClaw 以及Copilot Shell （非 AK/SK 认证场景）

### 安装方式

详情请参考[快速入门](https://help.aliyun.com/zh/alinux/agentic-os-getting-started)。

## 对话式交互使用方式

AgentSight 提供了对话式交互 Skill，支持在各类 AI Agent 中安装使用，用户无需记忆 CLI 命令，直接通过自然语言即可完成操作：

-   **查看 Token 消耗**：如"今天 Token 用了多少？"
    
-   **查询审计日志**：如"帮我查一下今天的 LLM 调用记录"
    

> 如果你使用的是 Copilot Shell（cosh），该 Skill 已内置，可直接使用以上自然语言指令，系统会自动调用 AgentSight 完成查询并返回分析结论。

* * *

## CLI 命令详细说明

### agentsight trace — 启动 eBPF 追踪

> 注：该服务已在系统中默认启动，无需手动执行。

启动基于 eBPF 的 AI Agent 活动追踪。

```
agentsight trace #需要root权限执行
```

### agentsight serve — 启动 API 及 Dashboard

> 注：该服务已在系统中默认启动，默认绑定 0.0.0.0:7396，无需手动执行。

启动 HTTP API 服务器，提供嵌入式 Dashboard UI。

```
agentsight serve --host 0.0.0.0 --port 7396 #需要root权限执行
```

该命令将绑定所有网络接口，可通过服务器公网 IP 访问：`http://<服务器公网IP>:7396`

**请确保服务器防火墙 / 安全组已放行 7396 端口**。

### agentsight token — 查询 Token 用量

查询 Token 用量数据。

```
# 查看今日用量
agentsight token
```

### agentsight audit — 查询审计事件

查询审计事件（LLM 调用、进程操作）。

```
# 查看最近事件
agentsight audit
# 按 PID 和类型过滤
agentsight audit --pid 12345 --type llm
# 汇总统计
agentsight audit --summary
```

### agentsight discover — 扫描 Agent

发现系统上运行的 AI Agent。

```
# 扫描 Agent
agentsight discover
# 列出已知类型
agentsight discover --list-known
```

## Dashboard 可视化界面

Dashboard 是AgentSight 的 Web 可视化界面，用于查看对话历史、Trace 详情和 Token 统计数据。

### Dashboard 功能

Dashboard 提供以下核心功能：

-   **Token 消耗总览**：查看当前机器在所选时间段内的 token 消耗情况。Dashboard 顶部提供时间范围选择器，可切换不同时间段；下方以统计卡片形式分别展示输入 Token、输出 Token 及总 Token 用量
    

-   **Agent 状态**：右侧状态栏可以查看当前 Agent 进程状态，并提供 Agent 进程 hang 住重启功能
    
-   **会话中断诊断**：针对长时间会话无输出或对话无响应的问题，自动识别 LLM 错误与 Agent 进程崩溃，输出详细原因分析，辅助快速定位与解决。会话列表展示各 Session 的 **SESSION ID**、**AGENT**、**MODEL**、对话数及 TOKEN 用量等信息。展开某条会话可查看 **CONVERSATION ID** 子表，中断列以标签形式（如 **1高危**）标识异常。底部 **Interruptions** 面板显示未处理的中断详情，例如 LLM Error 类型错误（错误信息 `invalid access token or token expired`，状态码 401），可通过 **Resolve** 或 **Hide** 按钮进行处理。
    
-   **Session 详情**：点击"详情"查看每个 session 和 trace的 token 使用详细情况
    

在 Trace 详情面板中，顶部通过**按 Trace**和**按 Session**标签页切换视图，输入 Trace ID 后单击**加载**查看详情。左侧 Agent 信息卡片展示名称、版本、模型及工具定义数量，右侧统计卡片分别展示**总步骤数**、**总输入 Token**和**总输出 Token**。下方**交互轨迹**区域按步骤列出每次交互的角色标签、时间戳和内容，包括系统 Prompt 文本及工具定义列表（如 read、write、edit、exec）。

-   **模型分析**：查看用户输入后的模型提示词与思考过程，定位 Token 主要消耗环节
    
-   **Token节省**：查看当前已经节省的Token数量，支持点击SESSION ID查看每个优化项，点击详情可查看优化前后的内容对比。
    

**Token 节省**页面支持按时间范围和 Agent 筛选查询。页面顶部以汇总卡片展示**总 Token 消耗**（含输入/输出分类）、**已降低 Token**（含工具/MCP 分类）及**降低率**（附等级评价）。下方表格按会话列出各 Session 的输入/输出 Token、已降低数量和降低率。

例如，**MCP输出** 分类下可查看每条优化项的优化前后 token 数量（如优化前 734、优化后 182），展开详情后左侧 **原始内容**（红色背景）显示完整 JSON，右侧 **优化后**（绿色背景）显示精简结果，冗余元数据字段会被截断以节省 token。

* * *

## 数据管理

### 数据库管理

**自动限容与清理**：为防止数据库无限增长占用过多磁盘空间，系统默认设置数据库最大容量为 200 MB。当数据库大小达到上限时，会自动触发清理流程。

用户可通过环境变量 AGENTSIGHT\_GENAI\_DB\_MAX\_SIZE\_MB 自定义最大容量（单位：MB），例如设置为 500 MB。

```
export AGENTSIGHT_GENAI_DB_MAX_SIZE_MB=500 
```

### 清理历史数据

如需清理历史数据，执行以下操作：

```
rm -rf /var/log/sysak/.agentsight
```

然后重启 AgentSight 即可。

## **常见问题**

**Q1：为何无法获取 OpenClaw 的 Token 消耗数据？**  
**A：** AgentSight 监控的是 `openclaw-gateway` 守护进程。请检查客户端与 Gateway 的连接状态是否正常。若出现以下异常日志，表明配对未成功：  
`Gateway agent failed; falling back to embedded: Error: gateway closed (1008): pairing required`  
建议执行命令 `openclaw devices approve` 完成设备配对。  
  
  

**Q2：为何 Token 节省页面未显示当前 Session ID，或显示的 Token 节省量为 0？**  
**A：** 可能由以下两种原因导致：  

1.  当前版本暂不支持 Cosh 的 AK/SK 认证方式；
    
2.  Session ID 格式非标准 UUID，导致系统匹配失败。
    

**Q3：为何 Token 节省页面显示的“优化项节省量”大于“优化前 Token 数”减去“优化后 Token 数”的差值？**

**A：** 这是因为 Agent 在每次对话时会将历史消息纳入上下文。因此，当前对话的统计结果中包含了历史消息的优化收益，导致累计节省量大于单次对话的即时差值。
