# AgentSight

[English](../../en/agentsight/index.md)

AgentSight 是面向 AI Agent 的零侵入可观测性工具。它把 eBPF 探针挂到已经在运行的进程上，记录 LLM
调用、Token 消耗、工具调用和会话中断——不需要改 Agent 代码、提示词或配置。

![AgentSight Agent 可观测页面](../../../images/agentsight/zh/dashboard-observability.png)

## 快速上手

这一页带你从一台空机器走到「Dashboard 上能看到自己的 Agent 流量」。下面的命令在一台 Alibaba Cloud
Linux 4（内核 6.6）主机上以 AgentSight 0.11 验证过；示例输出保留了真实的排版格式，但 ID 一律用占位
值、数字一律取整，因此你看到的都不是真实采集结果。

### 1. 检查前置条件

```bash
uname -r                                  # 内核必须 >= 5.8
ls /sys/kernel/btf/vmlinux                # BTF 必须存在
id -u                                     # 追踪需要 root（0）或 CAP_BPF
```

如果 `/sys/kernel/btf/vmlinux` 不存在，说明内核编译时没开 BTF，eBPF 探针无法加载，需要换用带
`CONFIG_DEBUG_INFO_BTF=y` 的内核。

### 2. 安装

```bash
# 推荐：system 模式，因为 eBPF 需要 root
sudo anolisa install agentsight

# 已配置仓库的 Alibaba Cloud Linux / Anolis 也可以
sudo yum install agentsight

# 开发者从源码构建
cd src/agentsight && make build-all
```

> 源码构建请用 `make build-all`，它会依次构建 Dashboard 前端、主二进制和 `agentsight-enforcer`。
> 只跑 `make build` 会跳过 enforcer，之后每次启动 `serve` 都会打印
> `AgentSight enforcement unavailable`。

### 3. 启动服务

```bash
sudo systemctl enable --now agentsight.service
systemctl is-active agentsight.service
```

安装包里的 unit 会启动一个守护脚本，同时拉起两个工作进程：`agentsight trace`（eBPF 采集）和
`agentsight serve --host 0.0.0.0`（API + Dashboard）；如果装了 enforcer 包，它还会顺带拉起
`agentsight-enforcer.service`。

数据写入 `/var/log/sysak/.agentsight`，且使用私有 umask，因此查询服务产生的数据需要 `sudo`。

### 4. 产生一些流量

AgentSight 只记录被识别为 Agent 的进程，所以要先跑一个 Agent 任务。cosh、Claude Code、Codex、
Qwen Code、OpenClaw、Hermes、AgentScope 都可以。下面用 cosh-ng 的 headless 模式举例：

```bash
cosh-core --headless --approval-mode trust "用一句话说明 Linux 中 load average 的含义"
```

趁进程还在跑，确认它被识别到了：

```bash
$ sudo agentsight discover
已发现 AI Agent（共 1 个）:
============================================================

  CoshNG [PID: 10000]
    类别: custom
    命令:  /usr/libexec/anolisa/cosh-ng/cosh-shell ...

总计: 1 个 Agent
```

### 5. 确认数据落库

```bash
$ sudo agentsight summary --last 24
AgentSight Summary (last 24h)

Sessions      10
  Tokens      100.0K in / 10.0K out / 110.0K total

Interruptions 1
  critical    0
  high        0
  medium      1
  low         0

Tokenless     10% saved (110.0K -> 99.0K, 20 ops)
```

`summary` 是最快的体检命令：会话数与 Token、按严重级别分组的中断、以及装了 Tokenless 时的节省量。
各数据源互相独立降级——某个数据库缺失只会让对应行显示为 0，不会让整份报告失败。

### 6. 打开 Dashboard

```bash
$ sudo agentsight dashboard --no-open

AgentSight 仪表盘状态
=====================

  认证:    已启用
  本机:    http://127.0.0.1:7396 (无需认证)
  局域网:   http://192.168.1.10:7396/?token=<TOKEN>
  公网:    http://203.0.113.10:7396/?token=<TOKEN>
```

- 在本机直接打开 `http://127.0.0.1:7396`，本机回环访问免认证。
- 从自己的笔记本访问，请使用打印出来的带 `?token=…` 的地址，并确认防火墙或安全组放行 TCP 7396。

没有有效令牌时，Dashboard 显示登录页：

![AgentSight Dashboard 登录页](../../../images/agentsight/zh/dashboard-login.png)

把 `agentsight dashboard --no-open` 打印的令牌粘进去，就会进入 Agent 看板
（`http://127.0.0.1:7396/#/health`）——实时 Agent 健康状态与尚未解决的中断收件箱。

会话分析页有自己独立的地址：在导航栏选择 **Agent 可观测**，或直接打开
`http://127.0.0.1:7396/#/observability`。这里按会话列出 Token 汇总和中断标记，点击某一行会展开
该会话下的对话：

![会话列表与展开的会话](../../../images/agentsight/zh/dashboard-session-expanded.png)

任意一行的**详情**按钮可以打开该会话或对话的逐步轨迹：

![会话轨迹：步骤与工具调用](../../../images/agentsight/zh/dashboard-session-trajectory.png)

### 7. 记住关键路径

| 内容 | 路径 |
|---|---|
| 二进制 | `/usr/local/bin/agentsight` |
| 配置文件 | `/etc/agentsight/config.json` |
| 数据库与 Dashboard 令牌 | `/var/log/sysak/.agentsight/` |
| systemd unit | `agentsight.service`、`agentsight-enforcer.service` |
| Dashboard 默认端口 | 7396 |

### 下一步

- 自己的 Agent 没被识别 → [Agent 发现规则](configuration.md#agent-发现规则)
- 任务失败或卡住 → [中断检测](interruption-detection.md)
- 想看全部参数 → [CLI 参考](cli-reference.md)
- 想逐页了解界面 → [Dashboard 指南](dashboard.md)
- 完全没有数据 → [排查](troubleshooting.md)

## 从这里开始

- [快速开始](index.md)——安装、启动追踪、产生第一条会话、打开 Dashboard。
- [Dashboard 指南](dashboard.md)——令牌访问方式与逐页说明。
- [CLI 参考](cli-reference.md)——所有命令、参数与真实输出。
- [配置](configuration.md)——配置文件、功能开关、Agent 发现规则。

## 你能用它做什么

| 目标 | 阅读 |
|---|---|
| 看清有哪些 Agent 在跑、各花了多少 Token | [快速开始](index.md) |
| 查清某个 Agent 任务为什么卡住、失败或空转 | [中断检测](interruption-detection.md) |
| 逐步回放一次会话（提示词、工具调用、结果） | [Dashboard 指南](dashboard.md#轨迹查看) |
| 让 AgentSight 认识一个它还不识别的 Agent | [配置](configuration.md#agent-发现规则) |
| 以服务、容器或 macOS 形态运行 | [部署](deployment.md) |
| 从脚本、Prometheus 或其他系统取数据 | [数据与存储](data-and-storage.md) |
| 与 Tokenless、agent-sec-core、cosh 配合 | [集成](integrations.md) |
| 解决“没数据”“401”“端口打不开” | [排查](troubleshooting.md) |

## 能力一览

| 能力 | 具体价值 |
|---|---|
| 零侵入采集 | eBPF uprobe 从 TLS 调用中读取明文，Agent 侧无需 SDK、代理或环境变量 |
| Agent 自动发现 | 按命令行识别 Agent 进程（cosh、Claude Code、Codex、Qwen Code、OpenClaw、Hermes、AgentScope，也支持自定义规则） |
| Token 计账 | 按 Agent、会话、对话、模型统计输入/输出/缓存 Token |
| 会话与对话视图 | 会话包含多个对话；每个对话保留自己的 LLM 调用、消息与工具调用 |
| 中断检测 | 18 种中断类型（崩溃、超时、限流、上下文溢出、死循环、工具失败……），带严重级别和根因证据 |
| 轨迹导出 | 任意会话、对话或单次调用都能导出为 ATIF v1.7 JSON |
| Dashboard | Web 界面：时间范围筛选、Token 曲线、延迟分位、中断标记、逐步回放 |
| 机器可读输出 | 多数命令支持 `--json`，另有 Prometheus `/metrics` 和文档化的 HTTP API |

## 前置条件

| 条件 | 要求 |
|---|---|
| 操作系统 | Linux（x86_64）；macOS 只有精简的轨迹采集模式 |
| 内核 | >= 5.8，且开启 BTF |
| 权限 | `agentsight trace` 需要 root（或 `CAP_BPF` + `CAP_PERFMON`） |
| 安装模式 | system 模式——eBPF 需要 root |
| 磁盘 | `/var/log/sysak/.agentsight` 下几百 MB，有容量上限，见[数据与存储](data-and-storage.md) |

> **macOS**：只提供 `agentsight trace`（扫描本地 Agent JSONL 会话文件，无 eBPF）和
> `agentsight serve`（Dashboard）。其余命令都依赖 Linux 上的 eBPF 流水线。

## 工作方式

```
Agent 进程 ──TLS 读写──▶ eBPF uprobe ─┐
Agent 进程 ──execve/exit─▶ eBPF 探针  ─┼─▶ 解析 ─▶ 聚合 ─▶ 分析
                                       │   (HTTP/SSE) (请求↔响应) (Token、审计)
                                       │                            │
                                   ring buffer                      ▼
                                                            GenAI 语义事件
                                                                    │
                                    ┌───────────────────────────────┼──────────────┐
                                    ▼                               ▼              ▼
                              SQLite 数据库                    中断检测器      外部日志导出
                                    │                               │
                                    └──────▶ HTTP API + Dashboard ◀─┘
```

真正干活的是两个进程，安装包里的服务会同时拉起它们：

| 进程 | 职责 |
|---|---|
| `agentsight trace` | 加载 eBPF 探针、发现 Agent、把事件写入 SQLite（需 root） |
| `agentsight serve` | 基于同一批 SQLite 提供 HTTP API 与 Dashboard |

更细的设计见源码目录中的 [ARCHITECTURE.md](../../../../src/agentsight/docs/ARCHITECTURE.md)。

## 术语

| 术语 | 含义 |
|---|---|
| 会话（Session） | Agent 自己标识的一次运行（来自 Agent 的 `session_id`，例如一次 cosh 会话） |
| 对话（Conversation） | 会话内的一次请求-响应循环，含其中的工具调用 |
| 调用（Trace） | 一次被捕获的 LLM HTTP 调用（请求 + 流式响应） |
| 中断（Interruption） | 被检测到的对话异常结束或停滞，带类型和严重级别 |
| Agent 名称 | 发现规则给进程打的标签，如 `CoshNG`、`Claude`、`Codex` |
| 轨迹（Trajectory） | 以 ATIF v1.7 格式导出的会话或对话，可用于回放和离线分析 |

## 安装与首次运行

```bash
# 必须 system 模式——eBPF 需要 root
sudo anolisa install agentsight

# 同时启动追踪与 Dashboard
sudo systemctl enable --now agentsight.service

# 打印 Dashboard 地址与访问令牌
sudo agentsight dashboard --no-open
```

完整流程与真实输出见[快速开始](index.md)。

## 参考页面

| 页面 | 内容 |
|---|---|
| [快速开始](index.md) | 安装、验证采集、第一次打开 Dashboard |
| [Dashboard 指南](dashboard.md) | 认证方式与所有 Dashboard 页面 |
| [CLI 参考](cli-reference.md) | `trace`、`serve`、`dashboard`、`token`、`audit`、`discover`、`metrics`、`summary`、`interruption`、`skill-metrics` |
| [配置](configuration.md) | `config.json` 结构、功能开关、运行时上限、发现规则 |
| [中断检测](interruption-detection.md) | 18 种中断类型与排查流程 |
| [部署](deployment.md) | systemd、前台、容器/Sidecar、macOS、升级、卸载 |
| [数据与存储](data-and-storage.md) | 数据库、保留策略、HTTP API、Prometheus、ATIF 导出 |
| [集成](integrations.md) | Tokenless、agent-sec-core、enforcer、cosh、Prometheus |
| [排查](troubleshooting.md) | 没数据、401、端口不通、数据库增长 |
