# AgentSight 配置

[English](../../en/agentsight/configuration.md)

AgentSight 只读一个 JSON 文件：`/etc/agentsight/config.json`（可用 `--config` 覆盖）。它决定哪些进程
被追踪、哪些功能开启、流水线最多占用多少内存。源码里的参考副本是
`src/agentsight/agentsight.json`。

## 修改前必须知道的两件事

1. **你的文件是「替换」内置默认规则，而不是「追加」。** 如果 `cmdline.allow` 里漏了某条规则，对应
   Agent 就不再被发现。请始终从随包发布的文件出发，在它基础上增加。
2. **改完用 reload，不用 restart。** 执行 `sudo systemctl reload agentsight.service`，守护脚本会重启
   两个工作进程以重新读取配置，几秒内恢复采集。

## 文件结构

```json
{
  "schema_version": 5,
  "storage": {
    "base_path": "/var/log/sysak/.agentsight",
    "max_total_size_mb": 2200,
    "primary": { "retention_days": 30, "max_db_size_mb": 500, "check_interval_secs": 60 },
    "genai": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 60 },
    "interruptions": { "retention_days": 30, "max_db_size_mb": 100, "check_interval_secs": 60 },
    "trajectories": { "retention_days": 30, "max_db_size_mb": 500, "check_interval_secs": 300 },
    "optimization": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "security_audit": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 3600 },
    "reuse": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "causal": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "enforcement": { "retention_days": 30, "max_db_size_mb": 100, "check_interval_secs": 60 }
  },
  "runtime": {
    "sls_logtail_path": ""
  },
  "server": {
    "auth": { "enabled": true }
  },
  "deadloop": {
    "enabled": false,
    "kill_after_count": 3
  },
  "features": {
    "token_stats": true,
    "tokenizer": { "enabled": false, "cache_size": 4 },
    "session_mapping": { "enabled": true, "max_entries": 10000 },
    "sqlite_storage": { "enabled": true, "batch": { "max_size": 100, "flush_ms": 100 } },
    "resource_sampling": false,
    "interruption_detection": { "enabled": true },
    "audit": true,
    "token_consumption": false,
    "sls_logtail": false,
    "trajectory_collection": { "enabled": false, "scan_interval_secs": 30 },
    "reuse_llm_judge": false
  },
  "runtime_limits": {
    "event_channel_capacity": 10000,
    "event_channel_policy": "backpressure",
    "event_channel_max_bytes_mb": 64,
    "pending_genai_max_count": 1000,
    "pending_genai_max_bytes_mb": 16,
    "pid_cache_size": 1024,
    "connection_capacity": 128,
    "connection_max_bytes_mb": 64,
    "ffi_channel_max_bytes_mb": 64,
    "max_connection_body_mb": 8,
    "max_sse_retained_mb": 2,
    "retain_raw_sse": false,
    "connection_idle_timeout_secs": 60,
    "connection_hard_timeout_secs": 300,
    "ring_buffer_mb": 64
  },
  "https": [
    { "rule": ["dashscope.aliyuncs.com"] },
    { "rule": ["api.openai.com"] }
  ],
  "http": [],
  "cmdline": {
    "allow": [
      { "rule": ["*cosh-core*"], "agent_name": "CoshNG" },
      { "rule": ["*node*", "*claude*"], "agent_name": "Claude" }
    ],
    "deny": [
      { "rule": ["*", "*", "-c", "*sftp-server*"] }
    ]
  },
  "codex_offsets": { "schema_version": 1, "entries": [] }
}
```

## SQLite 存储策略

`storage.base_path` 是 AgentSight 自有数据库共用的目录。`storage.max_total_size_mb` 限制这些数据库的
总物理占用，默认值为 `2200` MiB。设为 `0` 可关闭总量限制；启用时必须至少为 `9` MiB。每轮维护前都会
重读有效配置；如果配置文件缺失、未写完或无效，则继续使用最近一次有效值。

在 schema v5 中，每项存储策略统一使用 `retention_days`、`max_db_size_mb` 和 `check_interval_secs`。值为
`0` 时，分别关闭按时间保留、按容量清理或定时维护；如果 `check_interval_secs` 为 `0`，即使另外两项
非零也不会自动执行维护。

| 存储 | 保留时间 | 容量上限 | 检查间隔 |
|---|---:|---:|---:|
| `storage.primary`（`agentsight.db`） | 30 天 | 500 MiB | 60 秒 |
| `storage.genai`（`genai_events.db`，含评估结果） | 30 天 | 200 MiB | 60 秒 |
| `storage.interruptions` | 30 天 | 100 MiB | 60 秒 |
| `storage.trajectories` | 30 天 | 500 MiB | 300 秒 |
| `storage.optimization` | 30 天 | 200 MiB | 300 秒 |
| `storage.security_audit` | 30 天 | 200 MiB | 3,600 秒 |
| `storage.reuse` | 30 天 | 200 MiB | 300 秒 |
| `storage.causal` | 30 天 | 200 MiB | 300 秒 |
| `storage.enforcement` | 30 天 | 100 MiB | 60 秒 |

维护先删除允许淘汰且已经过期的行。如果数据库因此发生变化，必须成功完成 WAL checkpoint 后才进入容量
阶段。物理占用超过 `max_db_size_mb` 时触发淘汰，并以逻辑占用降到上限的 90% 为目标。维护不会执行
`VACUUM`，释放页会留在 SQLite freelist 中供后续写入复用。

`check_interval_inserts` 已不再支持。`schema_version` 变化时配置格式整体替换：版本缺失或低于 v5 的文件
会先备份，再替换为 schema v5，不会合并旧设置。自定义配置请从随包发布的 `agentsight.json` 开始修改，
不要把旧字段复制到新文件。

## 功能开关

`features` 下的每一项都可以独立关闭。关闭后对应模块根本不会被实例化，因此既不占内存也不产生 I/O。

| 功能 | JSON 路径 | 默认值 | 作用 |
|---|---|---|---|
| Token 计账 | `features.token_stats` | `true` | 核心能力：按 Agent、模型统计 Token |
| 本地 tokenizer | `features.tokenizer.enabled` | `false` | 供应商未返回 usage 时用 Hugging Face 分词器兜底 |
| Session 映射 | `features.session_mapping.enabled` | `true` | 把供应商返回的 response ID 映射到 Agent 的 session ID |
| SQLite 存储 | `features.sqlite_storage.enabled` | `true` | 本地持久化；关闭后使用空实现，Dashboard 将没有数据 |
| 资源采样 | `features.resource_sampling` | `false` | 每秒采集 Agent CPU/RSS；依赖 SQLite 存储 |
| 中断检测 | `features.interruption_detection.enabled` | `true` | 检测失败、停滞与死循环 |
| 审计 | `features.audit` | `true` | 持久化 LLM 调用与进程动作 |
| Token 消费记录 | `features.token_consumption` | `false` | 额外的聚合消费记录 |
| 外部日志导出 | `features.sls_logtail` | `false` | 把结构化事件写入文件，供外部采集器读取 |
| 轨迹采集 | `features.trajectory_collection.enabled` | `false` | 周期扫描本地 Agent JSONL 会话和 Hermes `state.db` 写入 `trajectories.db`（仅 trace 模式） |
| 轨迹 LLM 判定 | `features.reuse_llm_judge` | `false` | 允许 `POST /api/reuse/judge` 调用已配置的 LLM 判定规则无法归类的轨迹；每次调用都会产生费用 |

`reuse_llm_judge` 只影响 `POST /api/reuse/judge`。除非已经配置优化 LLM 且明确需要付费的二级判定，否则请保持关闭。修改后 reload 服务，让 server 读取新配置。

随功能附带的调节项：

| 配置项 | 默认值 | 含义 |
|---|---|---|
| `features.tokenizer.cache_size` | `4` | 常驻内存的分词器模型个数 |
| `features.session_mapping.max_entries` | `10000` | response ID → session ID 映射上限 |
| `features.sqlite_storage.batch.max_size` | `100` | 每批写入行数 |
| `features.sqlite_storage.batch.flush_ms` | `100` | 批量写入的最大延迟（毫秒） |
| `features.trajectory_collection.scan_interval_secs` | `30` | JSONL 与 Hermes SQLite 轨迹发现间隔；存储清理由 `storage.trajectories.check_interval_secs` 控制 |
| `features.trajectory_collection.scan_dirs` | 未设置 | 覆盖默认位置及环境变量路径；每项可指向会话根目录、Hermes home/数据库或四级内含 `.claude/projects` 的工作区上层目录 |

## 运行时上限

这些限额约束流水线中保留的 payload。设置进程内存上限时，还应计入进程基线、eBPF ring buffer、
解码器工作内存和 allocator 开销。安装包 systemd unit 仍使用 `MemoryMax=350M`。

| 配置项 | 默认值 | 含义 |
|---|---|---|
| `cpu_budget_percent` | `25` | trace 进程的协作式 CPU 目标；`0` 关闭调度，`100` 表示一个逻辑核 |
| `event_channel_capacity` | `10000` | 探针到流水线的有界通道容量 |
| `event_channel_policy` | `backpressure` | 通过字节限额后等待队列槽位；`drop_newest` 拒绝已满队列。`sample`、`sample:N`、`sample=N` 每 N 条保留一条（默认 N=10） |
| `event_channel_max_bytes_mb` | `64` | 排队 probe payload 的字节上限；`0` 关闭限额。超过字节限额时丢弃事件并增加通道丢弃计数，与槽位策略分别执行 |
| `pending_genai_max_count` | `1000` | 原始完成记录、等待 session ID 的事件与本地 SQLite 重试共享条数上限 |
| `pending_genai_max_bytes_mb` | `16` | 同一队列的字节上限 |
| `pid_cache_size` | `1024` | PID → Agent 名称的 LRU 条目数 |
| `connection_capacity` | `128` | HTTP 连接缓存条目数，必须大于零；至少应达到预计同时存在的连接数 |
| `connection_max_bytes_mb` | `64` | HTTP/1 与 HTTP/2 共用的保留字节预算，包含 buffer capacity 和解码器状态；`0` 关闭预算 |
| `ffi_channel_max_bytes_mb` | `64` | FFI 事件 payload 的字节预算，计入执行中的 callback；`0` 关闭预算。同时受已有的 10000 条队列容量限制 |
| `max_connection_body_mb` | `8` | 单连接 body 上限，接收分片时立即检查 |
| `max_sse_retained_mb` | `2` | 单流保留的 SSE payload 上限，包含未完整事件；最小有效上限为 1 KiB |
| `retain_raw_sse` | `false` | 保留原始 SSE 事件序列，关闭 provider delta 归约；仍受字节限额约束 |
| `connection_idle_timeout_secs` | `60` | 空闲软超时：只写一次 SQLite pending 行，并保留流以便恢复 |
| `connection_hard_timeout_secs` | `300` | 空闲硬超时：输出不完整记录并释放 HTTP/1 连接及 HTTP/2 流；`0` 关闭该超时 |
| `ring_buffer_mb` | `64` | eBPF ring buffer 大小，必须是 2 的幂 |

默认 `connection_capacity=128` 可容纳 128 个并发 HTTP/1 连接。应按所有 Agent 实际打开的连接数
选择容量，一个 Agent 可能打开多个连接。HTTP/2 的流条目容量为该值的四倍，并共用同一字节预算。
最近 5 秒有流量的 HTTP/1 连接与 HTTP/2 流会跳过容量驱逐。所有条目均受保护时，拒绝新调用并标记
`abandon_reason=capacity_rejected`，已有调用继续采集。保护覆盖请求组装、等待响应及流式接收阶段；
在途调用空闲满 5 秒后可被淘汰。
HTTP/1 调用完成后将名额继续保留 5 秒，供 keep-alive 复用，仅保存连接标识和完成时间。
保留名额计入 `connection_capacity`，无后续流量时过期；期间新连接可能被拒绝。
HTTP/2 流 ID 不会复用，因此已完成的流不保留名额。
硬字节上限仍可回收受保护条目。已有配置中显式设置的容量保持不变。修改运行时限额后需要重启采集。

CPU 调度默认开启，`cpu_budget_percent=25`；设为 `0` 可关闭。
完成记录在分析和写入之前按进程 CPU 预算调度，并与 primary/GenAI
维护共用工作准入。原始记录转移到有界队列，最多使用 pending 字节预算的一半；队列满时施加背压。
清理在小步骤之间让出执行权，后续调度继续处理。较低目标可能增加投递和清理延迟。
该参数是协作式目标，单次操作和其他进程线程仍可能导致超出目标；调整时需一起检查采集完整性、
探针丢事件、队列增长及尾部排空时间。
`agentsight_completion_queue_count`、`agentsight_completion_queue_bytes` 和
`agentsight_completion_wait_seconds` 反映延迟投递，`agentsight_cpu_budget_deferred_total` 记录预算等待次数。

容量淘汰、容量拒绝、同 key 替换、字节超限、空闲硬超时、进程退出或采集关闭时，AgentSight 输出
`finish_reason="incomplete"`、`abandon_reason` 和 `incomplete=true`。
容量淘汰、容量拒绝与字节超限仅保留有界的路由和关联元数据，设置 `metadata_only=true`，
不保留正文、消息与 token 用量，也不执行完整 payload 分析。每个字段上限 2 KiB，最多保留八个路由或关联
header；字节超限还包含 `truncated=true`。其他生命周期移除继续保留已捕获的部分证据。
即使关闭 SQLite，这些记录也会走正常 FFI 投递路径；v2 callback 暴露 metadata，typed callback 暴露 incomplete finish reason。
新流量会刷新空闲计时。软超时 snapshot 依赖 SQLite 并保留在途状态；FFI 模式无采集事件时仍执行硬超时维护。

仅投递语义事件时，SSE delta 会增量归约，原始 TLS buffer 在流结束前即可释放。需要原始事件序列的 exporter
应启用 `retain_raw_sse`；FFI raw HTTPS 回退开启时也会保留原始序列。两种模式均受资源限额约束。

通过 `AGENTSIGHT_METRICS_FILE` 导出的运行时指标包括
`agentsight_http_conn_removed_total{reason,state}`、`agentsight_http_abandoned_emitted_total{reason}`、
`agentsight_http_conn_active`、`agentsight_http_conn_retained_bytes`、`agentsight_ffi_inflight_bytes`
和 `agentsight_ffi_dropped_total{reason}`。FFI 队列过载时丢弃最新事件，并按 `count` 或 `bytes` 原因计数，
可据此区分捕获不完整与消费端积压。

## Agent 发现规则

`cmdline.allow` 决定哪些进程算作 Agent、叫什么名字。每条规则是一组命令行 token，支持 `*` 通配，
所有 token 需按顺序匹配。

```json
{ "rule": ["*node*", "*claude*"], "agent_name": "Claude" }
```

表示第一个参数含 `node`、下一个参数含 `claude` 的进程。

随包发布的规则覆盖 Hermes、Codex、Runloop、cosh（`Cosh`）、cosh-ng（`CoshNG`）、OpenClaw、
Claude Code、Qwen Code 和 AgentScope，共 31 条。

添加自己的 Agent，追加规则后 reload：

```json
{ "rule": ["*python*", "*my_agent*"], "agent_name": "MyAgent" }
```

```bash
sudo systemctl reload agentsight.service
```

然后确认规则已生效：

```bash
sudo agentsight discover --list-known | grep -i MyAgent   # 规则已列出
sudo agentsight discover                                   # 跑一次自己的 Agent，然后看它被匹配
```

`discover` 读取的是 tracer 所用的同一份 `config.json`，因此 `--list-known` 会反映你的新增规则
（用 `--config <path>` 可指向其他文件）。也可以以采集到的数据为准——Agent 跑起来后
`sudo agentsight summary --last 1` 的会话数应当非零；若一直没有数据，用
`journalctl -u agentsight.service` 查看日志。

`cmdline.deny` 用于剔除本会被匹配到的进程——默认那条规则把 `sftp-server` 子进程排除在数据之外。

两个实战要点：

- Rust、Go 编译出来的 Agent 二进制不会被 `node*` 这类规则匹配，需要为二进制名单独加规则。
- 包装进程同样重要。cosh-ng 会派生 `cosh-shell` 和 `cosh-core`，因此两者都有规则。

## 端点规则

| 配置节 | 用途 |
|---|---|
| `https` | 需要通过 uprobe 解密 TLS 流量的域名。自己的供应商域名不在里面时请补上。 |
| `http` | 通过 TCP 探针采集的明文 HTTP 目标。 |

```json
"https": [
  { "rule": ["dashscope.aliyuncs.com"] },
  { "rule": ["api.openai.com"] },
  { "rule": ["my-gateway.internal"] }
]
```

## Dashboard 认证

```json
"server": { "auth": { "enabled": true } }
```

令牌认证默认开启。本机回环访问免认证，远程访问需要令牌。只有在可信内网才建议把 `enabled` 设为
`false`，详见 [Dashboard 指南](dashboard.md#认证)。

## 死循环自动终止

```json
"deadloop": { "enabled": false, "kill_after_count": 3 }
```

默认关闭。开启后，当同一个工具调用循环重复 `kill_after_count` 次时，AgentSight 会终止该 Agent 进程。
死循环的检测和上报与这个开关无关，它只控制 AgentSight 是否动手，详见
[中断检测](interruption-detection.md#死循环处理)。

## 外部日志导出

```json
"runtime": { "sls_logtail_path": "" },
"features": { "sls_logtail": false }
```

`runtime.sls_logtail_path` 非空即开启基于文件的结构化事件导出，供外部日志采集器读取，且该路径支持
运行期热更新。想让数据完全留在本机就保持为空，详见
[数据与存储](data-and-storage.md#外部日志导出)。

## Codex offset

`codex_offsets` 保存 Codex CLI 各版本的符号偏移量——Codex 静态链接 TLS 库且不导出符号。AgentSight
会依次尝试符号表、字节模式匹配，最后才查这张表。如果新版 Codex 采集不到数据，用
`src/agentsight/scripts/extract-codex-offsets.py` 重新生成条目。

## schema_version 与升级

`schema_version`（当前为 `5`）标记配置格式版本。启动时 AgentSight 会与内置版本比对：

- 相同或更新 → 保留文件不动；
- 缺失或更旧 → 先复制为 `config.json.bak.<unix秒>`，再替换为当前默认配置。

schema v5 引入上述 HTTP 与 FFI 保留预算默认值，并下调 Probe 及延迟 GenAI 队列的字节预算。
旧版本配置会先备份后整体替换，需要从备份重新应用自定义发现规则和设置。

schema v4 把所有存储策略的按写入次数检查统一改为 `check_interval_secs`，并新增 `reuse`、`causal` 和
`enforcement`。旧 schema 中的自定义项不会自动合并。请在新文件上重新应用需要的规则，然后 reload 服务。

## 环境变量

| 变量 | 用途 |
|---|---|
| `AGENTSIGHT_TOKENIZER_PATH` | 本地分词器模型所在目录 |
| `AGENTSIGHT_ENFORCER_SOCKET` | enforcer socket 路径（默认 `/run/agentsight/enforcer.sock`） |
| `AGENTSIGHT_CHROME_TRACE` | 输出 Chrome trace 文件用于流水线性能分析 |
| `AGENTSIGHT_METRICS_FILE` | 将 Prometheus 运行时指标原子写入指定文件；未设置或设为空时关闭导出 |
| `AGENTSIGHT_METRICS_INTERVAL_SECS` | 运行时指标快照的最短间隔，单位为正整数秒（默认 `1`）；仅在设置 `AGENTSIGHT_METRICS_FILE` 时使用 |
| `RUST_LOG` | 日志级别，例如 `RUST_LOG=debug` |

运行时指标的更新频率不会高于设置的间隔。退出时即使间隔尚未到期，也会写入最终快照。
启用指标导出时，`0` 等无效间隔会导致启动失败。

## 改完怎么验证

```bash
sudo systemctl reload agentsight.service
systemctl is-active agentsight.service
sudo agentsight summary --last 1
```

如果服务起不来，多半是 JSON 写坏了：

```bash
python3 -m json.tool /etc/agentsight/config.json > /dev/null && echo "JSON ok"
journalctl -u agentsight.service -n 30 --no-pager
```

## Tokenless 统计 API

连接远程 Tokenless 时，在传给 `agentsight serve --config /etc/agentsight/config.json`
的配置中加入以下可选的顶层设置：

```json
{"tokenless_api":{"url":"http://127.0.0.1:7397","credential_file":"/etc/agentsight/tokenless-api-token"}}
```

凭据文件保存与 `tokenless stats serve` 相同的 Bearer token。使用私有网络或 HTTPS
代理。服务在启动时读取文件；修改后需重启 AgentSight。凭据仅保存在后端，非法配置
会阻止启动。不配置此项时，保留现有的本地数据库集成方式。
