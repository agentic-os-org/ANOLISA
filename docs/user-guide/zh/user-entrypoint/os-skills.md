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

## CSV 和 TSV 编码

`xlsx` 技能包含 `xlsx_reader.py`，可读取电子表格导出数据而不修改源文件。
脚本需要 `pandas`；读取 Excel 文件还需要 `openpyxl`。在已安装的 `xlsx`
技能目录中，可指定已知的 Python 编码，避免旧版 CSV 或 TSV 数据的编码歧义：

```bash
python3 scripts/xlsx_reader.py export.csv --encoding cp1252 --json
python3 scripts/xlsx_reader.py export.tsv --encoding utf-16 --json
```

`--encoding CODEC` 仅使用指定编码。未知编码或解码失败时退出码为 1，错误输出到
stderr，不会回退到其他编码。该选项适用于 `.csv` 和 `.tsv`，Excel 输入会拒绝它。
不指定该选项时仍自动检测编码。

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

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
