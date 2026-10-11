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

## 电子表格编辑预览

解包 XLSX 工作簿后，可先预览行引用移动，再应用编辑：

```bash
python3 src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py /tmp/xlsx_work insert 5 2 --dry-run
python3 src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py /tmp/xlsx_work insert 5 2
```

参数依次为解包目录、`insert` 或 `delete`、起始行和行数。`--dry-run` 执行相同的
转换并报告各部分及总修改计数，同时保持每个文件不变。它覆盖辅助脚本已有的工作表、
图表、表格和透视缓存引用。命名范围、结构化表格引用和外部工作簿链接仍需手动检查。
查看预览后，去掉 `--dry-run` 运行命令应用修改，然后重新打包并验证工作簿。
预览成功退出 0；CLI 语法无效退出 2。

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
