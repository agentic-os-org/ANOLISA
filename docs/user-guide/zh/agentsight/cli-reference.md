# AgentSight CLI 参考

[English](../../en/agentsight/cli-reference.md)

本页所有参数均取自 Linux 上的 `agentsight 0.13.0 --help`。示例输出保留真实排版，但其中的 ID 全是占位
值、数字全是整数，均非真实采集结果。

## 通用约定

| 事项 | 说明 |
|---|---|
| 权限 | `trace` 需要 root（或 `CAP_BPF` + `CAP_PERFMON`）。查询类命令需要读取 `/var/log/sysak/.agentsight`，该目录归服务所有，请加 `sudo`。 |
| 配置文件 | 需要读取规则的命令支持 `--config`，默认 `/etc/agentsight/config.json`。`discover` 读取与 `trace` 相同的文件，因此会反映你的自定义规则。 |
| 数据位置 | 固定为 `/var/log/sysak/.agentsight`。`serve`、`dashboard`、`skill-metrics`、`audit`、`metrics`、`summary`、`interruption`、`token` 支持用 `--db` 指定其他数据库文件。 |
| 机器可读输出 | `discover`、`token`、`metrics`、`audit`、`summary`、`interruption *`、`skill-metrics *` 都支持 `--json`。 |
| 输出语言 | `summary`、`metrics`、`interruption` 输出英文；`discover`、`token` 无论 locale 都输出中文。需要稳定文本时请用 `--json`。 |
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

## 资源 benchmark 辅助脚本（Linux）

仓库中的 `resource_run.sh --ffi` 通过最小 C FFI 宿主测量 AgentSight，复用现有负载和 CPU/RSS 报告。
`--ffi-api typed|v2` 选择回调接口，`--candidate-lib` 和 `--baseline-lib` 支持已有动态库。
FFI 就绪和采集校验直接读取回调凭证，不要求 SQLite 存储。
采集仍需要 root 或 eBPF capabilities。构建条件、A/B 参数及测量范围见
[benchmark 使用指南](../../../../src/agentsight/scripts/docs/BENCHMARK_USAGE_zh.md#71-使用最小-c-宿主测量-ffi-动态库)。

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
报出 `Failed to create probes`。`--no-ebpf` 跳过探针，仅运行纯用户态的轨迹采集器：扫描本地 JSONL
会话（Claude Code、Qoder、QoderWork、Codex）和 Hermes `state.db`，转换为 ATIF v1.7 并存入
`trajectories.db`，供 `serve` 展示。

此模式**不提供**的能力：Token 计量、审计事件、中断检测以及进程与 TLS 流量监控均来自探针，因此
`agentsight token`、`audit`、`metrics`、`interruption` 查不到新数据。

两个需要注意的行为：

- 即使 `features.trajectory_collection.enabled` 为 `false`，该参数也会强制启用轨迹采集，因为它是此
  模式下唯一的数据来源。`scan_interval_secs` 和 `scan_dirs` 仍从配置文件读取。
- `trajectories.db` 在共享数据目录可写时写入该目录，否则写入 `$HOME/.local/share/agentsight/`。
  启动时会打印实际路径和对应的 `serve --db` 命令；发生回退时需把该路径传给 `serve`。回退目录与
  数据库按仅属主权限（`0700`/`0600`）创建，因为轨迹内含完整对话内容。

JSONL 默认扫描 `/root` 和 `/home/*`，并识别 AgentSight 进程继承的 `CLAUDE_CONFIG_DIR`；Hermes
检查 `$HERMES_HOME/state.db` 和 `~/.hermes/state.db`。设置 `scan_dirs` 会覆盖这些默认位置；配置项
可指向会话根目录、Hermes home/数据库或包含多个会话工作区的上层目录。嵌套 Claude 发现最多向下四级、
每个根目录最多检查 4096 个目录，且不跟随目录符号链接。若 SDK options 只给 CLI 子进程设置了非标准
配置目录，需向 AgentSight 进程设置相同的 `CLAUDE_CONFIG_DIR`，并留空 `scan_dirs`。

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
        --json                          Output as JSON
OPTIONS:
        --agent <agent>                 Scope the session and interruption totals to one agent
        --db <db>                       Override the sessions database path
        --last <last>                   Query the last N hours (default: 24) [default: 24]
        --tokenless-db <tokenless-db>   Override the tokenless stats database path
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

`--db` 改为读取数据库副本（例如从别的机器拷来的），不再读默认目录，因此无需 `sudo`，也不会混入在跑
主机的数据。中断库从同一目录解析（`interruption_events.db`），与默认布局一致。tokenless 统计库不在
这些数据库里（位于调用者的主目录），`--db` 不会移动它：用 `--tokenless-db` 指向 `stats.db` 的副本，
整份报告才完全不混入读取主机的数据。

```bash
agentsight summary --last 24 --db /backup/agentsight/genai_events.db
```

`--agent` 把 Sessions 与 Interruptions 两节收窄到单个 Agent，两节分别沿用各自存储的匹配口径：
Sessions 比较会话列表本就解析出的标签——优先记录到的 Agent 名，缺省时回退到进程名；Interruptions
只比较 `agent_name` 列，也就是 `interruption list --agent` 过滤的那一列。两者都大小写不敏感。
带上它时标题与 JSON 会多出一个 `agent` 键标明范围；不带时两节覆盖所有 Agent，报告形状与之前
完全一致。

```bash
agentsight summary --last 24 --agent CoshNG
```

Tokenless 节省量是按会话记录的、不带 Agent 归属，因此按 Agent 的报告不再输出该节，并明确说明，
而不是把全主机的数字摆在按 Agent 的汇总旁边。

## agentsight token

按周期查询 Token 消耗，可与上一周期对比。

```
FLAGS:
        --compare    Compare with previous period
        --json       Output as JSON

OPTIONS:
        --data-file <data-file>    Custom data file path
        --db <db>                  Override the database path (same as --data-file)
        --hours <hours>            Query last N hours
        --period <period>          Query by fixed time period
                                   [possible values: today, yesterday, week, last_week, month, last_month]
```

`--db` 与其余查询命令的库路径覆盖同名同义；它在 `--data-file` 之后加入，后者继续支持。两者只传
一个，同时给出会报用法错误。

```bash
# 读取一份数据库副本，而不是默认位置
agentsight token --db /backup/agentsight/agentsight.db --period week
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
        --db <db>                 Override the database path
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

`--db` 改为读取 `agentsight.db` 的副本（例如从别的机器拷来的），不再读默认目录：

```bash
agentsight audit --last 24 --db /backup/agentsight/agentsight.db
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

`--json` 把同样的按 Agent 汇总的行输出为对象（`agent_name`、`input_tokens`、`output_tokens`、
`total_tokens`、`request_count`），而不是 exposition 文本，供本来要解析文本的脚本直接使用：

```bash
$ sudo agentsight metrics --json
[
  {
    "agent_name": "CoshNG",
    "input_tokens": 100000,
    "output_tokens": 10000,
    "total_tokens": 110000,
    "request_count": 42
  }
]
```

运行中的服务在 `GET /metrics`（仅本机）暴露同样的内容，抓取时通常用它更方便，见
[数据与存储](data-and-storage.md#prometheus-指标)。

`--db` 改为读取 `genai_events.db` 的副本，不再读默认目录：

```bash
agentsight metrics --db /backup/agentsight/genai_events.db
```

## agentsight interruption

查询与关闭中断事件。数据库：`/var/log/sysak/.agentsight/interruption_events.db`。`--db`（放在子命令
之前）改为读取该库的副本（例如从别的机器拷来的）：

```bash
agentsight interruption --db /backup/agentsight/interruption_events.db stats --last 168
```

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

`stats` 与 `count` 接受和 `list` 相同的 `--agent` 过滤，不用再自己去处理 `list --json` 的输出来
得到单个 Agent 的分布：

```
FLAGS:
        --json          Output as JSON

OPTIONS:
        --agent <agent>          Filter by agent name (exact match)
        --last <last>            Query last N hours (default: 24) [default: 24]
```

`stats` 另外接受 `--unresolved` / `--resolved` 这对参数，于是按类型的表可以只回答「这个窗口里
还有多少未解决」，而不必把别人已经关掉的事件也算进去：

```
FLAGS:
        --json          Output as JSON
        --resolved      Count only resolved events
        --unresolved    Count only unresolved events
```

```
$ sudo agentsight interruption stats --last 168 --unresolved
TYPE                 SEVERITY    COUNT
----------------------------------------
token_limit          medium          3
llm_error            high            1
```

两个 flag 都不给时 `stats` 保持原有口径，统计整个窗口（含已解决与未解决）；两个 flag 互斥。
`count` 不变，仍只统计未解决事件。不带 `--agent` 时两者都覆盖主机上的所有 Agent。

`count` 同样接受 `list` 的 `--type`，把按严重级别的合计收窄到某一种中断类型，取值集合与 `list`
一致，于是「还有多少 `token_limit` 没解决」一次调用就能问出来，不必先拉 `list --type` 的数组再自己
累加：

```
$ sudo agentsight interruption count --last 24 --type token_limit --json
{
  "total": 2,
  "by_severity": {
    "critical": 0,
    "high": 0,
    "medium": 2,
    "low": 0
  }
}
```

不带 `--type` 时合计仍覆盖所有类型，人类可读形式的排版也不变；某个类型一条都没命中时返回全零，
而不是退回未过滤的合计。

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

# 只看某个 Agent 的同一份分布
sudo agentsight interruption stats --last 168 --agent CoshNG

# 未解决事件按严重级别计数
sudo agentsight interruption count --last 24

# 只看某个 Agent 的未解决计数
sudo agentsight interruption count --last 24 --agent CoshNG

# 查某个会话命中的全部中断，然后关闭其中一条
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444
sudo agentsight interruption resolve 11111111222222223333333344444444

# 同样关闭一条，但要机器可读的输出
sudo agentsight interruption resolve 11111111222222223333333344444444 --json

# 只看该会话里仍未解决的中断
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444 --unresolved

# 只看该会话里命中 token_limit 的中断
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444 --type token_limit
```

`resolve` 同样接受 `--json`，于是这个唯一会写库的 `interruption` 子命令与只读命令使用同一套机器可读
口径。不带该参数时，它保持原来那一行输出不变：

```
$ sudo agentsight interruption resolve 11111111222222223333333344444444 --json
{
  "status": "resolved",
  "interruption_id": "11111111222222223333333344444444"
}
```

`status` 与 HTTP 路由 `POST /api/interruptions/{id}/resolve` 的返回字段一致；CLI 额外带上
`interruption_id`，而该路由只在路径里带这个 id。
id 不存在时两种形式仍然以非零码退出并在 stderr 给出提示：`--json` 只改变成功时的返回体，不改变失败路径。

`session` 与 `conversation` 接受和 `list`、`stats` 相同的 `--unresolved` / `--resolved` 这对参数，
于是按 id 的视图可以只看仍未解决（或已关闭）的事件，不必用眼睛去读 `RESOLVED` 列。它们同样接受
`list` 的 `--type` 与 `--severity` 过滤，取值集合一致，因此单个会话可以直接读到 critical 的
`agent_crash` 那几行，不用再处理 `--json` 数组：

```
FLAGS:
        --json          Output as JSON (one JSON array)
        --resolved      Show only resolved events
        --unresolved    Show only unresolved events

OPTIONS:
        --severity <severity>    [possible values: critical, high, medium, low]
        --type <itype>           [possible values: agent_crash, rate_limit, auth_error,
                                                   network_timeout, service_unavailable,
                                                   safety_filter, sse_truncated, context_overflow,
                                                   token_limit, llm_error, retry_storm, dead_loop,
                                                   tool_failure, empty_response, resource_exhaustion,
                                                   slow_response, state_machine_error,
                                                   unauthorized_action]
```

解决状态那两个 flag 仍然互斥，所有过滤默认关闭：不带任何 flag 时照旧打印该 id 的全部历史。
这两个视图有意不提供 `--agent`——解析器按 `(agent, pid, prompt)` 推导 session，一个 session 本身
就只属于一个 Agent；按 Agent 过滤请用 `list`、`stats` 或 `count`。

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

OPTIONS（all、hotness）:
        --granularity <granularity>    按天或按周切分热度趋势
                                       [possible values: day, week] [default: week]
```

```bash
sudo agentsight skill-metrics all --last 168
sudo agentsight skill-metrics hotness --agent CoshNG --json

# 按天分桶，而不是默认的按周
sudo agentsight skill-metrics hotness --last 168 --granularity day
```

Dashboard 的 Skill 指标页展示的是同一批数字，其热度图也支持同样的按天/按周切换。
`--granularity` 只切分热度趋势，因此只有 `all` 和 `hotness` 接受它，四个单项指标子命令不接受——
在那里传该参数是用法错误，与 `GET /api/skill-metrics/downloads` 等接口的拒绝行为一致。不给该参数时
趋势仍按周分桶，文本与 JSON 输出保持不变。

## 在本地批量导出 ATIF

从源码仓库的 `src/agentsight` 目录运行：

```bash
python3 scripts/batch-analyze.py --file session.jsonl --export-atif ./atif-export --min-steps 1
```

`--export-atif DIRECTORY` 保存转换后的文档，不启动分析器，也不发起模型请求。
它与 `--dim` 互斥，并拒绝 `--binary`。文件选择沿用分析模式：`--file`、可重复的
`--dir`、`--limit`（默认 10）和 `--min-steps`（默认 3）。目录扫描仍要求文件至少
1 KiB，并排除 subagent 文件；小文件可通过 `--file` 显式导出。

文件名为源文件规范化绝对路径的完整 SHA-256 加 `.atif.json`。不同目录中的同名源文件
不会混淆；再次导出同一路径会原子替换其旧产物。移动源文件后产物名会变化。
源文件和目标目录中的其他文件保持不变，目标目录按需创建。标准输出按文件生成一行
JSON 记录，包含 `_atif_file`、`_source_file`、`_session_id` 和 `_steps`。

产物使用 UTF-8，沿用转换器已有的过滤和截断规则，不是原始日志的无损备份。
产物包含会话内容，应放在适合保存这些数据的本地目录。写入失败会记入标准错误的
汇总，清理临时文件后继续后续文件。与原有批量分析一致，批次处理结束时返回 0，
即使汇总中有单个文件失败。

## 相关页面

- [配置](configuration.md)——配置文件控制哪些行为
- [Dashboard 指南](dashboard.md)——这些查询在界面上的等价操作
- [数据与存储](data-and-storage.md)——HTTP API 与数据库结构

## 并行批量分析

源码仓库的批处理工具可以并行运行相互独立的分析器进程。
从 `src/agentsight` 先构建分析器，再显式启用最多四个 worker：

```bash
cargo build -p agentsight-opt --bin analyze
python3 scripts/batch-analyze.py --dir /path/to/sessions --dim perf --binary target/debug/analyze --jobs 4
```

`--jobs N` 接受 1 到 8，默认 1；运行中的调用和已提交但未输出的任务均不超过 N。
即使后面的调用先完成，JSON 文档和逐文件诊断仍按原始发现顺序输出，因此较早的慢文件
可能延后可见输出。每份文档仍保留源文件和会话元数据。

文件选择、最少步骤过滤、超时、失败计数、退出行为和临时文件清理沿用原有规则。
预先构建可避免多个 Cargo 调用争用构建锁。完整分析仍需要模型凭据；N 大于 1 时
可能同时产生付费请求，并触发端点限流。该能力在允许并发的情况下缩短独立会话的
批次耗时，不会降低单次模型延迟、Token 用量或单价。

## 基准测试产物

从仓库根目录运行 `bash src/agentsight/scripts/benchmark/single_run/resource_run.sh --help`，
可查看 Linux standalone 资源 benchmark（原 `memory_run.sh`）的参数。
默认 32 个 Agent、每路 100 输出 tok/s、60 秒，需要 root 或 eBPF capabilities。
`resource-report.txt` 和 `resource-summary.json` 汇总正式负载窗口的平均/峰值 RSS、
平均/P95/峰值 CPU，尾部收流单独统计；CPU 100% 表示一个逻辑核。
协作式 CPU 调度默认开启，目标为 25%；`--cpu-budget-percent N` 可覆盖目标，`0` 关闭，
省略时沿用配置，配置未设置时使用 25%。
评估时需同时检查 CPU、RSS、队列增长与尾部排空时间。配置、A/B 对比和有效性判断见
[benchmark 使用指南](../../../../src/agentsight/scripts/docs/BENCHMARK_USAGE_zh.md)。

运行 `scripts/benchmark/campaign/aggregate_report.py --campaign <campaign.json> --results <results>` 时还会导出 UTF-8 `run-inventory.csv`。清单包含 `runs/**/run-result.json` 下发现的 smoke、capacity、matrix、soak、recovery 和 fault 正式运行，按相对证据路径排序。列包含运行参数、harness 退出码、结论、缺失及失败门槛的 JSON 数组和 `result_path`；未提供的可选值留空。归档的不完整运行不在此清单内。既有汇总报告和原始证据保持不变。

资源报告可通过 `render_report.py --metrics <file.csv.gz>` 读取普通 CSV 或 gzip CSV。Campaign 恢复证据优先使用 `measurement/metrics.csv`，仅在普通文件缺失时使用 `measurement/metrics.csv.gz`。压缩文件沿用相同列和计算方式；损坏的压缩输入仍会报错。
