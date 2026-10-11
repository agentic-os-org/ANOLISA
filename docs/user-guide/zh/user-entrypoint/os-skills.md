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

### 系统管理

| 技能 | 说明 |
|------|------|
| `disk-usage` | 检查文件系统磁盘使用 |
| `service-ctl` | 启动/停止/重启系统服务 |
| `process-mgmt` | 列出和管理进程 |
| `user-mgmt` | 用户和组管理 |
| `package-ops` | 包安装/移除/查询 |

### DevOps 自动化

| 技能 | 说明 |
|------|------|
| `container-ops` | Docker/Podman 容器管理 |
| `log-analysis` | 搜索和分析系统日志 |
| `network-diag` | 网络诊断（ping、traceroute、端口检查） |
| `cron-mgmt` | Cron 任务管理 |

---

## 与 Agent 运行时集成

OS Skills 与 cosh 及其他 ANOLISA 兼容运行时自动集成。技能在启动时被发现并加入 Agent 的工具清单。

```bash
# 验证技能已加载
anolisa status os-skills
```

---

## 配置

配置文件：`~/.config/os-skills/config.toml`

```toml
[skills]
# 启用的技能类别
enabled = ["system", "devops"]

[safety]
# 对破坏性操作要求确认
confirm_destructive = true
```

---

## PDF 内嵌附件

发现 PDF 中内嵌的支持文件，并选择提取其中一个，保持文档不变。两种附件模式均要求 JSON：

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f portfolio.pdf --format json --attachments
python3 SKILL_DIR/scripts/read_pdf.py -f portfolio.pdf --format json --extract-attachment 0 --output recovered.bin
```

文档级 `attachments` 列表包含稳定的零起始 `index` 及 SDK 元数据：内嵌名称、文件名、描述、解压后 `size`、存储 `length`、日期及可用的 portfolio、校验值信息。索引按附件物理顺序排列，能区分重复名称。字符串和 PDF 日期保留 SDK 表示；存储的校验值是元数据，不是验证结果。页面选择影响文本页面，附件仍属于整个文档。元数据发现不会载入附件内容。

从列表选择索引并显式提供 `--output`。读取器提取精确字节，支持压缩二进制、UTF-8 和空内容；PDF 内的文件名不会被用于决定文件系统目标。JSON 还报告 `extracted_attachment`，包含所选索引、输出路径和字节大小。选中的内容会载入内存，不会执行附件程序或动作。

输出父目录必须存在。提取会发布完整的新文件，拒绝所有既有输出路径，包括源 PDF 和并发创建的文件。发布要求同文件系统的硬链接支持；失败时不会留下部分新输出，并会清理私有暂存。未知索引、缺少输出路径或不兼容的文本模式选项会明确报错。省略附件选项时保持原文本、JSON 结构；源 PDF 字节保持不变。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
