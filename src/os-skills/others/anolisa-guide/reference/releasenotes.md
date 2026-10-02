> **爬取时间**: 2026-06-17 16:29:13
> **原文链接**: https://help.aliyun.com/zh/alinux/releasenotes
> **文档更新**: 2026-06-04T08:31:48+08:00

---

阿里云定期发布 Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）镜像的更新版本，以确保用户可以获取到最新的功能特性和安全补丁。您可以通过本文查看 Alibaba Cloud Linux 4 Agentic Edition 镜像最新的可用版本及更新内容。

## **Agentic OS (**ANOLISA**)** **0.5**

### **发布信息**

发布时间：2026-05-29

镜像 ID：aliyun\_4\_x64\_20G\_agentic\_alibase\_20260529.vhd

### **更新内容**

Agentic OS 0.5 基于 Alibaba Cloud Linux 4.0.3.0。核心组件功能更新：

-   Copilot Shell 模型认证接入支持百炼 Token Plan；
    
-   AgentSight 支持用户自定义采集配置（HTTP/HTTPS 规则等），降低运行内存使用；
    
-   Tokenless 新增对 Hermes Agent 的支持；
    
-   AgentSecCore 新增用户输入敏感信息自动检测与告警，强化 Skill 恶意脚本扫描以保障使用安全，支持对 Agent 运行过程中的安全事件进行可观测监控，并提供 Hermes Agent 一键部署脚本以实现快速接入；
    
-   ws-ckpt 支持 OpenClaw 与 Hermes Agent，提供对话级自动快照；同时优化了文件变更识别，确保移动或链接操作准确显示为重命名而非误报删除，并在删除含关联 skill 的快照时增加强制确认机制，有效防止误删。
    

### **核心组件版本**

| 组件  | 版本  | 说明  |
| --- | --- | --- |
| copilot-shell | 2.4.1-1 | 代替 Bash 作为系统交互入口 |
| os-skills | 0.3.0-1 | 系统级别技能扩展包 |
| agent-sec-core | 0.5.0-1 | 智能体安全核心 |
| loongshield | 1.2.0-1 | 提供系统安全加固能力 |
| agentsight | 0.5.1-2 | 提供 Token 可观测能力 |
| tokenless | 0.4.1-1 | 优化上下文 Token 消耗 |
| ws-ckpt | 0.3.2-1 | 为 Agent 工作区提供毫秒级快照与回滚能力 |
| skillfs | 0.2.0-1 | 提供技能空间的智能感知能力 |

## **Agentic OS (**ANOLISA**) 0.4**

### **发布信息**

发布时间：2026-05-15

镜像 ID：aliyun\_4\_x64\_20G\_agentic\_alibase\_20260515.vhd

### **更新内容**

Agentic OS 0.4 基于 Alibaba Cloud Linux 4.0.3。核心组件功能更新：

-   Copilot Shell 引入自动记忆后台提取系统，优化内存使用量，长时间运行更稳定；
    
-   AgentSight 新增技能指标分析和 Hermes Agent 识别能力，完善 SSL 检测和事件上报链路；
    
-   Tokenless 优化安装部署和压缩统计链路，新增 Tool-Ready 工具就绪环境预检功能；
    
-   AgentSecCore 新增中文提示词注入检测基准以强化安全防护，并优化 Copilot Shell 首次使用引导，解决插件冲突问题，带来更流畅、安全的交互体验；
    
-   ws-ckpt 新增自动清理机制和配置热加载能力，修复状态恢复和镜像配置等核心问题；
    
-   SkillFS 支持写入与新建技能目录，同时日志时间改为本地时区。
    

### **核心组件版本**

| **组件** | **版本** | **说明** |
| --- | --- | --- |
| copilot-shell | 2.3.0-1 | 代替 Bash 作为系统交互入口 |
| os-skills | 0.3.0-1 | 系统级别技能扩展包 |
| agent-sec-core | 0.4.1-1 | 智能体安全核心 |
| loongshield | 1.2.0-1 | 提供系统安全加固能力 |
| agentsight | 0.4.0-2 | 提供 Token 可观测能力 |
| tokenless | 0.3.2-2 | 优化上下文 Token 消耗 |
| ws-ckpt | 0.2.0-1 | 为 Agent 工作区提供毫秒级快照与回滚能力 |
| skillfs | 0.2.0-1 | 提供技能空间的智能感知能力 |

## **Agentic OS (**ANOLISA**) 0.3**

### **发布信息**

**发布时间**：2026-05-07

**镜像 ID**：aliyun\_4\_x64\_20G\_agentic\_alibase\_20260507.vhd

### **更新内容**

Agentic OS 0.3 基于 Alibaba Cloud Linux 4.0.3。核心组件功能更新：

-   Copilot Shell 引入全新交互式 Skills TUI 面板、可配置状态栏、会话导出功能，聚焦 Hook 功能完善与问题修复；
    
-   AgentSight 面板新增 Token 节省、Agent 中断/卡死检测能力，提供更加精准的 Agent 健康监控能力；
    
-   AgentSecCore 引入了多层提示注入与越狱检测、静态代码安全分析和 Skill 供应链完整性管理三大全新安全扫描能力，建立安全事件可观测基础设施；
    
-   OS Skills 新增 Hermes Agent 安装与 ClawHub 技能管理能力；
    
-   Tokenless 优化组件引入压缩效果统计功能，并增加 TOON（Token-Oriented Object Notation）格式编码支持；
    
-   新增 Agent Workspace Checkpoint 组件（ws-ckpt），为 Agent 工作区提供毫秒级快照与回滚能力。
    

### **核心组件版本**

| **组件** | **版本** | **说明** |
| --- | --- | --- |
| copilot-shell | 2.2.1-1 | 代替 Bash 作为系统交互入口 |
| os-skills | 0.3.0-1 | 系统级别技能扩展包 |
| agent-sec-core | 0.3.0-1 | 智能体安全核心，Agent 运行引入系统级安全加固 |
| loongshield | 1.2.0-1 | 提供系统安全加固能力 |
| agentsight | 0.3.1-1 | 提供 Token 可观测能力 |
| tokenless | 0.2.0-4 | 优化上下文 Token 消耗 |
| ws-ckpt | 0.1.0-1 | 为 Agent 工作区提供毫秒级快照与回滚能力 |
| skillfs | 0.1.2-1 | 提供技能空间的智能感知能力 |
| skvm-bridge | 0.1.0-1 | 提供预编译技能的无感桥接能力 |

### **已知问题**

Tokenless 和 AgentSecCore 提供的 OpenClaw 插件仅支持 2026.04.23 及之前版本。对于 2026.04.23 之后的版本，请在插件安装后，手动增加 activation.onCapabilities 配置。

-   Tokenless 插件配置位于：~/.openclaw/extensions/tokenless/openclaw.plugin.json
    
-   Agent-Sec-Core 插件配置位于：~/.openclaw/extensions/agent-sec/openclaw.plugin.json
    

```
{
  "id": "xxx",
  "name": "xxx",
  "version": "x.y.z",
  // ... 其他原有的配置 ...
  "activation": {
    "onCapabilities": ["hook"]
  }
}
```

## **Agentic OS (**ANOLISA**) 0.2**

### **发布信息**

**发布时间**：2026-04-15

**镜像 ID**：aliyun\_4\_x64\_20G\_agentic\_alibase\_20260416.vhd

### **更新内容**

Agentic OS 0.2 基于 Alibaba Cloud Linux 4.0.3。核心组件功能更新：

-   小规格实例（2C2G）初始可用内存提升20%~30%，OpenClaw 并发会话数量提升 200+%、Agent 冷启动时间显著降低；
    
-   Copilot Shell 认证界面全面升级，内置多种模型提供商快捷配置，Aliyun 认证支持 RAM 角色一键授权；
    
-   AgentSight 新增可视化面板，提供 Agent 实时健康监控、离线告警、卡死进程重启能力，支持会话、对话级的 Token 消耗分析、Agent轨迹分析；
    
-   AgentSecCore 支持 Skill 完整性自动化校验（签名校验）；
    
-   OS Skills 内置技能“sysom-diagnosis”支持完整系统诊断能力；
    
-   新增 Tokenless 优化组件，通过模式压缩、响应压缩及命令重写三大核心策略，降低上下文窗口的 Token 消耗并提升运行效率。
    

### **核心组件版本**

| **组件** | **版本** | **说明** |
| --- | --- | --- |
| copilot-shell | 2.0.4.1-1 | 代替 Bash 作为系统交互入口 |
| os-skills | 0.2.2-1 | 系统级别技能扩展包 |
| agent-sec-core | 0.2.0-2 | 智能体安全核心，Agent 运行引入系统级安全加固 |
| loongshield | 1.1.1-4 | 提供系统安全加固能力 |
| agentsight | 0.2.2-1 | 提供 Token 可观测能力 |
| tokenless | 0.1.0-3 | 优化上下文 Token 消耗 |

## **Agentic OS (**ANOLISA**) 0.1**

### **发布信息**

**发布时间**：2026-03-30

**镜像 ID**：aliyun\_4\_x64\_20G\_agentic\_20260329.vhd

### **更新内容**

Agentic OS 0.1 基于 Alibaba Cloud Linux 4.0.2。相比于标准镜像，Agentic OS 镜像有以下修改：

-   预装 Copilot Shell，并替换为用户默认登录 Shell。提供系统级别原生 AI 能力；
    
-   预装 OS Skills。提供部署、运维、诊断等常用系统能力；
    
-   预装常用工具集。扩展 Agent 能力；
    
-   预装 Python 3.11、NodeJS 22；
    
-   集成龙盾安全组件；
    
-   集成 AgentSecCore Agent 安全组件；
    
-   集成 SysAK 可观测能力，支持时间维度的 Token 消耗分析和行为审计功能。
    

### **核心组件版本**

| **组件** | **版本** | **说明** |
| --- | --- | --- |
| copilot-shell | 2.0.1-1 | 代替 Bash 作为系统交互入口 |
| os-skills | 0.0.3-1 | 系统级别技能扩展包 |
| agent-sec-core | 0.0.9-1 | 智能体安全核心，Agent 运行引入系统级安全加固 |
| loongshield | 1.1.1-4 | 提供系统安全加固能力 |
| sysak | 3.12.0-1 | 提供 AgentSight Token 可观测能力 |
