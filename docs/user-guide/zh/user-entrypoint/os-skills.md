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

## PDF 导航链接

将 `--links` 与 JSON 输出一起使用，可检查选中页中嵌入的导航目标。此操作仅读取元数据，不访问 URI 或打开目标文件。

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f handbook.pdf --format json --links --pages 1-3 --metadata
```

每个选中页增加 `links` 数组，无链接页使用 `[]`。记录保留 PyMuPDF 返回的字段：`kind` 表示动作类型，`from` 转为包含四个数字的热点矩形，点类型的 `to` 转为 `[x, y]`。非负目标 `page` 索引转为从 1 开始的页码，与页面报告一致；间接目标的负数标记保持不变。

URI 动作提供 `uri`；远程或启动动作可能提供 `file`。`xref`、`id`、`zoom` 等可选字段会被保留。符号目标保留原文字。根据引擎版本，远程间接目标可能采用符号 `to` 与 `page: -1`，也可能采用文件名或 URI 中的锚点。返回的表示形式与引擎坐标保持不变。

页面选择同时控制正文与链接，元数据仍可单独选择。省略选项时保留既有输出；文本模式会在加载引擎或访问文件前报错，原 PDF 保持不变。此功能覆盖页面链接动作，与目录书签和审阅批注分别使用。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
