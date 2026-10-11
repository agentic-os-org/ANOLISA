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

## OpenAI 兼容的 OpenClaw 预检

OpenClaw 安装器会在写入配置或启动 Gateway 之前，验证 `anthropic-messages`
（默认值）和 `openai-completions` 两种协议：

```bash
python3 SKILL_DIR/scripts/install_openclaw.py --billing payg \
  --api-key-env BAILIAN_API_KEY --provider-api openai-completions \
  --base-url https://dashscope.aliyuncs.com/compatible-mode/v1 --model-id "$OPENAI_MODEL"
```

将 `SKILL_DIR` 替换为已安装的 `ai/install-openclaw` 目录，并将 `OPENAI_MODEL`
设为配置端点支持的模型。通过 `--base-url` 提供完整的兼容 API Base URL；
选择该协议不会替换计费计划的默认 Base URL。预检在基础路径后追加
`/chat/completions`，保留查询参数；如果已提供完整端点，则不会重复追加路径。
请求使用 Bearer API Key、用户消息 `ping` 和 `max_tokens: 1`，超时由
`--preflight-timeout` 控制（默认 20 秒）。HTTP、认证或网络失败会在写入配置前停止，
并复用现有的提供商诊断。

`--skip-preflight` 可显式延后端点验证；未支持的提供商协议保留原有跳过提示。
`--dry-run` 仍会执行预检，除非显式跳过；`--precheck-only` 仅检查本地依赖，
不会发送模型请求。端点需要接受标准 Chat 请求；需要额外提供商参数的模型，
应通过其支持的配置进行验证。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [anolisa CLI](anolisa-cli.md)
