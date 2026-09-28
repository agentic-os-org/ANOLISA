# V2 可观测单条采集契约与验收

状态：首批实现；以 `oss/main c7d3e5888` 的 OTel 集成为基线。
本批只有采集与落盘，不包含 Session/Run/Timeline 查询、report/review/schema 命令、
插件切换或 OTLP/Collector 导出。V1 Python 仅用于生成回归 oracle，不是 V2 运行依赖。

## 1. CLI 兼容接口 [PRESERVE V1]

```bash
agent-sec-cli observability record --format json --stdin <<'JSON'
{
  "hook": "before_tool_call",
  "observedAt": "2030-01-02T03:04:05Z",
  "metadata": {"sessionId": "session-001", "runId": "run-001", "toolCallId": "tool-001"},
  "metrics": {"tool_name": "read_file", "parameters": {"path": "README.md"}}
}
JSON
```

`--stdin` 必须指定；`--format` 默认 `json`，只接受 `json`，一次读取一个 JSON object。
成功退出 0，stdout 无输出；输入、连接或写入失败退出 1，stderr 报错。
Clap 层未知选项等 usage error 仍退出 2。输入在解析前限制为 4 MiB；请求编码后还受
现有业务帧 4 MiB（含 LF）与 carrier 独立 32 KiB 限制。

CLI 复用 `ObservabilityRecord` 的 Hook/metrics/metadata 校验及序列化：未知顶层字段、
metadata 额外字段和未知 metrics 被忽略；没有任何已知 metric 时拒绝。
本批以六种 Hook 和 24 个日期转换/拒绝样例的 V1 frozen fixtures 验证兼容性。
CLI 接受带时区的时间字符串，也兼容 V1 的秒/毫秒数值时间戳与十进制时间戳字符串，
统一转换为带时区的字符串发送给 daemon。日期 fixtures 包含 V1 对负浮点数与负数
字符串不同的转换行为；这些是有界差分证据，不宣称穷尽 Pydantic 的所有隐式转换。

CLI 用 `bind_metadata` 将本条输入的 metadata 绑定到 OTel Context，然后注入 carrier。
本条 session/run/call/tool 字段替换继承值，包括清除缺失的可选字段；技术父链路、
独立 agent.name 与兼容标签保留。不把 metadata 复制到 daemon params。
特殊字符按现有 Baggage 编码处理；metadata 字符串沿用 246 Unicode 字符截断、不 trim。

## 2. daemon 接口 [TARGET V2，新增]

UDS 请求采用既有单条 JSON + LF framing：

```json
{
  "method": "obs.record",
  "params": {
    "hook": "before_tool_call",
    "observedAt": "2030-01-02T03:04:05Z",
    "metrics": {"tool_name": "read_file", "parameters": {"path": "README.md"}}
  },
  "traceContext": {
    "version": 1,
    "baggage": "agentsec.session.id=session-001,agentsec.run.id=run-001,agentsec.tool_call.id=tool-001"
  }
}
```

`params` 三个字段均必填，拒绝额外字段（尤其是 `metadata`），不支持批量。
`hook` 与 `observedAt` 为字符串，`metrics` 为 object。
`traceContext` 完全复用已有 V1 carrier，可以携带 `traceparent`、`tracestate`。
没有有效技术父链路时 daemon 创建 root；必需 attribution 缺失则采集失败。

- 所有 Hook 都要求 Baggage 中的 `agentsec.session.id` 和 `agentsec.run.id`。
- `before_tool_call` / `after_tool_call` 额外要求 `agentsec.tool_call.id`。
- `agentsec.call.id` 对模型调用和工具调用可选；AgentRun 不落盘 call/tool 字段。
- `agentsec.agent.name` 可通过 OTel 传递；现有持久化 record metadata 不新增 agentName 字段。
- 沿用现有 metadata 存在性检查，空字符串属于字符串，不自动生成替代 ID。
- Baggage 不是授权身份；method 使用 `LocalUser`，身份来自已认证 UDS peer。

响应复用现有 `{requestId,result}` / `{requestId,error}` 信封：

```json
{"requestId":"daemon-generated-id","result":{}}
```

| 结果 | 错误码 / 语义 |
|---|---|
| 非法参数、Hook、时间、metrics 或必需 metadata 缺失 | `invalid_argument`，不写入 |
| embedding 未装配采集 service | `unavailable`，不写入；生产入口显式装配 |
| 写入前已取消 | `deadline_exceeded` |
| JSONL 或 SQLite 写入失败 | `internal`，提示 partial write is possible |

错误响应使用固定安全消息，不回显 payload 或文件路径。`requestId` 是调用关联标识，
不是记录 ID 或去重键。协议非法 carrier/envelope 继续使用既有 `invalid_request` 语义；
格式有效但 Baggage 内容被传播层丢弃后，缺少必需 metadata 则是 `invalid_argument`。

## 3. 数据路径与生命周期

`asc-cli → asc-daemon-handler → asc-daemon-core::ObservabilityService → ObservabilitySink`
由 daemon composition root 注入 `ConfiguredObservabilitySinks`；core/handler 不依赖具体
SQLite 或文件 writer。core 在请求的 OTel Context 中读取 metadata，交给现有领域 parser
校验、过滤并生成持久化记录。与 span sampling、Collector 和 exporter 是否启用无关。

文件使用 daemon 已解析的系统数据目录，默认 `/var/log/agent-sec`，部署可用绝对路径
`AGENT_SEC_DATA_DIR` 覆盖；不回退 HOME。文件为 `observability.jsonl` 和 `observability.db`，
复用现有 owner-only writer。采集 writers 惰性初始化，存储故障由该次调用返回。

1. 先写 JSONL；失败就不尝试 SQLite。
2. 再写 SQLite；失败不撤销已追加的 JSONL。
3. 两次写入均成功后返回空对象 acknowledgement；不承诺跨文件原子性或断电级 fsync。
4. 不自动重试、不去重。连接中断或超时可能发生在写入后，调用方不得假定“失败即未写”。
5. shutdown 停止 admission、drain transport/blocking 工作后，对已初始化 writer 调用 close，
   沿用 SQLite 的关闭时保留期维护；现有默认保留期为 7 天，未新增后台清理任务。
   有界 drain 超时仍沿用现有进程退出语义，不保证所有在途操作完成。

## 4. 验收与变更记录

| ID | 验收 | executable evidence | 本次结果 |
|---|---|---|---|
| OBS-001 | V1 六种 Hook 输入、日期转换与持久化投影兼容 | `v2/fixtures/observability/v1-records.json`；`v1-timestamps.json` / `timestamp_coercions_match_v1_oracle`；CLI `v1_records_become_business_params_and_native_baggage`；E2E `test_v1_cli_records_persist_and_survive_restart` | PASS |
| OBS-002 | 元数据经 Baggage 传递，清除继承可选字段，保留 Unicode/空格和兼容标签 | 同一 CLI fixture test；同一 restart E2E（unsampled carrier） | PASS |
| OBS-003 | raw UDS 非法参数/缺少 metadata 无写入 | E2E `test_daemon_rejects_invalid_params_and_missing_metadata_without_writes` | PASS |
| OBS-004 | 并发隔离、无 carrier 不继承其他请求 | E2E `test_concurrent_ingestion_isolates_baggage_and_no_carrier_does_not_inherit` | PASS |
| OBS-005 | 双写失败不吞错、不重试；部分写入保留 | E2E `test_write_failures_surface_without_retry_or_rollback` | PASS |
| OBS-006 | CLI 非法输入失败、daemon 不可用不 local fallback | CLI `input_errors_precede_any_daemon_call`；E2E `test_cli_input_errors_and_unavailable_daemon_never_fall_back` | PASS |
| DPROC-022 | 显式系统目录、文件权限、重启后记录存续 | E2E `test_v1_cli_records_persist_and_survive_restart` | PASS |

E2E 文件：`tests/v2/e2e/test_observability_record_e2e.py`。
执行：在 `v2` 下 `cargo build -p asc-cli -p asc-daemon --locked`，然后从 agent-sec-core 执行
`PATH="$PWD/v2/target/debug:$PATH" agent-sec-cli/.venv/bin/python -m pytest tests/v2/e2e/test_observability_record_e2e.py -q`。
这些是源码构建本地进程/真实文件与 SQLite 验收，不代表 RPM/systemd、跨 UID 授权或断电恢复验收。

内部变化：新增 `ObservabilityRecordParams` / allowlisted `obs.record` / core service 与 sink port；
复用 OTel carrier，现有 DB schema 和 JSONL record schema 不变。
外部变化：V2 CLI 新增 V1 风格 record 命令；持久化由 daemon 执行，失败不启动 daemon 或回退本地。
回滚先停止新 CLI 调用，再回退 daemon；旧 daemon 对该方法返回 unknown_method。
本批不修改 V1、插件、包或存量数据，也不要求数据库逆向迁移。

本次本地验收：上述 OBS-001..006、DPROC-022 均通过；涉及 9 个 Rust crate 的 350 项测试通过，
采集/OTel E2E 与架构检查合计 23 项通过，Clippy、rustfmt、Python lint/格式检查通过。

### 内部 API 清理：可观测全局 writer

移除 V2 Rust 的 `record_observability`、全局 JSONL/SQLite accessors、初始化查询及
对应测试注入入口。唯一双写装配入口为 daemon 持有的
`ConfiguredObservabilitySinks::record`，关闭使用该实例的 `close()`；
`shutdown_sinks()` 仅处理遗留全局安全事件 writer。

兼容分类：内部 Rust API 有意删除；V1 Python、CLI、`obs.record` wire、落盘格式及
失败顺序不变。原双写成功/两类失败测试迁至
`configured::observability_tests`，另在成功用例校验惰性初始化和显式关闭维护。
直接消费者仍为 `asc-daemon`。回滚可整体恢复本次内部 API 清理，无数据迁移。

本次清理验收：`cargo test -p asc-event-sink --all-features --locked` 27 项通过；
CLI/daemon 重建通过；`test_observability_record_e2e.py` 7 项通过（允许 UDS 的沙箱外运行）；
该 crate 的 all-targets/all-features Clippy（`-D warnings`）通过。

### V1 CLI E2E 场景迁移回归

`tests/e2e/cli/test_observability_record_jsonl_e2e.py` 与
`test_observability_record_sqlite_e2e.py` 的输入、CLI 参数及落盘断言已移植到
`tests/v2/e2e/test_observability_record_e2e.py` 的两个 `test_v1_observability_record_*`
用例，通过真实 V2 CLI → daemon 执行。V1 测试文件未修改。

差异分类：V2 daemon 启动会预初始化安全事件存储，因此将 V1 的“安全事件文件不存在”
断言替换为“本次采集前后安全事件 JSONL 内容和 SQLite 行数不变”。其余业务断言保留。
源码 CLI/daemon 重建后，该文件全部 9 项通过（其中移植场景 2 项）；这是本地源码
二进制回归证据，不代表 RPM/systemd 验收或完整 V1/V2 等价。
