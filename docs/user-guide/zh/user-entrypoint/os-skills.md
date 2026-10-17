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

## PDF 表单字段分析

通过 `--form-fields --format json` 提取已填写 PDF 表单中保存的值。可选的逐页 `form_fields` 列表包含每个选中页面上的 AcroForm 字段外观：

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f application.pdf --format json --form-fields
python3 SKILL_DIR/scripts/read_pdf.py -f application.pdf --format json --form-fields -p "1-2" --metadata
```

每条记录包含 `name`、`label`、`type`、`type_id`、保存的 `value`、`flags`、`rect` 和 `xref`。矩形用包含四个数值的 JSON 数组表示。选择字段还包含 `choices`；复选框和单选按钮在可用时包含 `button_states`。`Yes`、`Off` 等值保留为实际保存的状态，不根据页面文本猜测。签名字段包含 SDK 的 `signed` 状态，此状态不验证签名的有效性或可信性。

逻辑字段可在多个位置或页面出现，因此每个外观保留为独立记录，归属于其页面。保留带点号的层级名称、Unicode 值、空白字段及选择项信息。没有字段的页面使用 `form_fields: []`。页面选择与文档元数据照常工作；省略此选项时保持默认文本和 JSON 输出不变。

读取器在关闭文档前复制值，PDF 原文件字节保持不变。不会填写、重置字段，执行嵌入的 JavaScript，或从外观文本推断表单值。普通注释提取是独立视图，因为 widget 表示表单字段而非普通注释。此模式要求 JSON 及支持 widget、按钮状态的 PyMuPDF 后端。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
