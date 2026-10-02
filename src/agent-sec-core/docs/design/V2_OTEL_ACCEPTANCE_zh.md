# V2 OTel tracing 实现与验收

本工作包实现 Rust CLI → client → UDS → daemon → PAP/compiler 的 tracing，及
SecurityEvent/observability 消费者使用的只读关联投影。code-scan 已将快照接入现有
SecurityEvent JSONL/SQLite 和 telemetry JSONL；不新增存储 schema、本地链路重组、
历史查询或尚未迁移的安全 capability。架构设计见 [实现设计](V2_OTEL_IMPLEMENTATION_DESIGN_zh.md)。

本次为首次 OTel 接入。V1/V2 是产品实现版本；V1 caller 的 trace-context/metadata 输入
仍受支持，适配这些输入不表示存在旧版 OTel，也不表示接口已废弃。兼容关联标签专指
caller 提供的 opaque trace_id / invocation 标签，不包含另一套技术 tracing 身份。

## 实际落地

- `asc-observability`：一个 OTel Context 承载 SDK identity、五个 Baggage 字段和兼容标签；
  `snapshot()` 不依赖采样或 exporter。`bind_trace_context_input()` 对照冻结 V1 oracle；
  `bind_metadata(parent, value, kind)` 先按 hook 校验再替换记录字段，保留 metadata 首尾空白；
  `validate_metadata()` 区分普通调用与必填 metadata 消费者。
- `runtime` feature 仅由 CLI/daemon 入口启用，负责真实 SDK、固定未采样策略、独立 span/log
  过滤与本地诊断 worker。无公开 exporter 或 OTLP 配置；业务模块使用原生 tracing 埋点。
- 原生请求新增 version 1 `traceContext` 和 `compatibility`，严格 schema。每个 handler 请求从
  干净 context 开始，scope 覆盖授权、PAP 和响应编码。超时不提前结束仍执行中的 handler span。
- CLI 保留 V1 `--trace-context` bootstrap 位置、别名、last-wins 和错误退出码 1；新增
  `--otel-context`。client 在 child span 内注入请求副本，保持原参数、deadline 和不重试行为。
- 请求保留 4 MiB 业务容量，额外 32 KiB propagation 容量；响应仍为 4 MiB，均包含 LF。
  业务计量保留原始 whitespace/escape 字节，不通过重新序列化缩小请求。
- SDK 0.32.0 / SDK implementation 0.32.1 / bridge 0.33.0 已锁定；关闭 bridge 默认 metrics/log features。
  CLI 不启动应用级 Tokio runtime 或 HTTP client；只保留本地诊断 worker。

实现文件在 `v2/crates/asc-observability/src/{lib,fields,propagation,runtime}.rs`；
protocol DTO 在 `asc-daemon-protocol/src/trace.rs`。没有第二份 Agent context store 或自定义 TraceId。

## 可复现检查

从组件目录执行；真实 socket/进程测试需要运行环境允许 UDS 和子进程：

```bash
cargo fmt --all --manifest-path v2/Cargo.toml --check
cargo clippy --workspace --all-targets --manifest-path v2/Cargo.toml --locked
cargo +1.88.0 check --workspace --manifest-path v2/Cargo.toml --locked
cargo test --workspace --manifest-path v2/Cargo.toml --locked
cargo build --workspace --manifest-path v2/Cargo.toml --locked
PATH="$PWD/v2/target/debug:$PATH" uv run --project agent-sec-cli pytest tests/v2/e2e/test_otel_e2e.py -v
```

当前范围的 workspace Rust tests 和真实 CLI/daemon 本地日志测试已执行通过；
不保留 mock Collector 或 OTLP exporter 测试，不以历史导出测试结果作为当前交付证据。
沙箱内 socket 被拒绝，真实 UDS 验收在允许本机 socket 的环境执行。

性能不设验收要求：不比较不同机器的启动延迟、请求耗时、吞吐或峰值内存。
进程测试保留 10 s 外层 watchdog 检测挂死。
关闭预算、业务 deadline、取消后的 span 生命周期和诊断阻塞隔离仍是功能要求；
watchdog 通过不表示精确测量或证明 50 ms / 2 s 的关闭开销。

## 覆盖矩阵

以下 PASS 针对“本次已执行范围”；原设计 TO/UF 中更广的组合测试不因此自动成为全覆盖。

| 验收项 | 本次证据 | 结果与边界 |
| --- | --- | --- |
| TO-001/007/008 | `asc-observability/tests/context.rs` | PASS：native root/child、晚绑定后 contextual/explicit child、投影与恢复；交错 task/blocking、task abort 后同一 worker、blocking worker 复用、panic 恢复；metadata task 保留技术 ID/请求引用/兼容标签 |
| TO-009 | context + `asc-observability/tests/runtime.rs` | PASS：真实 runtime 固定未采样，环境 always_on/off × RUST_LOG info/off 不影响当前 ID/Baggage；未知调优设置不输出诊断；普通消息 canary 不进入关联日志 |
| TO-002/003/004/005 | context tests + `test_otel_e2e.py` | PASS（列明的正反例）：remote parent、flags/tracestate、无 parent、只带 Baggage、重复 Baggage key/非法编码/超长及成员上限、Unicode 往返；不等于完整 W3C 规范符合性 |
| TO-006 | protocol `tests/tracing.rs` + process schema case | PASS：版本、null、重复字段、未知字段，原始 UDS 拒绝且不执行业务 |
| TO-010 | writer 单测 + process blocked stderr case | PASS：队列饱和丢诊断；启动前填满 stderr 后仍完成请求、重复 daemon 启动失败和关闭，RUST_LOG info/off 均覆盖。公开 exporter 故障测试已移出范围 |
| TO-017 | process SIGTERM/blocked stderr + runtime shutdown test | PASS：正常与 stderr 阻塞时进程退出完成，stdout 无诊断污染；残留 blocking 工作不无限阻止应用 runtime 退出，不声称精确测量关闭耗时 |
| TO-011/013 | 真实非 root peer 伪造 Baggage 用例 + 原有 PAP/CLI CRUD、拒绝、revision、digest、CAS、JSON/退出码 fixtures | PASS：保持当前 V2 行为；Agent attribution 不参与 Principal 构造 |
| TO-012 | process correlation + client/context + `tracing_failures.rs` | PASS：两个真实二进制日志共享 trace、五字段和兼容标签；SDK 内存检查验证 parentage、PAP→compiler 成功/失败 span。未以本地日志声称证明跨进程每层 parent |
| TO-019 | 无 | [SUPERSEDED] 公开 OTLP 导出与 wire 验收移出本期，无 mock Collector |
| TO-014/016 | `tracing_failures.rs` + process canary + runtime tests | PASS：真实 UDS busy/read timeout/invalid envelope/unknown method/permission denied/handler panic 的内部 span 范围和错误类别；params/panic/普通消息/未知 Baggage 不进入新增关联诊断，原用户输出另按业务契约验收 |
| TO-015 | `asc-daemon/tests/tracing.rs` | PASS：真实 UDS deadline 返回后 handler span 仍打开，工作完成后闭合并记录 `cancel_requested` |
| TO-018 | client `tests/tracing.rs` + 当前原始请求 fixtures | PASS：显式 carrier 隔离、request 不变、daemon 错误只返回一次且不降级重发；旧 daemon 不属于支持范围；无 carrier 请求仍可用 |
| TO-020 | Cargo.lock + product/workspace feature trees | PASS：一套 OTel 0.32 类型；OTLP/reqwest/hyper/AWS-LC 已退出 lockfile；AgentSight Client 的 ureq/rustls/ring 保留 |
| TO-021 | protocol budget + 新增 process maximum Unicode case | PASS：4 MiB 业务帧同时携带五个各 256 个四字节字符的 Baggage，全部字段到达 daemon；业务多一个空白字节、传播预算超限分别拒绝；并发请求按 admission 功能测试验收，不做内存性能基准 |
| UF-001/002/003 | CLI bootstrap tests + frozen `v1-trace-context-normalization.json` | PASS：输入适配、别名优先级、Python 空白/Unicode 截断；实际 Agent hook 业务命令不在当前迁移范围 |
| UF-004/005/006 | context/runtime + process 未采样并发与标签用例 | PASS：opaque/invocation 标签、raw UDS root；同一 trace 的 24 个并发请求各有独立 request_span_id、开始/结束日志成对且归属一致；子 span 日志保持请求引用。范围为统一 diagnostic helper，未迁移的 V1 日志访问入口另行验收 |
| UF-010 | 原 CLI goldens + process 关闭用例 | PASS：当前 PAP stdout、默认 stderr、退出码及公开 UUID；尚未迁移的真实 hook 整体超时未宣称通过 |
| UF-011 | 冻结 `metadata.json` 的 138 个 V1 schema 输入 + context tests | PASS：AgentRun/ModelCall/ToolCall 的缺失/null/类型/别名/extra/空白/截断；带父值时仍先校验自身输入，可选字段清除；技术 parentage/agent_name/标签保留；子 span/task/carrier 投影及未采样读取。真实 observability RPC/存储仍属后续模块 |

没有以 Markdown ID 的存在替代执行。初始 tracing 切片仅证明未采样快照可读；下述
OTEL-CR-009 增加了真实 code-scan 的 JSONL/SQLite 持久化和 telemetry 隐私验收。

标准语法校验仍依赖锁定的 SDK；例如 SDK 0.32 的 `TraceState` 构造会接受重复 vendor key，
本轮没有将它扩展为独立的 W3C 全规范验证器。TO-005 的通过结论仅限已列出的正反例，
不能据此宣称所有 malformed tracestate 都会被拒绝或清空。

## 兼容性与内部变更记录

| 记录 | 决策 | 对外影响 |
| --- | --- | --- |
| OTEL-CR-001 | 原生 PAP envelope 增加可选 versioned carrier | 仅支持新 CLI/new daemon；部署先升级 daemon，旧 daemon 不属于兼容门禁，不做自动重试 |
| OTEL-CR-002 | 旧 opaque trace ID 作为兼容标签 | 不转换、哈希或伪装成 OTel TraceId；V1 历史记录不在这里迁移 |
| OTEL-CR-003 | 一个 Context + Baggage + native spans | 业务函数不添加 metadata/context 参数；首期只有 PAP/compiler 埋点 |
| OTEL-CR-004 | 本期仅本地 OTel Context 与关联日志 | 取消公开 OTLP exporter、采样和 batch 配置；固定 AlwaysOff，本地 ID/Baggage 保留。有界诊断不得改变业务结果，原 CLI 结果输出保持 |
| OTEL-CR-005 | 不新增 invocation UUID 或诊断请求 ID 生成器 | 显式 invocation 标签保留；当前公开 PAP requestId UUID 保留；日志以 trace_id + request_span_id 关联 |
| OTEL-CR-006 | 独立请求 propagation 容量 | 4 MiB 业务 + 32 KiB propagation；响应不扩容；raw whitespace 不被隐藏 |
| OTEL-CR-007 | metadata 使用同一传播通道但保留自身值语义 | V1 trace-context trim；metadata 不 trim。SDK 注入会 trim，适配器以 percent encoding 保留原值 |
| OTEL-CR-008 | metadata adapter 增加 hook kind，先校验再替换记录字段 | V1 必填/null/alias/extra 规则保持；父值不掩盖缺失，省略/null 的可选字段清除。agent_name 由原 trace-context/carrier 提供；只改内部 helper，无新增 RPC 或 caller 参数 |
| OTEL-CR-009 | 共享 Finalizer 构造两类记录时复用一个 Context snapshot | 既有 audit 五个关联字段获得请求归属，trace_id 保持 opaque 兼容标签；telemetry 仅增加白名单 agent_name 值，无新字段、schema 或授权输入 |

直接消费者：两个产品入口、同步 client、dispatcher/rejection encoder、PapService、compiler 和
共享 Finalizer；ActionService 仅传递可信 CallerIdentity，Finalizer 构造两类记录时复用一次
Context 快照，两个 sink 只写入完整记录。仅新增
`traceContext/compatibility` wire 字段；没有修改 PAP domain model、revision、授权、
资源 ID、事件/数据库 schema 或 Agent 插件配置。

### OTEL-CR-009：Context 到扫描输出

`tests/v2/e2e/test_scan_lifecycle_process.py` 使用真实 CLI、UDS、daemon 和独立 SQLite reader：

- regex 成功与 llm 受控失败均保存五个兼容关联字段，JSONL/SQLite 值一致；
- 同时提供原生未采样 traceparent/Baggage 和旧 trace-context 时，兼容输入覆盖业务归属，
  SDK TraceId 不替换 event.trace_id；原生输入没有 opaque 标签时该列保持空串；
- 后续无 context 的请求不继承前次字段；16 个并发请求的所有关联字段不串线；
- audit 写失败时 telemetry 仍使用同一快照的 agent_name；telemetry 不可用时 audit 仍保存 correlation；
- `codex`/`hermes` 产品名到达 telemetry；未知产品为空串，私有 correlation 和未知字段
  canary 不进入 telemetry JSONL。生产 SDK 固定 AlwaysOff，测试不需要 Collector。

`asc-action-runtime/tests/lifecycle.rs` 在不初始化 SDK 的条件下附加 Context，按相同
8 个 V1 frozen cases 验证两个 sink 收到的完整 audit/telemetry 投影。
移除内部 `ActionAttribution`/`Correlation`，`Invocation` 改为接收 `CallerIdentity`；
外部 RPC 和事件 schema 不变。Finalizer 必须在有效 Context scope 内执行；sink 接收的记录已经包含所需归属。

```bash
uv run --project agent-sec-cli pytest tests/v2/e2e/test_scan_lifecycle_process.py tests/v2/test_action_architecture.py -q
```

这是源码构建进程验收，不代表 RPM/systemd、OTLP 导出或 versioned SecurityEventV2 已验收。
本次验证：workspace Rust tests 786 passed；scan lifecycle/architecture pytest 8 passed；
既有 OTel process pytest 14 passed；build、fmt、clippy（`-D warnings`）与 rustdoc 均通过。
回滚该接线只需移除 Finalizer 的 Context 投影；既有记录和 SQLite schema 无需迁移。

## Review 修复验收

本轮 workspace Rust tests 通过；pytest E2E **8 passed，0 skipped**。
这些为本机验证，不宣称 GitHub CI job 已执行。

- 范围：移除生产 exporter/config 与 mock OTLP；保留内部 SDK span 检查、本地日志及
  V1 输入兼容 fixtures。OTEL 环境变量无法开启导出或改变固定采样策略。
- 打印审计：daemon PAP 警告、signal/runtime/bind/serve 错误和异常链使用同一有界 writer；
  OTel 初始化失败使用最多等待 50 ms 的临时 worker；均无同步 fallback。
  CLI 帮助/usage/结果/业务错误与 daemon 帮助/参数错误为必需输出，保留同步语义及背压。
- 日志：writer 单测覆盖阻塞/满队列/超长；pytest 在进程启动前填满 stderr，覆盖正常启动、
  请求拒绝和响应、队列饱和、CLI 正常退出、重复 daemon 启动失败与 SIGTERM。
  RUST_LOG info/off 均执行；不是精确延迟基准。
- 传播：context tests 覆盖未知字段非法 UTF-8 隔离、单次 header 注入、转义往返，以及
  8 KiB 内 SDK 互通和超出后仅本地 16 KiB adapter 保留的边界；不声称全 SDK 全容量互通。
- client：无可注入 SDK context 时保留显式 carrier；runtime 测试覆盖重复初始化冲突。
- 测试入口：用例位于 `tests/v2/e2e/test_otel_e2e.py`，构建二进制后由 pytest 执行；
  binary 从 PATH 解析，支持本地构建与安装态；现有 `make test-e2e-rpm-v2` 已收集 `tests/v2/`。
  本次仅运行本机源码测试，不将已有 CI 配置视为 RPM/systemd 已通过的证据。
  缺少 binary/socket 权限导致失败，非 root 授权用例仅在 root 环境中明确 skip，不计为通过。

内部变更：生产 exporter 配置入口退出范围；无效允许值仍丢弃整个 Baggage，但未知值不再做 UTF-8
解码；metadata/归属字段值不变，ASCII wire 转义可更简洁。JSON 诊断改为有界 best-effort 队列，
SecurityEvent 持久化契约不变。新 CLI/旧 daemon 不在支持范围，无 capability 协商或兼容降级。

生产初始化还将 Rust 默认的同步 panic hook 替换为同一有界 writer，仅输出固定
`runtime: panic`，不记录 panic payload，也不改变 unwind/abort 或业务错误映射。
内部 runtime 子进程测试验证 caught panic 的固定诊断及 payload 隔离。

## Rebase 后的诊断完整性回归

主线新增的 reconciliation、JSONL 和 SQLite 错误路径原来直接写 stderr。
填满 stderr 后注入 JSONL 写失败，实际复现 CLI 5 s 超时；修复为标准 `tracing` 事件，
由进程 subscriber 将 `asc_process_diagnostic` 消息交给现有有界 writer。
该 target 不受 RUST_LOG 控制；无 subscriber 的库宿主不输出诊断，也不隐式创建线程。
CLI 业务输出、daemon 参数错误/help 保持同步；SecurityEvent 仍由原持久化路径独立写入。

- `test_storage_fault_diagnostics_do_not_block_scan_or_shutdown`：JSONL 写失败、SQLite 插入失败、
  高版本 schema，均在 stderr 已满时验收扫描响应、独立 audit sink 和退出，覆盖 info/off。
- `process_diagnostics.rs`：真实 worker 的 repository 失败及未确认 terminalization 诊断不能阻止 join。
- `runtime.rs`：后台线程的进程警告在 info/off 均可见，普通非白名单消息和 panic payload 不输出。
- 初始化前的 daemon panic hook 也采用最多等待 50 ms 的临时 writer，保留源码位置脱敏测试。
- E2E 不再硬编码 `target/debug`，通过 PATH 使用构建或安装的产品 binary；完整 RPM/systemd 未在本次运行。

## 部署与回滚

新 CLI 仅支持接入 carrier 的新 daemon。先升级 daemon，再升级 CLI/其他 native caller。回滚时先停止/回滚发送新 carrier 的 caller，
再回滚 daemon；`RUST_LOG=off` 不关闭 carrier，不能替代协议回滚。
没有状态 schema 迁移，生产没有 exporter 开关。

后续工作：versioned SecurityEventV2 技术 TraceId/SpanId、其它 capability、实际 observability RPC、
AgentSight 跨服务传播、历史查询与本地链路重组各自按其业务契约验收。
