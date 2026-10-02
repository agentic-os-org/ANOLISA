# AgentSight CLI 参考

[English](../../../en/agent-observability/agentsight/cli-reference.md)

本页所有参数均取自 Linux 上的 `agentsight 0.11.x --help`。示例输出保留真实排版，但其中的 ID 全是占位
值、数字全是整数，均非真实采集结果。

## 通用约定

| 事项 | 说明 |
|---|---|
| 权限 | `trace` 需要 root（或 `CAP_BPF` + `CAP_PERFMON`）。查询类命令需要读取 `/var/log/sysak/.agentsight`，该目录归服务所有，请加 `sudo`。 |
| 配置文件 | 需要读取规则的命令支持 `--config`，默认 `/etc/agentsight/config.json`。`discover` 读取与 `trace` 相同的文件，因此会反映你的自定义规则。 |
| 数据位置 | 固定为 `/var/log/sysak/.agentsight`。`serve`、`dashboard`、`skill-metrics` 支持用 `--db` 指定其他数据库文件。 |
| 机器可读输出 | `token`、`audit`、`summary`、`interruption *`、`skill-metrics *` 都支持 `--json`。 |
| 输出语言 | `summary`、`metrics`、`interruption` 输出英文；`discover`、`token` 无论 locale 都输出中文。需要稳定文本时请用 `--json`（`discover` 没有该参数）。 |
| 平台 | macOS 上只有 `trace`（轨迹采集）和 `serve`。 |
| 示例 ID | 示例里的会话、对话、调用、中断 ID 都是占位值，请替换成你自己输出里的实际 ID。 |

```
$ agentsight --help
agentsight 0.11.x
AI Agent observability tool - trace processes, SSL traffic, and LLM API calls via eBPF

SUBCOMMANDS:
    audit            Query audit events
    dashboard        Display dashboard URL and ECS console access guide
    discover         Discover running AI agents on the system
    interruption     Query and manage session interruption events detected during agent conversations
    metrics          Print per-agent token usage metrics in Prometheus text format
    serve            Start the API server
    skill-metrics    Compute and display skill usage metrics
    summary          Print a unified summary of sessions, interruptions, and tokenless savings
    token            Query token consumption data
    trace            Trace agent activity (default)
```

## agentsight trace

加载 eBPF 探针、发现 Agent 进程、把采集到的事件写入 SQLite。

```
FLAGS:
        --daemon              Run as daemon in background (Linux only)
        --enable-filewatch    Enable file watch probe (monitors .jsonl file opens from traced processes)
        --no-ebpf             Skip eBPF probes and collect trajectories only (Linux only)
    -v, --verbose             Enable verbose/debug output

OPTIONS:
    -c, --config <config>        Path to JSON configuration file (Linux only) [default: /etc/agentsight/config.json]
        --pid-file <pid-file>    PID file path for daemon mode (Linux only) [default: /tmp/agentsight.pid]
```

```bash
# 前台运行（先停掉服务，同时只应有一个 tracer）
sudo systemctl stop agentsight.service
sudo agentsight trace

# 后台运行并指定规则文件
sudo agentsight trace --daemon -c /etc/agentsight/config.json

# 无特权环境：仅采集轨迹，无需 root 与 CAP_BPF
agentsight trace --no-ebpf
```

> 同时跑两个 tracer 会争抢同一批 uprobe，数据也会变得难以解释。启动前台 tracer 前请先停掉
> `agentsight.service`。

### 无特权运行（`--no-ebpf`）

加载探针需要 root 或 `CAP_BPF`/`CAP_PERFMON`。没有这些权限时，`trace` 会在探针初始化阶段停下并
报出 `Failed to create probes`。`--no-ebpf` 跳过探针，仅运行纯用户态的轨迹采集器：扫描本地 Agent
会话文件（Claude Code、Qoder、QoderWork、Codex），转换为 ATIF v1.7 并存入 `trajectories.db`，供
`serve` 展示。

此模式**不提供**的能力：Token 计量、审计事件、中断检测以及进程与 TLS 流量监控均来自探针，因此
`agentsight token`、`audit`、`metrics`、`interruption` 查不到新数据。

两个需要注意的行为：

- 即使 `features.trajectory_collection.enabled` 为 `false`，该参数也会强制启用轨迹采集，因为它是此
  模式下唯一的数据来源。`scan_interval_secs` 和 `scan_dirs` 仍从配置文件读取。
- `trajectories.db` 在共享数据目录可写时写入该目录，否则写入 `$HOME/.local/share/agentsight/`。
  启动时会打印实际路径和对应的 `serve --db` 命令；发生回退时需把该路径传给 `serve`。回退目录与
  数据库按仅属主权限（`0700`/`0600`）创建，因为轨迹内含完整对话内容。

默认扫描根目录是 `/root` 和 `/home/*`。若 home 目录不在这两处（例如容器使用 `/app`），需显式配置
`features.trajectory_collection.scan_dirs` 指向会话目录。

## agentsight serve

基于 tracer 写入的同一批数据库，提供 HTTP API 和内嵌的 Dashboard。

```
OPTIONS:
        --config <config>    Path to JSON configuration file (Linux only) [default: /etc/agentsight/config.json]
        --db <db>            Custom database path (Linux only)
        --host <host>        Host to bind to [default: 127.0.0.1]
        --port <port>        Port to bind to [default: 7396]
```

```bash
# 仅本机访问
sudo agentsight serve

# 允许其他主机访问（请先在防火墙限制端口）
sudo agentsight serve --host 0.0.0.0 --port 7396

# 不采集，只浏览一份归档数据
agentsight serve --db /backup/genai_events.db
```

`serve` 要用和 `trace` 相同的用户运行，否则两者解析到的数据目录不一致。

## agentsight dashboard

打印 Dashboard 地址与访问令牌，并尝试打开浏览器。在 ECS 实例上还会打印安全组配置链接。

```
FLAGS:
        --no-open          Do not attempt to open a browser
        --skip-sg-guide    Skip ECS security group guide output

OPTIONS:
        --config <config>    Path to JSON configuration file [default: /etc/agentsight/config.json]
        --db <db>            Custom database path (used to locate the token file)
        --host <host>        Host the server is bound to (use a specific IP/hostname to override the Network URL) [default: 0.0.0.0]
        --port <port>        Port the server is listening on [default: 7396]
```

```bash
$ sudo agentsight dashboard --no-open

AgentSight 仪表盘状态
=====================

  认证:    已启用
  本机:    http://127.0.0.1:7396 (无需认证)
  局域网:   http://192.168.1.10:7396/?token=<TOKEN>
  公网:    http://203.0.113.10:7396/?token=<TOKEN>
```

在服务器上建议加 `--no-open`——以 root 身份打开浏览器通常不是你想要的行为。

## agentsight summary

一条命令看清最近一段时间的整体情况。

```
FLAGS:
        --json           Output as JSON
OPTIONS:
        --last <last>    Query the last N hours (default: 24) [default: 24]
```

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

## agentsight token

按周期查询 Token 消耗，可与上一周期对比。

```
FLAGS:
        --compare    Compare with previous period
        --json       Output as JSON

OPTIONS:
        --data-file <data-file>    Custom data file path
        --hours <hours>            Query last N hours
        --period <period>          Query by fixed time period
                                   [possible values: today, yesterday, week, last_week, month, last_month]
```

```bash
$ sudo agentsight token --json
{
  "period": "今天",
  "input_tokens": 100000,
  "output_tokens": 10000,
  "total_tokens": 110000,
  "request_count": 20,
  "comparison": null,
  "breakdown": []
}

# 本周与上周对比
sudo agentsight token --period week --compare
```

## agentsight audit

查询审计流水：LLM 调用与进程动作。

```
FLAGS:
        --json       Output as JSON
        --summary    Show summary statistics

OPTIONS:
        --type <event-type>       Filter by event type: "llm" or "process"
        --exclude <exclude>...    Hide process_action events whose command/args contain any of these
                                  substrings. Repeatable. The hidden count is reported
        --last <last>             Query last N hours (e.g. 24)
        --pid <pid>               Filter by PID
```

```bash
$ sudo agentsight audit --summary
=== Audit Summary (last 24 hours) ===

LLM calls:        20
Process actions:  100

Providers:
  openai: 20 calls

Top commands:
  agent-sec-cli scan-pii --stdin --format json --redact-output --source observability ...: 40 times
  sh -c python3 /usr/share/anolisa/extensions/agent-sec-core/hooks/observability_hook.py ...: 30 times
  ...
```

单条事件是 JSON，可以交给 `jq`。请使用 `--json`（它输出的是一个数组）并遍历它——不加 `--json` 时会先
打印一行人类可读的表头，`jq` 无法解析：

```bash
sudo agentsight audit --last 24 --type llm --json \
  | jq -r '.[] | [.extra.model, .extra.input_tokens, .extra.output_tokens] | @tsv'
```

在装了 agent-sec-core 或 Tokenless 的机器上，`process_action` 会被大量 hook 和包装进程占满，可以用
多个 `--exclude` 过掉：

```bash
sudo agentsight audit --last 1 --exclude agent-sec-cli --exclude observability_hook.py
```

## agentsight discover

查看哪些 Agent 进程在运行、有哪些规则生效。

```
FLAGS:
        --list-known    List all known agents and show currently matched PIDs
    -v, --verbose       Show detailed output including executable path

OPTIONS:
    -c, --config <config>    Path to JSON configuration file [default: /etc/agentsight/config.json]
```

```bash
$ sudo agentsight discover
已发现 AI Agent（共 1 个）:
============================================================

  CoshNG [PID: 10000]
    类别: custom
    命令:  /usr/libexec/anolisa/cosh-ng/cosh-shell ...

总计: 1 个 Agent

$ sudo agentsight discover --list-known | head -12
已知 AI Agent（共 31 条规则）:
============================================================

  Hermes (custom)
    命令行规则: hermes*
    运行中 PID: 无
    Config-driven agent
```

`discover` 与 `discover --list-known` 读取的是 `trace` 所用的同一份配置（`--config`，默认
`/etc/agentsight/config.json`），因此 `--list-known` 反映的是实际生效的规则——包括你新增的那些。
文件缺失或无法解析时，会打印提示并回退到内置规则。详见
[Agent 发现规则](configuration.md#agent-发现规则)。

## agentsight metrics

以 Prometheus 文本格式输出按 Agent 分组的 Token 计数器（累计值）。

```bash
$ sudo agentsight metrics | head -8
# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)
# TYPE agentsight_token_input_total counter
agentsight_token_input_total{agent="CoshNG"} 100000
agentsight_token_input_total{agent="Cosh"} 50000

# HELP agentsight_token_output_total Total output tokens consumed by agent (all-time)
# TYPE agentsight_token_output_total counter
agentsight_token_output_total{agent="CoshNG"} 10000
```

运行中的服务在 `GET /metrics`（仅本机）暴露同样的内容，抓取时通常用它更方便，见
[数据与存储](data-and-storage.md#prometheus-指标)。

## agentsight interruption

查询与关闭中断事件。数据库：`/var/log/sysak/.agentsight/interruption_events.db`。

```
SUBCOMMANDS:
    list            List interruption events with optional filters
    get             Get a single interruption event by its ID
    stats           Show per-type count statistics within a time range
    count           Count unresolved interruptions grouped by severity
    session         List all interruption events for a specific session
    conversation    List all interruption events for a specific conversation
    resolve         Mark an interruption event as resolved
```

`list` 的参数：

```
FLAGS:
        --json          Output as JSON (one JSON array)
        --resolved      Show only resolved events
        --unresolved    Show only unresolved events

OPTIONS:
        --agent <agent>          Filter by agent name (exact match)
        --type <itype>           [possible values: agent_crash, rate_limit, auth_error,
                                                   network_timeout, service_unavailable,
                                                   safety_filter, sse_truncated, context_overflow,
                                                   token_limit, llm_error, retry_storm, dead_loop,
                                                   tool_failure, empty_response, resource_exhaustion,
                                                   slow_response, state_machine_error,
                                                   unauthorized_action]
        --last <last>            Query last N hours (default: 24) [default: 24]
        --limit <limit>          Maximum number of results (default: 100) [default: 100]
        --severity <severity>    [possible values: critical, high, medium, low]
```

```bash
$ sudo agentsight interruption list --last 24
INTERRUPTION_ID                    TYPE          SEVERITY  OCCURRED_AT              RESOLVED  AGENT    SESSION_ID
------------------------------------------------------------------------------------------------------------------
11111111222222223333333344444444   token_limit   medium    2026-01-01 12:00:00.000  no        CoshNG   00000000-11...

Total: 1 event(s)

$ sudo agentsight interruption get 11111111222222223333333344444444
Interruption Event Detail
============================================================
  ID:           11111111222222223333333344444444
  Type:         token_limit
  Severity:     medium
  Occurred At:  2026-01-01 12:00:00.000 (1767268800000000000ns)
  Resolved:     no
  Session ID:   00000000-1111-2222-3333-444444444444
  Conversation: aaaaaaaabbbbbbbbccccccccdddddddd
  Trace ID:     chatcmpl-00000000-1111-2222-3333-444444444444
  PID:          10000
  Agent:        CoshNG
  Detail:
{
  "model": "qwen-plus",
  "output_tokens": 4096,
  "max_tokens": 4096,
  "ratio": 1.0
}
```

```bash
# 最近一周按类型统计
sudo agentsight interruption stats --last 168

# 未解决事件按严重级别计数
sudo agentsight interruption count --last 24

# 查某个会话命中的全部中断，然后关闭其中一条
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444
sudo agentsight interruption resolve 11111111222222223333333344444444
```

> `--type` 接受检测器能产出的所有中断类型（取值由 `InterruptionType::ALL` 派生）。
> `agentsight interruption list --help` 会列出当前取值；带触发条件的完整清单见
> [中断检测](interruption-detection.md#中断类型)。

## agentsight skill-metrics

按需扫描 GenAI 事件，计算 Skill 使用指标。

```
SUBCOMMANDS:
    all             Compute all skill metrics
    downloads       Show skill download tracking (first appearance in available_skills)
    loads           Show skill load counts (SKILL.md reads via tool_calls)
    usage-ratio     Show skill usage ratio (tasks with/without skills)
    distribution    Show per-task skill count distribution
    hotness         Show skill hotness ranking by week

OPTIONS（所有子命令通用）:
        --agent <agent>    Filter by agent name
        --db <db>          Override database path
        --last <last>      Query last N hours (default: 168 = 7 days) [default: 168]
        --json             Output as JSON
```

```bash
sudo agentsight skill-metrics all --last 168
sudo agentsight skill-metrics hotness --agent CoshNG --json
```

Dashboard 的 Skill 指标页展示的是同一批数字。

## 相关页面

- [配置](configuration.md)——配置文件控制哪些行为
- [Dashboard 指南](dashboard.md)——这些查询在界面上的等价操作
- [数据与存储](data-and-storage.md)——HTTP API 与数据库结构
