# AgentSight 配置

[English](../../../en/agent-observability/agentsight/configuration.md)

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
  "schema_version": 4,
  "storage": {
    "base_path": "/var/log/sysak/.agentsight",
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
    "pending_genai_max_bytes_mb": 64,
    "pid_cache_size": 1024,
    "max_connection_body_mb": 8,
    "connection_idle_timeout_secs": 60,
    "ring_buffer_mb": 32
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

`storage.base_path` 是 AgentSight 自有数据库共用的目录。在 schema v4 中，每项策略统一使用
`retention_days`、`max_db_size_mb` 和 `check_interval_secs`。值为 `0` 时，分别关闭按时间保留、按容量清理
或定时维护；如果 `check_interval_secs` 为 `0`，即使另外两项非零也不会自动执行维护。

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

`check_interval_inserts` 已不再支持。`schema_version` 变化时配置格式整体替换：版本缺失或低于 v4 的文件
会先备份，再替换为 schema v4，不会合并旧设置。自定义配置请从随包发布的 `agentsight.json` 开始修改，
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
| 轨迹采集 | `features.trajectory_collection.enabled` | `false` | 周期扫描本地 Agent JSONL 会话写入 `trajectories.db`（仅 trace 模式） |
| 轨迹 LLM 判定 | `features.reuse_llm_judge` | `false` | 允许 `POST /api/reuse/judge` 调用已配置的 LLM 判定规则无法归类的轨迹；每次调用都会产生费用 |

`reuse_llm_judge` 只影响 `POST /api/reuse/judge`。除非已经配置优化 LLM 且明确需要付费的二级判定，否则请保持关闭。修改后 reload 服务，让 server 读取新配置。

随功能附带的调节项：

| 配置项 | 默认值 | 含义 |
|---|---|---|
| `features.tokenizer.cache_size` | `4` | 常驻内存的分词器模型个数 |
| `features.session_mapping.max_entries` | `10000` | response ID → session ID 映射上限 |
| `features.sqlite_storage.batch.max_size` | `100` | 每批写入行数 |
| `features.sqlite_storage.batch.flush_ms` | `100` | 批量写入的最大延迟（毫秒） |
| `features.trajectory_collection.scan_interval_secs` | `30` | 轨迹文件发现间隔；存储清理由 `storage.trajectories.check_interval_secs` 控制 |

## 运行时上限

这些参数限制流水线中所有内存缓冲区。忙的机器可以调高，内存紧张的机器可以调低。安装包 systemd unit
里的 `MemoryMax=350M` 是按默认值估算的。

| 配置项 | 默认值 | 含义 |
|---|---|---|
| `event_channel_capacity` | `10000` | 探针到流水线的有界通道容量 |
| `event_channel_policy` | `backpressure` | 通道满时的策略：`backpressure`、`drop_newest`、`sample` |
| `event_channel_max_bytes_mb` | `64` | 该通道中排队事件的字节预算，`0` 表示不限制。与容量同时生效：单条 SSL 记录最大可达 4 MiB，仅靠 10000 个槽位无法限定内存。超出预算的事件会被丢弃并计入日志 |
| `pending_genai_max_count` | `1000` | 等待 session ID 的事件条数上限 |
| `pending_genai_max_bytes_mb` | `64` | 同一队列的字节上限 |
| `pid_cache_size` | `1024` | PID → Agent 名称的 LRU 条目数 |
| `max_connection_body_mb` | `8` | 单个 HTTP 连接的 body 缓冲上限 |
| `connection_idle_timeout_secs` | `60` | 连接缓冲被丢弃前的空闲超时 |
| `ring_buffer_mb` | `32` | eBPF ring buffer 大小，必须是 2 的幂 |

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

`schema_version`（当前为 `4`）标记配置格式版本。启动时 AgentSight 会与内置版本比对：

- 相同或更新 → 保留文件不动；
- 缺失或更旧 → 先复制为 `config.json.bak.<unix秒>`，再替换为当前默认配置。

schema v4 把所有存储策略的按写入次数检查统一改为 `check_interval_secs`，并新增 `reuse`、`causal` 和
`enforcement`。旧 schema 中的自定义项不会自动合并。请在新文件上重新应用需要的规则，然后 reload 服务。

## 环境变量

| 变量 | 用途 |
|---|---|
| `AGENTSIGHT_TOKENIZER_PATH` | 本地分词器模型所在目录 |
| `AGENTSIGHT_ENFORCER_SOCKET` | enforcer socket 路径（默认 `/run/agentsight/enforcer.sock`） |
| `AGENTSIGHT_CHROME_TRACE` | 输出 Chrome trace 文件用于流水线性能分析 |
| `RUST_LOG` | 日志级别，例如 `RUST_LOG=debug` |

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
