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

通过 `anolisa install` 安装的技能位于 `/usr/share/anolisa/skills/`，并由 Copilot Shell 自动发现。

开发者也可以从源码检出目录安装供 cosh 使用的技能集合：

```bash
make -C src/os-skills install
```

普通用户继续使用原有的 `~/.copilot-shell/skills/` 目录；root 默认安装到
`/usr/local/share/anolisa/skills/`。系统资源的 `AWPRJ` 默认为 `anolisa`，可显式覆盖。
使用 `SKILLS_DIR=/absolute/path` 可选择其他技能集合目录；卸载时使用相同的值。
启动新的 cosh 会话后，运行 `/skills` 检查已安装的技能。

`INSTALL_PROFILE=user` 的默认路径由 `HOME` 推导；`HOME` 未设置、为空、为相对路径
或为 `/` 时安装直接失败，不会写到 `/` 下。没有家目录时，请显式传入 `PREFIX`、
`DATADIR` 和 `SKILLS_DIR`。

技能同名时，本地系统副本优先于 `/usr/share/anolisa/skills/` 中的 RPM 副本。
同一集合应选择一种交付方式；管理旧的源码安装时，将 `SKILLS_DIR` 指向原目录。

`make uninstall` 只删除 `make install` 记录在
`<datadir>/anolisa/components/os-skills/install-manifest` 中的技能目录。
缺少该记录时不会删除任何内容，并以非零状态退出，因此不会误删 os-skills
从未安装过的同名技能目录。`make install` 反向同理：遇到不是自己安装的同名目录
会直接拒绝，只有显式传入 `REPLACE_UNOWNED=1` 才会整体替换并记录，供后续卸载删除。
只有 `1` 会开启替换；`0` 或不设置时保持拒绝，其他取值会在改动任何内容前报错。
因此在此版本之前完成的安装，需要先执行一次 `REPLACE_UNOWNED=1 make install`，
之后才能正常卸载。被记录的目录整体归 os-skills 管理，每次安装都会整体替换，
请勿在这些目录中放置个人文件，自建技能请使用其他名称。安装失败或被中断时会放回原有技能，
记录保持不变；若记录已写入新技能后才被中断，则保留新安装的技能，并提示再次执行 `make install`
完成安装。若某一步无法撤销（例如集合目录变为只读），错误信息会指出遗留内容：
未能删除的新副本，或仍保存原有技能的 `.os-skills.*/.old` 目录。记录会保留历次安装的技能名，
新版本移除的技能在卸载时也会一并删除。
RPM 安装请通过包管理器卸载，raw `anolisa install` 部署请使用
`anolisa uninstall os-skills`，而不是 `make uninstall`。

一份记录只对应一个集合：`make install` 会拒绝记录中未登记的目录，
`make uninstall` 会连同技能一起删除记录，因此同时维护多个集合时请为每个
集合单独指定 `MANIFEST=`。卸载其中一个集合不会影响其他集合的记录与共享组件元数据。

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
