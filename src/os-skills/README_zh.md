# OS Skills

[English](README.md)

面向 AI Agent 的运维技能集合，涵盖系统管理、监控、安全、DevOps 和云集成。OS Skills 是 [ANOLISA](../../README.md) 的技能库，安装到 `/usr/share/anolisa/skills/` 后由 Copilot Shell 自动发现。

## 技能分类

| 分类 | 目录 | 说明 |
|------|------|------|
| **AI 工具** | `ai/` | AI 编程工具集成（Claude Code、OpenClaw、QwenPaw、MCP 配置） |
| **系统管理** | `system-admin/` | 包管理、存储、网络、内核、Shell 脚本 |
| **开发运维** | `devops/` | Git 工作流、CI/CD、内核开发、系统诊断 |
| **阿里云** | `aliyun/` | ECS 实例管理、云网络、GPU/AI 部署 |
| **安全** | `security/` | CVE 查询、合规检查、系统加固 |
| **监控与性能** | `monitor-perf/` | sysAK 诊断、keentune 调优、sysctl 管理 |
| **其他** | `others/` | 实验性功能与第三方扩展 |

## 包含的技能

### AI 工具
- **install-claude-code** — 安装和配置 Claude Code IDE
- **install-qwenpaw** — 部署 QwenPaw AI 助手（支持钉钉集成）
- **install-openclaw** — 安装和配置 OpenClaw
- **install-tokenless** — 安装和配置 Tokenless（LLM token 优化）
- **qwenpaw-usage** — QwenPaw 使用指南
- **setup-mcp** — 在 Copilot Shell 中配置 MCP 服务器

### 系统管理
- **alinux-admin** — ALinux 4 系统管理（systemd、SSH、firewalld、NetworkManager）
- **backup-restore** — 系统备份与恢复
- **regex-mastery** — 正则表达式指南
- **shell-scripting** — Bash/Zsh 脚本编写与自动化
- **storage-resize** — 阿里云磁盘扩容（XFS/EXT4/Btrfs）
- **upgrade-alinux-kernel** — ALinux 内核升级

### 开发运维
- **github** — GitHub 工作流与集成
- **kernel-dev** — ALinux 4 内核研发自动化（SRPM 和 Upstream 方式）
- **sysom-agentsight** — 系统诊断工具
- **sysom-diagnosis** — SysOM 诊断与调优

### 阿里云
- **aliyun-ecs** — 通过阿里云 CLI 管理 ECS 实例生命周期

### 安全
- **alinux-cve-query** — 查询 Alibaba Cloud Linux CVE 漏洞信息

## 技能退场计划

OS Skills 计划将下列九个技能移出默认技能包，并逐步结束对内置副本的常规维护。
当前技能包仍包含这些技能。调整后，OS Skills 将更聚焦于系统运维，用户可按下表使用
各工作流程对应的官方工具、上游技能或发行版文档。

| 技能 | 迁移方式 |
|------|----------|
| `github` | 直接使用 `gh` 处理 issue、PR 和 CI。参见 [GitHub CLI 手册](https://cli.github.com/manual/)。 |
| `clawhub-skill-mng` | 使用官方 CLI 搜索、安装和更新技能。如果曾依赖内置技能的 cosh 配置，请核对注册表地址和安装路径。参见 [ClawHub CLI 指南](https://github.com/openclaw/clawhub/blob/main/docs/cli.md)。 |
| `humanizer` | 按需单独安装上游技能。参见 [Humanizer](https://github.com/blader/humanizer)。 |
| `image-gen` | 计划将百炼集成交由使用它的产品维护。直接调用 API 时，参见[阿里云百炼文生图指南](https://help.aliyun.com/zh/model-studio/text-to-image)。 |
| `pdf-reader` | 使用 Agent 自带的 PDF 工具或 PyMuPDF 集成。切换自动化流程前，验证页码选择、表格提取和 JSON 输出。参见 [PyMuPDF 文档](https://pymupdf.readthedocs.io/en/latest/)。 |
| `xlsx` | 通用表格任务可使用同源的上游技能。切换前，用实际样本验证编辑后的工作簿是否完整，以及能否在所用应用中打开和使用。参见 [MiniMax XLSX](https://github.com/MiniMax-AI/skills/blob/main/skills/minimax-xlsx/SKILL.md)。 |
| `regex-mastery` | 查阅所用工具或语言的正则表达式文档。`grep` 用法可参见 [GNU grep 参考](https://www.gnu.org/s/grep/manual/html_node/Regular-Expressions.html)。 |
| `shell-scripting` | 通用脚本编写可参见 [Bash 手册](https://www.gnu.org/s/bash/manual/bash.html)或 [Zsh 手册](https://zsh.sourceforge.io/Doc/Release/index.html)。ALinux 4 运维脚本可参见 [Bash 脚本编写指南](system-admin/alinux-admin/references/shell-scripting.md)。 |
| `upgrade-alinux-kernel` | 按当前安装版本查阅发行版的内核升级与恢复指引。参见 [Alibaba Cloud Linux 文档](https://help.aliyun.com/zh/alinux/)。 |

如果内置技能导致数据丢失或阻塞关键流程，请在 [GitHub issue](https://github.com/agentic-os-org/ANOLISA/issues)
中注明 OS Skills 版本和实际影响。安全漏洞请按[安全政策](../../SECURITY.md)报告。

## 技能格式

每个技能由独立目录组织，至少包含一个 `SKILL.md` 文件：

```
skill-name/
├── SKILL.md              # 必需：技能定义（YAML 前置元数据 + Markdown）
├── scripts/              # 可选：可执行脚本（.sh、.py）
├── reference/            # 可选：配置模板和参考文件
└── docs/                 # 可选：详细文档
```

### SKILL.md 结构

```yaml
---
name: my-skill
version: 1.0.0
description: 技能用途说明
layer: application          # application | system | core
lifecycle: production       # production | operations | usage | maintenance
tags: [example, demo]
platforms: [cosh, claude-code, gemini-cli]
---

# 我的技能

Markdown 内容，包含使用说明、前置条件和示例。
```

## 安装

### ANOLISA

```bash
anolisa install os-skills --backend raw
```

GitHub Release 和 Preview 工作流从同一个源码提交生成 Linux x86_64 与 ARM64
raw 压缩包。每个平台提供 `.tar.gz`、SHA256 校验和、CycloneDX SBOM 及其校验和。
压缩包将已跟踪的技能目录安装到 `{datadir}/skills`，无需编译可执行程序。
正式发布时 CI 校验 tag 与源码版本；本地发布流水线继续使用现有配置。

### 扩展安装

一键安装全部技能：

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills
```

或按分类单独安装：

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/ai
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/system-admin
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/devops
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/security
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/aliyun
```

或从本地路径安装：

```
/extensions install /path/to/os-skills
```

> **注意：** 扩展安装方式目前仅支持 Copilot Shell（cosh）。

### RPM 安装

```bash
sudo yum install os-skills
```

技能安装到 `/usr/share/anolisa/skills/`，由 Copilot Shell 自动发现。

### 手动安装

将技能目录复制到以下搜索路径之一：
1. 项目级：`.copilot-shell/skills/`
2. 用户级：`~/.copilot-shell/skills/`
3. 系统级：`/usr/share/anolisa/skills/`

## 编写新技能

1. 在对应分类目录下创建技能文件夹（如 `system-admin/my-skill/`）。
2. 添加包含 YAML 前置元数据和 Markdown 内容的 `SKILL.md`。
3. 可选添加 `scripts/` 和 `reference/` 目录。
4. 在 Copilot Shell 中测试技能。
5. 提交 Pull Request。

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。
