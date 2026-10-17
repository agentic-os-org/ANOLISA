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

## OpenClaw 模型选择

OpenClaw 安装脚本允许在 `--model-id` 和可重复的 `--extra-model` 参数中使用可选的
provider 前缀。它先移除该前缀，再对主模型、额外模型和计费计划目录中的选择去重，
保留首次出现的顺序。`vendor/custom` 与 `custom` 只生成一个名为 `custom` 的 provider
模型；Agent 引用使用选定的 `--provider-id`，例如 `bailian/custom`。
带前缀的额外模型命中目录已有模型时，也不会生成重复条目。

```bash
python3 src/os-skills/ai/install-openclaw/scripts/install_openclaw.py --api-key-env BAILIAN_API_KEY --extra-model vendor/custom --extra-model custom --dry-run
```

除非指定 `--skip-preflight`，安装脚本在 `--dry-run` 中仍会执行模型预检。
前缀规范化不会选择第二个 provider。

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
