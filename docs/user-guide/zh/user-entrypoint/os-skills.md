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

## 有界电子表格分析

通过 `--max-rows N` 以可预测的分析记录数检查大表。此正整数上限独立用于每个选中的 Excel 工作表，或单个 CSV/TSV 表格：

```bash
python3 SKILL_DIR/scripts/xlsx_reader.py export.csv --max-rows 1000 --json
python3 SKILL_DIR/scripts/xlsx_reader.py workbook.xlsx --sheet Sales --max-rows 1000 --quality
```

读取器向 pandas 最多请求 `N+1` 条数据记录，用额外记录检测截断，然后分析前 N 条。截断表会按 N 条记录重新读取，避免用于检测的额外记录改变已分析值或列类型。表头不占预算；跨行的带引号 CSV 字段仍按一条记录计数。空表、较短表和恰好 N 行的表报告未截断。此功能限制载入的 DataFrame 行数，而不保证工作簿解析的字节内存上限。

提供上限后，JSON 增加 `sampling`，包含 `scope: analyzed_rows_only`、`max_rows_per_sheet` 以及每张表的 `rows_analyzed`/`truncated`。文本报告标明分析行数及仍有额外数据的工作表。结构、缺失值百分比、质量发现与统计都只描述已分析的前缀，读取器不会据此推断完整文件的行数或质量。已读记录以外的错误或编码问题可能不会被发现。

省略此选项时保持原有完整数据行为与报告结构。与 `--sheet`、`--quality`、Unicode、带引号记录及自动文本解码兼容，原文件字节保持不变。此模式选择前几行，不是随机或代表性抽样。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
