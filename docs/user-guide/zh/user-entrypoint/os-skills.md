# OS 技能库

OS Skills 是面向 AI Agent 的系统管理与 DevOps 技能库。它提供预构建的技能，使 Agent 能够执行常见的系统管理和自动化任务。

---

## 概述

OS Skills 覆盖三大领域：

- **系统管理** — 用户管理、服务控制、包管理、文件系统操作
- **云集成** — 云资源查询、实例管理、网络配置
- **DevOps 自动化** — CI/CD 流水线管理、容器操作、部署工作流

---

## 安装

```bash
anolisa install os-skills
```

---

## 快速开始

安装后，OS Skills 可供任何 ANOLISA 兼容的 Agent 运行时使用。Agent 可通过自然语言调用技能：

```
> "检查所有已挂载文件系统的磁盘使用情况"
> "重启 nginx 服务"
> "显示运行中的容器及其资源使用"
```

---

## 技能分类

内置技能按 `src/os-skills/` 下的分类目录分组：

### AI 工具

| 技能 | 说明 |
|------|------|
| `install-claude-code` | 安装和配置 Claude Code IDE |
| `install-hermes` | 安装和配置 Hermes Agent |
| `install-openclaw` | 安装和配置 OpenClaw |
| `install-qwenpaw` | 部署 QwenPaw AI 助手（支持钉钉集成） |
| `install-tokenless` | 安装和配置 Tokenless（LLM token 优化） |
| `qwenpaw-usage` | QwenPaw 使用指南（心跳、定时任务、打包部署） |
| `setup-mcp` | 在 Copilot Shell 中配置 MCP 服务器 |

### 系统管理

| 技能 | 说明 |
|------|------|
| `alinux-admin` | ALinux 4 系统管理（systemd、SSH、firewalld、NetworkManager） |
| `backup-restore` | 系统备份与恢复 |
| `ktuner` | 内核参数自动调优，输出建议并可应用，支持回滚 |
| `regex-mastery` | 正则表达式指南 |
| `shell-scripting` | Bash/Zsh 脚本编写与自动化 |
| `storage-resize` | 阿里云磁盘扩容（XFS/EXT4/Btrfs） |
| `upgrade-alinux-kernel` | ALinux 内核升级 |

### 开发运维

| 技能 | 说明 |
|------|------|
| `github` | 通过 `gh` CLI 进行 GitHub 工作流与集成 |
| `kernel-dev` | ALinux 4 内核研发自动化（SRPM 和 Upstream 方式） |
| `sysom-agentsight` | AgentSight token、审计与会话诊断查询 |
| `sysom-diagnosis` | SysOM 诊断与调优 |

### 阿里云

| 技能 | 说明 |
|------|------|
| `aliyun-ecs` | 通过阿里云 CLI 管理 ECS 实例生命周期 |

### 安全

| 技能 | 说明 |
|------|------|
| `alinux-cve-query` | 查询 Alibaba Cloud Linux CVE 漏洞信息 |

### 其他

| 技能 | 说明 |
|------|------|
| `anolisa-guide` | ANOLISA、Agentic OS、cosh、Copilot Shell 使用问答 |
| `anolisa-register` | 管理 ANOLISA 注册状态（加入/退出共建计划） |
| `clawhub-skill-mng` | 从 ClawHub 搜索、安装和管理 Agent 技能 |
| `cosh-guide` | Copilot Shell 用户指南与帮助 |
| `humanizer` | 去除文本中的 AI 生成痕迹，使行文更自然 |
| `image-gen` | 通过 DashScope/通义千问从文本生成图片 |
| `pdf-reader` | 从 PDF 文件提取文本 |
| `xlsx` | 打开、创建、编辑和校验 Excel/表格文件 |

---

## 与 Agent 运行时集成

OS Skills 与 cosh 及其他 ANOLISA 兼容运行时自动集成。技能在启动时被发现并加入 Agent 的工具清单。

```bash
# 验证技能已加载
anolisa status os-skills
```

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
