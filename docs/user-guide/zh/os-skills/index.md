# OS 技能库

OS Skills 是面向操作系统运维场景的 AI Agent 技能库。它提供预构建的技能，使 Agent 能够完成系统管理、内核研发、云上运维和文档处理等任务。

---

## 概述

每个技能都是 `src/os-skills/` 下的一个独立目录，并按类别归组：

| 类别 | 目录 | 覆盖范围 |
|------|------|----------|
| **AI 工具** | `ai/` | AI 编码工具的安装与配置、MCP 服务配置 |
| **阿里云** | `aliyun/` | 通过阿里云 CLI 管理 ECS 实例生命周期 |
| **DevOps** | `devops/` | GitHub 工作流、内核研发、SysOM 与 AgentSight 诊断 |
| **监控与性能** | `monitor-perf/` | 预留给监控与性能类技能，当前尚未包含技能 |
| **其他** | `others/` | 产品指南、技能管理，以及图片、PDF、表格处理工具 |
| **安全** | `security/` | Alibaba Cloud Linux CVE 漏洞查询 |
| **系统管理** | `system-admin/` | ALinux 系统管理、备份恢复、内核调优与升级、存储扩容、Shell 脚本 |

---

## 安装

```bash
anolisa install os-skills
```

技能会被安装到 `/usr/share/anolisa/skills/`，并由 Copilot Shell 自动发现。

---

## 快速开始

安装后，OS Skills 可供任何 ANOLISA 兼容的 Agent 运行时使用。Agent 可通过自然语言调用技能：

```
> "云盘扩容后帮我扩分区并扩展 XFS 文件系统"
> "查一下这个 CVE 是否影响我的 Alibaba Cloud Linux 主机"
> "把 report.pdf 里的表格提取出来"
```

---

## 包含的技能

下表中的名称即技能目录名，每个目录内的 `SKILL.md` 定义该技能。

### AI 工具（`ai/`）

| 技能 | 说明 |
|------|------|
| `install-claude-code` | 在 RPM 系 Linux 上安装并配置 Claude Code |
| `install-hermes` | 一键安装并配置 Hermes Agent |
| `install-openclaw` | 使用阿里云百炼以非交互方式安装并配置 OpenClaw |
| `install-qwenpaw` | 在 Alibaba Cloud Linux 4 上部署 QwenPaw AI 助理，包含模型 API Key 与钉钉频道配置 |
| `install-tokenless` | 安装并配置 Tokenless（LLM token 优化工具集） |
| `qwenpaw-usage` | QwenPaw 命令行使用技巧：心跳配置、定时任务、Docker 打包、工作区迁移 |
| `setup-mcp` | 在 Copilot Shell 中添加并配置 MCP 服务，自动转换格式并保证 JSON 正确 |

### 阿里云（`aliyun/`）

| 技能 | 说明 |
|------|------|
| `aliyun-ecs` | 通过 `aliyun` 命令行管理弹性计算服务：实例、磁盘、快照、镜像、安全组、密钥对与弹性网卡 |

### DevOps（`devops/`）

| 技能 | 说明 |
|------|------|
| `github` | 通过 `gh` 命令行与 GitHub 交互：issue、pull request、CI 运行与 API 调用 |
| `kernel-dev` | Linux 内核研发自动化：SRPM 与上游两种构建方式、内核 module 开发环境、依赖安装 |
| `sysom-agentsight` | 查询 AgentSight 的 token 消耗、token 节省、审计事件与会话中断事件 |
| `sysom-diagnosis` | 诊断 Linux 服务器性能与稳定性问题并给出处理建议 |

### 其他（`others/`）

| 技能 | 说明 |
|------|------|
| `anolisa-guide` | 解答 ANOLISA、Alibaba Cloud Linux 4 Agentic Edition 与 Copilot Shell 相关问题 |
| `anolisa-register` | 管理 Agentic OS 注册状态，包含加入与退出共建计划 |
| `clawhub-skill-mng` | 搜索、安装、卸载、更新与管理 Agent 技能 |
| `cosh-guide` | Copilot Shell（cosh）命令行用户指南与帮助 |
| `humanizer` | 去除文本中的 AI 写作痕迹，使表达更自然 |
| `image-gen` | 通过 DashScope/Qwen 依据文本提示词生成图片 |
| `pdf-reader` | 从 PDF 文件中提取文本与表格 |
| `xlsx` | 打开、创建、读取、分析、编辑与校验 Excel 及 CSV/TSV 文件 |

### 安全（`security/`）

| 技能 | 说明 |
|------|------|
| `alinux-cve-query` | 查询 Alibaba Cloud Linux 的 CVE 漏洞信息与对应安全公告 |

### 系统管理（`system-admin/`）

| 技能 | 说明 |
|------|------|
| `alinux-admin` | 管理 Alibaba Cloud Linux 4：Shell 脚本、systemd 服务与网络 |
| `backup-restore` | 备份与恢复工作区、用户数据、操作系统以及阿里云快照 |
| `ktuner` | 自动调优内核参数，输出调优建议，支持一键应用与回滚 |
| `regex-mastery` | 编写、调试与优化正则表达式 |
| `shell-scripting` | 编写 bash 或 zsh 脚本：参数解析、错误处理与命令行自动化 |
| `storage-resize` | 云盘扩容后完成分区调整与 XFS/EXT4/Btrfs 文件系统扩容 |
| `upgrade-alinux-kernel` | 升级 Alibaba Cloud Linux 实例的 Linux 内核 |

---

## 与 Agent 运行时集成

OS Skills 与 cosh 及其他 ANOLISA 兼容运行时自动集成。技能在启动时被发现并加入 Agent 的工具清单。

```bash
# 验证技能已加载
anolisa status os-skills
```

---

## 参见

- [anolisa CLI](../getting-started/anolisa-cli.md)
