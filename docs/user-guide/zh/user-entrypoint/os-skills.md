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

## 受密码保护的 Excel 分析

使用已知密码分析 OOXML 工作簿，无需创建未加密文件。只读读取器通过可选 `msoffcrypto-tool` 后端支持 `.xlsx` 和 `.xlsm`：

```bash
pip install pandas openpyxl msoffcrypto-tool
python3 SKILL_DIR/scripts/xlsx_reader.py protected.xlsx --password-env WORKBOOK_PASSWORD --json
python3 SKILL_DIR/scripts/xlsx_reader.py protected.xlsx --password-env WORKBOOK_PASSWORD --sheet Sales --quality
```

运行示例前在命令环境中设置 `WORKBOOK_PASSWORD`；此选项接收环境变量名称，不接收密码值。变量未设置或为空时为用法错误；此后端要求非空密码。读取器不会提示输入、寻找或存储密码，报告也不包含凭据。

已知密码解密在内存中完成，验证密码及支持的载荷完整性，原文件保持不变。全工作表和按名称选择沿用现有结构、质量与统计报告。普通 Excel 输入不依赖此可选后端，即使提供密码变量也如此。缺少凭据、后端，凭据错误或加密数据损坏时会报错，不会产生明文输出文件。

此选项仅用于 OOXML `.xlsx`/`.xlsm`；与 CSV、TSV 或旧版 XLS 一起使用会被拒绝。读取存储的表格值而不执行宏。工作表保护与文件加密不同，读取受保护工作表不需要文件密码。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
