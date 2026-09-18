# ANOLISA 用户指南

[English](../en/README.md)

ANOLISA 为 AI Agent 提供完整的服务端运行时能力。通过 `anolisa` CLI 统一安装，各组件独立使用。

---

## 组件全景

```
┌────────────────────────────────────────────────────────────────────┐
│  Agent 应用层（cosh / OpenClaw / Hermes / 自定义）                 │
├────────────────────────────────────────────────────────────────────┤
│  用户入口点                                                        │
│  anolisa-cli · cosh · os-skills                                    │
├──────────────────────────────────┬─────────────────────────────────┤
│  Token 节省                       │  运行时                          │
│  tokenless · agent-memory        │  skillfs · ws-ckpt              │
├──────────────────────────────────┼─────────────────────────────────┤
│  Agent 可观测                     │  Agent 安全                      │
│  agentsight                      │  agent-sec-core                 │
└──────────────────────────────────┴─────────────────────────────────┘
```

---

## 文档目录

### 全局入口

| 文档 | 内容 |
|------|------|
| [安装与初始化](installation.md) | 从 CLI 到全栈组件的渐进式安装 |
| [故障排查](troubleshooting.md) | 跨模块常见问题与修复方案 |

### 用户入口点 `user-entrypoint/`

| 文档 | 组件 | 说明 |
|------|------|------|
| [anolisa CLI](user-entrypoint/anolisa-cli.md) | anolisa | 统一 CLI 组件管理 |
| [cosh-ng](user-entrypoint/cosh-ng/README.md) | cosh-ng | 集成 Agent runtime 的 AI 原生 Linux 终端 |
| [Copilot Shell](user-entrypoint/copilot-shell/QUICKSTART.md) | cosh | AI 终端助手与命令网关 |
| [OS 技能库](user-entrypoint/os-skills.md) | os-skills | 系统管理与 DevOps 技能 |

### 可观测性 `agent-observability/`

| 文档 | 组件 | 说明 |
|------|------|------|
| [AgentSight](agent-observability/agentsight/README.md) | agentsight | eBPF 追踪、Token 计账、Web Dashboard |
| [AgentSight 快速开始](agent-observability/agentsight/QUICKSTART.md) | agentsight | 安装、采集第一条会话、打开 Dashboard |
| [AgentSight Dashboard 指南](agent-observability/agentsight/dashboard.md) | agentsight | 令牌访问方式与逐页说明 |
| [AgentSight CLI 参考](agent-observability/agentsight/cli-reference.md) | agentsight | 全部命令与参数，附真实输出 |
| [AgentSight 配置](agent-observability/agentsight/configuration.md) | agentsight | 配置文件、功能开关、Agent 发现规则 |
| [中断检测](agent-observability/agentsight/interruption-detection.md) | agentsight | 18 种中断类型与排查流程 |
| [AgentSight 部署](agent-observability/agentsight/deployment.md) | agentsight | systemd、容器/Sidecar、macOS、升级、卸载 |
| [AgentSight 数据与存储](agent-observability/agentsight/data-and-storage.md) | agentsight | 数据库、保留策略、HTTP API、Prometheus、ATIF 导出 |
| [AgentSight 集成](agent-observability/agentsight/integrations.md) | agentsight | Tokenless、agent-sec-core、enforcer、cosh、Prometheus |
| [AgentSight 排查](agent-observability/agentsight/troubleshooting.md) | agentsight | 没数据、401、端口不通、数据库增长 |

### 安全 `agent-security/`

| 文档 | 组件 | 说明 |
|------|------|------|
| [AgentSecCore](agent-security/agent-sec-core/QUICKSTART.md) | agent-sec-core | 系统加固、代码扫描、提示词扫描、技能账本 |
| [OWASP Agentic Top 10 控制映射](agent-security/owasp-agentic-top10.md) | ANOLISA | 十类 Agentic 风险对应的安全控制、适用条件与证据 |
| [Code Scanner Hook 配置](agent-security/agent-sec-core/code-scanner.md) | agent-sec-core | 各 Agent 的 hook 模式、环境变量与 fallback 行为 |
| [Prompt Scanner](agent-security/agent-sec-core/prompt-scanner.md) | agent-sec-core | 提示词注入 / 越狱检测、模式与 verdict |
| [PII 检测](agent-security/agent-sec-core/pii-checker.md) | agent-sec-core | 个人数据/凭证检测与脱敏 |
| [资产验证](agent-security/agent-sec-core/asset-verification.md) | agent-sec-core | GPG 签名的 Skill 分发验证与发现结果 |
| [Skill Ledger 用户指南](agent-security/agent-sec-core/skill-ledger.md) | agent-sec-core | 技能账本完整性链与签名工作流 |
| [OpenClaw 兼容部署与升级](agent-security/agent-sec-core/openclaw-deploy.md) | agent-sec-core | OpenClaw 插件部署与升级指南 |
| [内部命令](agent-security/agent-sec-core/internal-commands.md) | agent-sec-core | hook 调用的隐藏命令契约与清单 |

### Token 节省 `token-saving/`

| 文档 | 组件 | 说明 |
|------|------|------|
| [Tokenless 快速开始](token-saving/tokenless/QUICKSTART.md) | tokenless | 安装、接入 Agent、首次压缩与验收 |
| [Tokenless 用户手册](token-saving/tokenless/user-manual.md) | tokenless | 能力边界、运行行为与任务导航 |
| [Tokenless Python SDK](token-saving/tokenless/sdk.md) | tokenless | 通用与 AgentScope 两层、Runtime 操作、统计与示例 |
| [Tokenless AgentScope SDK 集成](token-saving/tokenless/sdk/agentscope.md) | tokenless | 把 AgentScope 1.x、2.x 与 App 挂载到通用 SDK |
| [Tokenless Agent 集成](token-saving/tokenless/framework-integration.md) | tokenless | 产品 Adapter、Hook 与 Plugin |
| [Tokenless CLI 参考](token-saving/tokenless/cli-reference.md) | tokenless | Protocol Pipeline、直接压缩、Stash、MCP 与统计命令 |
| [Tokenless 效果度量](token-saving/tokenless/measuring-savings.md) | tokenless | 统计、diff、dry-run、AgentSight 与 SLS 度量 |
| [Tokenless 配置与数据隐私](token-saving/tokenless/configuration-and-privacy.md) | tokenless | 配置优先级、本地数据与敏感工作负载 |
| [Tokenless 故障排查](token-saving/tokenless/troubleshooting.md) | tokenless | Adapter、数据库、Stash、升级与卸载 |
| [Agent 记忆](token-saving/agent-memory.md) | agent-memory | 持久化记忆、MCP 工具、检索与数据主权控制 |

### 运行时 `runtime/`

| 文档 | 组件 | 说明 |
|------|------|------|
| [Blaze Sandbox Runtime](runtime/blaze.md) | blaze | Managed sandbox 的可选 VM 网络与周期存储制品同步 |
| [工作区快照](runtime/ws-ckpt.md) | ws-ckpt | 秒级快照创建/回滚，基于 btrfs COW |
| [技能文件系统](runtime/skillfs.md) | skillfs | FUSE 虚拟视图、渐进披露 |
| [SkillFS Kubernetes Sidecar](runtime/skillfs-kubernetes-sidecar.md) | skillfs | 在 Kubernetes 中以 FUSE Sidecar 运行 SkillFS |

---

## 术语速查

| 术语 | 含义 |
|------|------|
| 组件（Component） | 实现某项功能的软件单元，如 `tokenless` |
| 适配器（Adapter） | 将组件接入 Agent 框架的桥接包 |
| system mode | 需要 root 权限的安装模式（`sudo anolisa install`） |
| user mode | 安装到用户目录，无需 sudo |
