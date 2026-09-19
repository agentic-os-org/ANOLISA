# AgentSec Security Middleware 跨语言契约

> PII 第一阶段 V2 已实现扩展见本文末尾专节；其版本化差异不修改 V1 oracle 基线。

| 属性 | 值 |
| --- | --- |
| 状态 | V1 Python 行为基线、compatibility fixtures 及 V2 Action Runtime 目标 |
| 实现核对日期 | 2026-08-19 |
| 实现核对提交 | `fe58ed4b23b8` |
| OTel V2 目标决策日期 | 2026-08-25 |
| 适用路径 | 当前 Python direct oracle；目标 asc-daemon -> daemon-core -> action-runtime -> CapabilityExecutor |

## 1. 文档地位

本文定义 V1 security middleware 的语言无关逻辑契约，并把可复用语义映射到 V2 Action
Runtime。V2 产品架构以仓库内迁移总计划为准；权威关系见
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)。
本文使用以下范围：

- **[CURRENT]**：当前 Python middleware 已实现的可观察行为；
- **[PRESERVE V1]**：supported 兼容接口在兼容期必须保持的结果、事件、错误和副作用；
- **[TARGET V2]**：asc-action-runtime、CapabilityExecutor 和 daemon adapter 的新增目标；
- **[SUPERSEDED]**：被当前仓库 V2 架构取代的 Python/PyO3/本地 fallback 目标；
- **[HISTORICAL]**：仅作设计依据，不构成当前或目标 contract。

它同时约束：

- **[CURRENT]** 当前 Python `security_middleware.invoke()` 的 oracle 语义；
- **[PRESERVE V1]** 被 compatibility inventory 选中的结果、事件和副作用；
- **[TARGET V2]** asc-daemon 经 daemon-core 调用 action-runtime 的唯一产品路径；
- 其它经正式 adapter 接入 action-runtime 的可信 V2 调用方。

本文规定行为、数据和副作用，不规定 Python function、Rust trait、async runtime 或
serialization library。当前 Python execution 是 **[CURRENT]** oracle；V2 action-runtime
必须在 protocol projection 前产生等价的 logical invocation、`ActionResult`、event 和 core
error。Python/PyO3 不进入 V2 runtime；daemon V1 compatibility adapter 使用第 8.3 节定义
的兼容投影。

各 action 的 params 和 data 见
[`SECURITY_ACTIONS_REFERENCE_zh.md`](SECURITY_ACTIONS_REFERENCE_zh.md)。daemon wire
protocol 是独立契约，见 [`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md)。

Rust-specific execution、lifecycle 和 observability 的候选实现架构见
[`RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md`](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md)。
该文档当前是 **[PROPOSAL]**，不属于六份语言无关行为契约；其中的 **[OPEN]** 决策只有进入
本文、对应 action/protocol contract 和可执行 fixture 后，才能成为实现约束。

## 2. 核心不变量

### 2.1 **[CURRENT][PRESERVE V1]** 业务语义

1. 所有安全 capability 通过一个逻辑 `invoke(action, context, params)` 入口调度。
2. action registry 是显式 allowlist，不允许按用户输入动态 import 或执行任意 backend。
3. 正常领域结果统一为 `ActionResult`；安全 verdict 与执行成功/失败必须分离。
4. 每次已路由 invocation 最多写一条 SecurityEvent：完成或异常二选一。
5. SecurityEvent 和 telemetry 均为 best-effort，写入失败不得改变业务结果或异常。
6. V1 trace/correlation context 在 CLI、daemon、middleware、event 和 telemetry 间保持
   当前语义。
7. 敏感字段的 audit projection 是 action contract 的一部分，不能由 adapter 自行决定。

### 2.2 **[TARGET V2]** Rust 执行路径

1. asc-daemon、Job 和其它正式入口通过 asc-daemon-core 调用同一 asc-action-runtime。
2. agent-sec-cli 是 daemon client；daemon 不可用时不使用 PyO3、Python backend 或通用本地 fallback。
3. Rust panic、V1 Python traceback 或底层任意异常不得穿过进程/protocol boundary 成为未结构化输出。
4. adapter 只能投影 Action Runtime result，不得复制或重新解释 CapabilityExecutor、lifecycle、event 和
   redaction 语义。
5. OpenTelemetry 是技术 TraceId、SpanId、parent 和 propagation context 的唯一权威；
   AgentSec 不维护并行的自定义 trace identity。
6. AgentSec 维护 Agent/security 领域的 span topology、semantic attributes 和安全投影；
   SecurityEvent 的可靠性不依赖 OTel sampling、exporter 或 Collector。

## 3. 逻辑数据模型

### 3.1 **[CURRENT][PRESERVE V1]** ActionInvocation

语言无关逻辑结构：

```json
{
  "action": "code_scan",
  "caller": "cli",
  "context": {
    "trace_id": "trace-1",
    "session_id": null,
    "run_id": null,
    "call_id": null,
    "tool_call_id": null,
    "agent_name": null,
    "timestamp": "2026-08-19T00:00:00+00:00",
    "invocation_id": "invocation-1"
  },
  "params": {
    "code": "echo ok",
    "language": "bash",
    "mode": "regex"
  }
}
```

此 JSON 是说明性 logical model，不是当前 Python API 或 daemon V1 的 wire envelope。
**[TARGET V2]** protocol DTO 与 domain ActionInvocation 必须有明确版本和转换边界；不得让
Serde 默认值或 transport DTO 改变本节字段语义。

### 3.2 **[CURRENT][PRESERVE V1]** ActionContext

| 字段 | 类型 | 语义 |
| --- | --- | --- |
| `action` | non-empty string | registry key |
| `caller` | string | 调用来源；无法识别时为 `unknown` |
| `trace_id` | non-empty string | 优先继承调用方；缺失时生成 UUID |
| `session_id` | string/null | session correlation |
| `run_id` | string/null | agent run/turn correlation |
| `call_id` | string/null | LLM call correlation |
| `tool_call_id` | string/null | tool call correlation |
| `agent_name` | string/null | telemetry 的 Agent 产品候选值 |
| `timestamp` | ISO-8601 string | invocation 创建时间，默认 UTC now |
| `invocation_id` | non-empty string | 一次 CLI/进程 invocation 的关联 ID |

显式 context 字段优先于 ambient context；未显式设置的 correlation 字段可以从当前
request/process context 继承。仍无 trace ID 时生成新 UUID。

当前 Python adapter 的 caller 解析规则是：显式 `caller` 优先；否则调用栈中
`sandbox-guard.py -> sandbox-guard`、`cli.py -> cli`；仍未识别时为 `unknown`。
**[TARGET V2]** action-runtime 不检查 V1 Python stack；trusted adapter 在调用前解析 caller
attribution，并独立构造 Principal。

### 3.3 **[TARGET V2]** OTel context 与 AgentContext

V2 把技术 tracing context 与 Agent 业务语义分开：

```json
{
  "trace_context": {
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01",
    "tracestate": "vendor=opaque"
  },
  "agent_context": {
    "agent_name": "qwen-code",
    "session_id": "session-001",
    "run_id": "run-7",
    "call_id": "llm-call-3",
    "tool_call_id": "tool-call-9"
  }
}
```

该 JSON 是语言无关 logical carrier，不是 daemon V1 wire 变更。V2 规则如下：

- `traceparent` 使用 W3C Trace Context 格式并承载上游 TraceId、parent SpanId 和 flags；
  `tracestate` 是可选传播字段；
- V2 不接受单独的 arbitrary `trace_id` 输入，不把 V1 opaque `trace_id` 解析、hash 或
  padding 成 OTel TraceId，也不以 invocation ID/UUID 作为 fallback trace；
- 合法 carrier 由 OTel propagator 提取；carrier 缺失或不合法时，由当前执行 host 的 OTel
  SDK 创建新 root trace；
- `agent_name/session_id/run_id/call_id/tool_call_id` 是可选业务关联字段，进入
  `agentsec.*` semantic attributes；它们不建立 span parent-child 关系；
- Agent 业务字段缺失时保持 absent，不由 AgentSec 伪造 session/run/call/tool-call ID；
- `traceparent/tracestate` 均是不可信关联输入，不得作为 caller identity、授权、
  principal、幂等、去重或重放依据。

OTel active context 由 SDK 与 instrumented execution scope 维护，不作为普通 domain
`ActionContext` 字符串字段逐层复制。AgentSec 的语言无关职责是规定在哪里建立 span、使用
哪些有界 attributes，以及如何把当前 span identity 投影到日志和 SecurityEvent。

### 3.4 ActionResult

```json
{
  "success": true,
  "data": {},
  "stdout": "",
  "exit_code": 0,
  "error": "",
  "error_type": ""
}
```

| 字段 | 类型 | 规则 |
| --- | --- | --- |
| `success` | boolean | backend 操作是否按产品语义完成；不等同于安全 verdict |
| `data` | object | action-specific structured result，默认 `{}` |
| `stdout` | string | CLI 可直接渲染的文本，默认空串 |
| `exit_code` | integer | CLI 语义退出码；0 通常表示执行完成 |
| `error` | string | 人类可读错误，成功时为空 |
| `error_type` | string | 无消息的错误分类；当前兼有固定 token 与 legacy Python exception name，成功时为空 |

**[CURRENT]** 已知产品路径使用 `ValueError`、`FileNotFoundError`、
`NativeScannerUnavailable`、`CodeScanError`、`PromptScanError`、`SkillLedgerError` 等固定
token；部分捕获路径仍使用 `type(exception).__name__`，因此当前全集不能称为稳定产品
错误目录。

**[PRESERVE V1]** 已知固定 token 和其触发条件必须兼容。**[TARGET V2]** action-runtime 必须定义
语言无关的固定错误 token 映射；无法归入已知 token 的 legacy exception name 作为 opaque
兼容值处理，不得依据 Rust type name 临时生成新的公开分类。

必须分别解释三层状态：

| 层 | 示例 | 含义 |
| --- | --- | --- |
| transport | daemon socket timeout | 没有 ActionResult，是否执行可能不明确 |
| product execution | `success=false`, `error_type=ValueError` | action 输入或执行失败 |
| security decision | `success=true`, `verdict=deny` | scanner 正常完成并判为高风险 |

`deny`/`warn` 不是基础设施错误。以当前 code/prompt/PII scanner 为例，正常产生 deny 时
`success=true, exit_code=0`；只有 scanner `error` verdict 才是执行失败。

## 4. 当前 action registry

registry 恰好包含：

| action | backend capability | SecurityEvent category |
| --- | --- | --- |
| `sandbox_prehook` | sandbox decision audit | `sandbox` |
| `harden` | Security Baseline/loongshield | `hardening` |
| `verify` | asset signature verification | `asset_verify` |
| `summary` | local SecurityEvent summary | `summary` |
| `code_scan` | code security scanner | `code_scan` |
| `prompt_scan` | prompt injection scanner | `prompt_scan` |
| `pii_scan` | PII/credential scanner | `pii_scan` |
| `skill_ledger` | Skill Ledger commands | `skill_ledger` |

未知 action 必须在 CapabilityExecutor execution 前失败。当前 Python public API 抛出 `ValueError`，
写 diagnostic error log，但因为没有 backend 可执行 action-specific projection，不写
SecurityEvent。**[TARGET V2]** action-runtime 可以使用 `Result::Err(UnknownAction)`；V1
oracle harness 只比较稳定错误语义，daemon adapter 映射为 `unknown_method`。不得把未知
action 伪装为某个已注册 action 的 ActionResult。

registry 当前为静态 mapping。当前 Python 进程按 action lazy-create 并复用 backend
instance；调用方不得依赖对象 identity。**[TARGET V2]** Rust 可以复用无状态实例或安全管理
共享资源，但必须保证并发安全，不得因 cache 策略改变结果和副作用。

## 5. Invocation 生命周期

### 5.1 正常路径

```text
resolve caller/context
  -> diagnostic "action started" (DEBUG)
  -> allowlist route
  -> pre_action (当前 no-op)
  -> backend.execute(context, params)
  -> post_action: one SecurityEvent + telemetry projection
  -> diagnostic completion (INFO when exit_code=0, otherwise WARNING)
  -> return ActionResult
```

当前 `pre_action` 明确不写 request event。不得同时写 `<action>_request` 和 completion，
否则会破坏“一 invocation 一事件”。

### 5.2 Backend 返回失败 ActionResult

backend 返回 `success=false` 时仍走 `post_action`，不抛异常：

- SecurityEvent `result=failed`；
- details 包含 request/result；
- 非空 `error` 加入 details；
- 非空 `error_type` 时 details 还加入 `error_type` 和 `exit_code`；
- diagnostic completion level 由 `exit_code` 决定。

### 5.3 Backend 抛出未处理异常

```text
backend exception
  -> on_error: one failed SecurityEvent + telemetry
  -> diagnostic ERROR with local exception info
  -> rethrow/mapped core error to caller
```

当前 Python `invoke()` 在记录后重新抛出原异常。**[TARGET V2]** Rust 实现不得 panic；应返回
结构化 core error，由同进程 Rust application service 转为对应 error path。跨 daemon
wire 时必须进一步转为 daemon 的稳定 error envelope。

### 5.4 Lifecycle 自身失败

- SecurityEvent writer 失败：吞掉异常，仍尝试 telemetry。
- telemetry 失败：吞掉异常，不影响 SecurityEvent 或 ActionResult。
- action-specific detail builder、event construction 或 lifecycle 其它逻辑失败：吞掉异常，
  不改变 backend result/exception。

这种 best-effort 只适用于 audit transport，不允许吞掉 backend 业务错误。

### 5.5 **[TARGET V2]** tracing lifecycle

V2 invocation 的最小 span topology 是：

```text
daemon.request
  -> security.invoke
       -> security.backend
       -> security_event.persist  # 可选 child span
```

- adapter 在进入 core 前提取或创建 OTel context；`security.invoke` 覆盖 route、action
  validation、admission、backend completion 和唯一 finalizer；
- capability span 必须是 invocation span 的 child；正常失败、timeout、cancelled 和 panic
  conversion 使用固定 status/error code，不记录原始 exception、request 或 result；
- finalizer 在 `security.invoke` 结束前捕获它的 TraceId/SpanId，供 SecurityEventV2
  使用；不得误存 event persistence child span 的 SpanId；
- session/run/call/tool-call/action/backend/policy/verdict 只作为经过 allowlist、长度限制
  和脱敏的 attributes；
- tracing/logging/export 失败不改变 ActionResult，也不跳过 SecurityEvent sink attempt；
- asc-daemon 即使未配置 exporter，也必须有有效 OTel SDK context，不能退化回 V1 自定义
  trace ID。

### 5.6 **[TARGET V2]** 已实现的共享 scan lifecycle 切片

`asc-action-runtime` 统一执行与 finalization；daemon composition root 一次装配 audit、
telemetry 和 diagnostic outputs，handler 只通过 daemon-core Action application 调用。
正常结果与受控 execution failure 均由同一 owner 尝试终态记录。audit 与 telemetry
投影/写入失败相互隔离，不改变业务结果；投影失败只生成最小安全审计详情。

输出在请求 blocking worker 同步尝试后再返回，transport timeout/断连不自动取消已运行的
blocking work；进程退出不保证尚未完成的记录。whole-invocation `duration_ms` 属于诊断，
scanner `elapsed_ms` 保持原始业务语义。生产 sink 不允许隐式 no-op 默认值。

本切片仅包含 code-scan 的 identity、真实能力、telemetry 投影及 fixture；其它扫描能力
随各自后续提交加入共享机制，不预留未实现 identity 或 RPC。受控异常使用固定 `InternalExecutionError` 和空 request，明确替代
Python 原始 exception 审计文本。现有正常 audit schema 不变，OTel 与 metadata ingress
仍待后续工作，不能把本切片当作 SMC-017–023 完成。
实现、fixture 对应表与回滚见 [共享生命周期验收记录](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)。

## 6. SecurityEvent 契约

### 6.1 **[CURRENT][PRESERVE V1]** Envelope

每个已路由 invocation 完成后生成：

```json
{
  "event_id": "uuid",
  "event_type": "code_scan",
  "category": "code_scan",
  "result": "succeeded",
  "timestamp": "2026-08-19T00:00:00+00:00",
  "trace_id": "trace-1",
  "pid": 1234,
  "uid": 1000,
  "session_id": null,
  "run_id": null,
  "call_id": null,
  "tool_call_id": null,
  "details": {
    "request": {},
    "result": {}
  }
}
```

`event_type` 等于 action；category 使用第 4 节映射；result 只取
`succeeded/failed`。`ActionResult.data.verdict` 不改变 event result：正常完成的 deny
scanner 仍是 `succeeded`。

### 6.2 默认 details projection

默认完成事件：

```json
{
  "request": "deep copy of params",
  "result": "deep copy of ActionResult.data",
  "error": "only when success=false and error non-empty",
  "error_type": "only when failure has error_type",
  "exit_code": "only when failure has error_type"
}
```

默认异常事件：

```json
{
  "request": "deep copy of params",
  "error": "exception string",
  "error_type": "exception class/product type"
}
```

这意味着除有专用 sanitizer 的 action 外，当前本地 SecurityEvent 可能保存 code、prompt、
command、path 等原始 request。兼容迁移不得无意改变；进一步数据最小化应作为单独产品
变更，配套 reader/UI/审计兼容评审。

### 6.3 强制 sanitizer

#### `pii_scan`

本地 event 绝不能保存原始 `text`、`raw_evidence` 或 `redacted_text`：

- request 只保留 `source`、`text_length`、`text_sha256`、`max_bytes`、
  `include_low_confidence`、`redact_output`、`input_truncated`；
- result finding 删除 `raw_evidence`，可以保留 `evidence_redacted`；
- result 删除 `redacted_text`；
- error message 固定为 `pii_scan error details omitted from audit`，保留 error type。

#### `skill_ledger`

- request 中非 null `passphrase` 必须替换为 `[REDACTED]`；
- result 用 snake_case event projection，scanner-owned `metadata` 内容保持 opaque；
- command-specific 状态投影为顶层 event result `verdict`，批量结果取最严重值；
- sanitizer 不得修改返回给调用方的原始 `ActionResult.data/stdout`。

其它 action 当前使用默认 projection。

### 6.4 **[TARGET V2]** OTel identity projection

SecurityEventV2 增加显式版本和标准 span identity：

```json
{
  "schema_version": 2,
  "event_id": "uuid",
  "event_type": "code_scan",
  "category": "code_scan",
  "result": "succeeded",
  "timestamp": "2026-08-25T00:00:00+00:00",
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "span_id": "3333333333333333",
  "pid": 1234,
  "uid": 1000,
  "session_id": "session-001",
  "run_id": "run-7",
  "call_id": "llm-call-3",
  "tool_call_id": "tool-call-9",
  "details": {
    "request": {},
    "result": {}
  }
}
```

- V2 `trace_id` 必须是当前 `security.invoke` span 的 32 位小写十六进制 OTel TraceId；
- V2 `span_id` 必须是同一 span 的 16 位小写十六进制 OTel SpanId；
- `tracestate` 不进入 SecurityEvent；
- 缺少 `schema_version` 的历史记录按 V1 解释，其 `trace_id` 仍是 opaque correlation；
  reader/query 在迁移窗口必须支持混合 V1/V2 数据并显示 schema 语义；
- V2 之后不再生成或保存新的 AgentSec legacy trace ID，也不定义
  `agentsec.correlation.trace_id` attribute；
- SecurityEvent 是独立的本地安全审计记录。OTel sampling decision、export queue、exporter
  或 Collector 故障不能使已路由 invocation 的 event 因此消失。

## 7. Telemetry projection

同一 SecurityEvent 会 best-effort 投影为隐私收敛的 telemetry record。Telemetry 不是
SecurityEvent 的完整副本，必须使用固定 allowlist：

- 允许组件、Agent 产品、action/category/result/timestamp；
- scanner 只允许稳定 verdict/elapsed 等固定字段；
- verify/harden 只允许固定计数和结构化错误；
- 禁止 prompt、code、command、path、stdout/stderr、evidence、secret、原始 error、
  correlation IDs 和未知开放式容器。

完整门控、sentinel、字段矩阵见
[`telemetry-security-event-sync.md`](telemetry-security-event-sync.md)。**[TARGET V2]** Rust
lifecycle 必须复用同一 allowlist 语义；不能因为不再经过 Python telemetry mapper 而扩大
字段。

当前 telemetry 文件投影与 OTel pipeline 是两个独立 sink。引入 OTel 不自动把 TraceId、
SpanId 或 Agent correlation 写入现有 telemetry L1；若要增加，必须单独更新其 allowlist、
隐私审查和 fixture。两者任一失败均不得阻塞另一者或 SecurityEvent。

## 8. V1 oracle 与 Rust adapter

### 8.1 **[CURRENT]** Python public oracle

V1 Python 层当前提供：

```python
invoke(action: str, *, caller: str | None = None, **params) -> ActionResult
```

V1 fixture 继续比较 `result.success/data/stdout/exit_code/error/error_type`，但该 Python API
不是 V2 runtime entrypoint，也不要求 V2 打包 Python 或 PyO3。

### 8.2 **[TARGET V2]** Action Runtime API

asc-action-runtime 可以采用等价形式：

```text
invoke(ActionInvocation) -> Result<ActionResult, CoreInvokeError>
```

其中 `CoreInvokeError` 只表示没有 ActionResult 的 routing/unhandled failure。Rust panic
必须在 process/protocol boundary 前转换为受控 internal failure。Action Runtime 依赖
CapabilityExecutor port，不依赖具体 capability 或 asc-daemon-core。

### 8.3 **[TARGET V2]** daemon handler adapter

daemon 不通过一个接受任意 action 名称的 wire router 暴露 core。每个 action 由
asc-daemon-protocol 中的固定 method handler 显式注册；handler 根据 method 选择唯一 action，
从 trusted Principal 和已验证 carrier 构造 context，然后只调用一次 asc-daemon-core action
use case。daemon-core 再调用 action-runtime，不复制 CapabilityExecutor 语义。

daemon V1 的 `trace_context.trace_id` 继续按第 3.2 节作为 compatibility 字段处理。未来
daemon V2 handler 只提取/注入 `traceparent/tracestate`，不得同时开放另一个 raw
`trace_id` 技术入口；UDS 不是 HTTP 也不改变 W3C carrier 字符串语义。

该模式来自 commit `ef0d75f27c389434cf6f4361f5dbcdeaff42ab72` 的历史
`scan-prompt` handler。目标实现复用 handler registration、blocking boundary、context
propagation 和三层 response，删除其 Python scanner/preload 耦合。完整 method 和 wire
projection 见
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md#daemon-security-action-handler-contract)。

V1 compatibility adapter 中 daemon `ok` 与 `ActionResult.success` 不得合并：handler 返回失败 ActionResult 时 daemon
仍返回 `ok=true`。历史 V1 wire 只投影 `data/stdout/error->stderr/exit_code`；内部
`success/error_type` 不新增为 daemon 顶层字段。只有 handler 之前或 handler 本身的 daemon
boundary failure 才返回 `ok=false`。需要 product error type 的新 wire consumer 必须使用
未来的版本化扩展，不能改变 V1 projection。

## 9. 并发、取消和副作用

### 9.1 **[CURRENT][PRESERVE V1]** 共享语义

- middleware contract 不承诺 backend object identity。
- timeout/cancellation 不等于 action 未执行；调用方不得自动重放有副作用 action。
- 同一个 logical invocation 只能由一个 lifecycle owner 写 SecurityEvent。

### 9.2 **[TARGET V2]** Rust 并发边界

- action-runtime 必须允许并发 read action，不依赖 Python GIL 或 ambient singleton。
- SQLite、Skill Ledger key/manifest/snapshot/activation 和文件更新使用事务、原子替换或明确
  的 repository concurrency contract；不能只依赖某个 task-local mutex。
- blocking 文件、数据库、模型、CPU 和外部进程工作进入有界 resource class，不阻塞 RPC
  accept、timeout、authorization 或 shutdown 推进。

## 10. 兼容与变更规则

以下变更需要同步更新本文、action reference、Python/Rust fixtures 和用户文档：

- action 增删或重命名；
- params 默认值、类型、枚举或忽略/拒绝规则变化；
- ActionResult 字段或 success/exit/verdict 语义变化；
- event category、single-event lifecycle、correlation propagation、span topology 或
  OTel carrier 变化；
- sanitizer、redaction、telemetry allowlist 变化；
- action 从只读变为写入，或副作用/幂等性变化；
- backend exception 与 ActionResult failure 之间的映射变化。

新增 optional action data 字段可以兼容，但必须确认 consumer 是否做严格 schema 校验。
删除或改型现有字段必须有版本化迁移方案。

## 11. 双实现验收矩阵

### 11.1 **[CURRENT][PRESERVE V1]** 当前语义基线

| ID | 验收行为 |
| --- | --- |
| SMC-001 | registry 恰好包含 8 个 action，未知 action 一致失败 |
| SMC-002 | context 继承、显式优先、UUID/timestamp/invocation ID |
| SMC-003 | `ActionResult` 六字段、默认值和类型 |
| SMC-004 | lifecycle 顺序及 single-event invariant |
| SMC-005 | success=false 返回失败 event，但不自动抛异常 |
| SMC-006 | backend unhandled error 写一条失败 event 后传播为 core error |
| SMC-007 | event writer 与 telemetry 任一失败不影响业务 |
| SMC-008 | 8 个 action/category 映射 |
| SMC-009 | PII 原文/raw evidence/redacted output 不进入 event |
| SMC-010 | Skill Ledger passphrase 脱敏且不修改业务 result |
| SMC-011 | V1 trace/session/run/call/tool-call IDs 按当前 opaque correlation 语义完整传播 |

### 11.2 **[TARGET V2]** Rust 路径验收

| ID | 验收行为 |
| --- | --- |
| SMC-012 | 当前 Python oracle 与 Rust action-runtime 对同 fixture 的六字段 ActionResult、core error、event 和副作用等价 |
| SMC-013 | daemon、Job 和其它入口调用同一 lifecycle owner，每次 invocation 最多一条 event |
| SMC-014 | panic/traceback/secret 不跨 process 或 protocol boundary 泄露 |
| SMC-015 | daemon 不存在时 agent-sec-cli 返回稳定 unavailable，不启动用户 daemon、不使用 PyO3 或本地业务 fallback |
| SMC-016 | V1 daemon adapter 只比较 `data/stdout/stderr/exit_code` projection 与 `ok` response layer；内部 action-runtime 比较完整六字段 ActionResult |
| SMC-017 | V2 ingress 只使用 `traceparent/tracestate`，不存在 AgentSec 自定义 trace ID 生成、fallback、hash 或格式转换 |
| SMC-018 | 合法上游 carrier 在 daemon request、action invocation 和 CapabilityExecutor 间保持同一 TraceId，并形成可验证的父子 SpanId |
| SMC-019 | carrier 缺失或不合法时创建有效 root trace；未配置 exporter 时 daemon 路径仍有有效 TraceId/SpanId |
| SMC-020 | Agent/session/run/call/tool-call/action/backend/policy/verdict 使用有界、脱敏 semantic attributes，不替代 OTel identity |
| SMC-021 | SecurityEventV2 保存 `security.invoke` TraceId/SpanId；sampling/exporter/Collector 故障不改变 ActionResult 或 event sink attempt |
| SMC-022 | V1/V2 event 由 schema version 区分并可混合读取；V2 不产生新的 legacy trace ID |
| SMC-023 | TraceId/SpanId/`traceparent` 不参与 authorization、principal、idempotency、deduplication 或 replay decision |

比较 current Python oracle 与 Rust action-runtime result 时，可以规范化 UUID、
PID/UID、timestamp、elapsed/duration 和临时绝对路径；不得忽略 `success`、data schema、
stdout、exit code、error type、event projection、redaction 和文件/数据库副作用。比较 daemon
V1 时只检查其四字段 action projection、`ok` response layer、event 和副作用；不得要求从
V1 response 重建未传输的 `ActionResult.success/error_type`。

## 12. 当前实现证据

- 入口与 orchestration：`agent-sec-cli/src/agent_sec_cli/security_middleware/__init__.py`。
- registry：`security_middleware/router.py`。
- context/result：`security_middleware/context.py`、`result.py`。
- lifecycle/event projection：`security_middleware/lifecycle.py`、
  `security_events/schema.py`。
- default/custom audit detail：`security_middleware/backends/base.py`、
  `pii_checker/audit.py`、`security_middleware/backends/skill_ledger.py`。
- tests：`tests/unit-test/security_middleware/test_context.py`、`test_result.py`、
  `test_router.py`、`test_invoke.py`、`test_lifecycle.py`。

## 13. V2 tracing 标准依据

- [W3C Trace Context](https://www.w3.org/TR/trace-context/)；
- [OpenTelemetry Context propagation](https://opentelemetry.io/docs/concepts/context-propagation/)；
- [OpenTelemetry Trace API](https://opentelemetry.io/docs/specs/otel/trace/api/)。

## PII 第一阶段 V2 Runtime 与 Finalizer

**[TARGET V2，已实现]** `action.pii_scan` 使用 Handler → ActionService → Invocation /
ActionRuntime → PiiScanExecutor → PiiAuditProjector → Finalizer。daemon 启动逻辑负责组装规则、
具体 Runtime 和输出；Handler 与 ActionService 不依赖具体检测 capability。
已识别且已授权的请求发生参数错误时，由 ActionService 构造固定错误及空请求投影，
通过公共 `Invocation.reject` 进入同一个 Finalizer，随后返回，不调用 Executor。
执行 panic 沿用 Runtime 的受控 `InvokeError`，收尾后由 Handler 返回安全的内部 RPC 错误。
正常扫描、部分完成、扫描失败和参数拒绝各在正常生命周期内提交一次扫描终态；无效信封、
未知方法、授权失败及传输失败在对应入口收尾。崩溃或强制退出不保证 exactly-once。

PII 投影按白名单构造，只持久化摘要、长度、source、规则标识、coverage、脱敏 findings、
有界 trace/agent 元数据及安全错误码；不存原文、raw_evidence、完整 redacted_text、规则内容、
输入中字段的原始拼写或任意异常信息。参数拒绝不持久化未校验 params。
扫描状态与 JSONL/SQLite 存储健康分别验证；启动时 SQLite 初始化要求保持不变。

Finalizer 的审计、遥测与 diagnostic 输出沿用公共框架并隔离故障。PII 遥测只在既有生命周期
字段之外投影 verdict 与耗时；不写入原文、findings、完整脱敏文本或规则内容。
V1 opaque trace 别名保留在此兼容入口，归一化业务字段和 `agent_name` 传入
`ActionAttribution`；UID/GID/PID 仍来自 UDS peer。它们不能转换或宣称为 OTel TraceId。
ActionRuntime 是执行服务，不等同 PIP；未来 PIP adapter 将复用执行和 Finalizer，
PDP/PEP 不在此阶段实现。可执行验证为 capability `tests/runtime.rs`、公共 runtime/sink 测试及
`tests/v2/e2e/test_pii_cli_e2e.py`；设计见 [PII 两阶段设计](PII_V2_MIGRATION_zh.md)。
