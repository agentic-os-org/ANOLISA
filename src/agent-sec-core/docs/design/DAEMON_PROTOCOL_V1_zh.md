# AgentSec daemon 协议 V1

| 属性 | 值 |
| --- | --- |
| 状态 | V1 wire 基线、兼容语料及 V2 asc-daemon-protocol 候选扩展 |
| 实现核对日期 | 2026-08-19 |
| 实现核对提交 | `fe58ed4b23b8` |
| wire version | V1（当前 envelope 无显式 version 字段） |

## 1. 文档地位与版本

本文是 AgentSec daemon V1 wire protocol、默认 method catalogue 和 action handler 候选
扩展的权威规范。V2 产品架构以仓库内迁移总计划为准；V1 Python client/daemon 只作为
oracle，不进入 V2 runtime。Rust compatibility adapter 和测试工具不得维护语言私有字段
或错误语义。
权威关系见
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)。

本文使用以下范围标记：**[CURRENT]** 表示 V1 Python daemon 已实现，**[PRESERVE V1]**
表示 supported V1 接口在兼容期必须保持，**[TARGET V2]** 表示与仓库内迁移总计划一致的
新增目标，**[HISTORICAL]** 仅表示设计依据。V1 行为必须先进入 compatibility inventory，
不能因当前存在就自动决定 V2 的进程、身份或数据架构。

“V1”是本文对现有协议的版本名称。当前业务 request/response **没有**顶层
`schema_version` 字段；兼容实现不得仅因缺少版本字段而拒绝请求。未来协议协商属于
V2 设计，不能在 V1 兼容迁移中单方面加入必填字段。

控制面生命周期、权限和并发见
[`DAEMON_CURRENT_BEHAVIOR_zh.md`](DAEMON_CURRENT_BEHAVIOR_zh.md)；Job 生命周期、调度、
日志、trace 和恢复语义见 [`DAEMON_JOB_CONTRACT_zh.md`](DAEMON_JOB_CONTRACT_zh.md)。
security action 不通过
当前默认 method catalogue 执行；迁移目标通过本文 6.12 的显式 handler 扩展接入。
其逻辑契约见
[`SECURITY_MIDDLEWARE_CONTRACT_zh.md`](SECURITY_MIDDLEWARE_CONTRACT_zh.md)。

## 2. Transport 与 framing

### 2.1 普通连接

- transport：当前用户拥有的 Unix domain `SOCK_STREAM`。
- 编码：UTF-8 JSON object。
- framing：一帧一行 NDJSON，序列化时以单个 `LF` (`0x0a`) 结束。
- 生命周期：一个连接一个 request、一个 response；response 后服务端关闭连接。
- 普通 request 的第一帧可以由 LF 或 EOF 终止。
- client 必须发送完整 request 后读取第一条 response；不得依赖同连接的多请求复用。
- 服务端收到同一读取中的额外 request frame 时只处理第一帧并关闭连接。

V1 默认 request 和 response frame 上限均为 4194304 bytes（4 MiB），wire 上限包含
末尾 LF。未完成 frame 超过上限时必须返回 `payload_too_large`；响应序列化后超过上限
时也使用 `payload_too_large`。例外是启用 notify authentication 后、第一帧尚不能安全
分类的 timeout/oversize：服务端按认证 fail-closed 规则静默关闭连接。

### 2.2 一般 JSON 规则

- request 和 response 顶层必须是 JSON object。
- request 必须是有效 UTF-8 和 JSON。
- `params`、`trace_context` 必须是 JSON object；缺省为 `{}`。
- V1 request 的未知顶层字段被忽略，以允许调用方携带旧 `id` 等字段。
- method-specific 未知 `params` 是否拒绝由各方法单独规定，不能推断为全局规则。
- response `data` 可以是任意 JSON value；当前默认方法返回 JSON object。

## 3. Request envelope

### 3.1 Schema

```json
{
  "method": "sec.events.list",
  "params": {
    "limit": 100,
    "include_details": false
  },
  "trace_context": {
    "trace_id": "trace-1",
    "session_id": "session-1",
    "run_id": "run-1",
    "call_id": "call-1",
    "tool_call_id": "tool-1",
    "agent_name": "qwen-code"
  },
  "caller": "agentsight",
  "timeout_ms": 5000
}
```

| 字段 | 类型 | 必填 | 规则 |
| --- | --- | --- | --- |
| `method` | string | 是 | trim 后必须非空；dispatch 使用原字符串，调用方不得发送首尾空白 |
| `params` | object | 否 | 默认 `{}`；由 method 做进一步校验 |
| `trace_context` | object | 否 | 默认 `{}`；只保留已知 string correlation 字段 |
| `caller` | string | 否 | 非空 string 会 trim；其它值或空白值归一为缺失 |
| `timeout_ms` | integer/null | 否 | 正整数，boolean 不算 integer，最大 300000 |

调用方提供的 `id` 或 `request_id` 被忽略。daemon 必须为每个已解析请求生成新的 UUID
request ID；在 request 尚未解析时发生错误，也必须为错误响应生成 fallback request ID。

### 3.2 Trace context

支持以下 snake_case/camelCase alias，snake_case 非空合法值优先：

| 规范字段 | alias |
| --- | --- |
| `trace_id` | `traceId` |
| `session_id` | `sessionId` |
| `run_id` | `runId` |
| `call_id` | `callId` |
| `tool_call_id` | `toolCallId` |
| `agent_name` | `agentName` |

值必须是 trim 后非空 string；无效值被忽略。每个 correlation value 最长 256 个字符，
超出时保留前缀并追加 `...[truncated]`，最终长度仍为 256。

daemon 本身不会为缺失的 `trace_id` 自动生成 trace ID。官方 client 在发送前会以当前
invocation ID 补齐 `trace_id`；其它协议 client 可以发送空 context。无 trace ID 时，
request ID 仍然存在，但二者不能混用。

### 3.3 Timeout

执行 deadline 按以下规则选择：

1. request 含合法 `timeout_ms` 时使用调用方值；
2. 否则使用 method registry 的默认值；
3. method 未注册时仍返回 `unknown_method`，而不是以 timeout 掩盖 allowlist 错误。

首帧读取超时是 transport 配置，和 `timeout_ms` 无关。当前默认二者均为 5000 ms，
但语义不同。

## 4. Response envelope

### 4.1 成功 dispatch

```json
{
  "request_id": "96b8bc11-b95c-4a24-a7fb-8f33bd56be9d",
  "ok": true,
  "data": {},
  "stdout": "",
  "stderr": "",
  "exit_code": 0
}
```

### 4.2 daemon failure

```json
{
  "request_id": "96b8bc11-b95c-4a24-a7fb-8f33bd56be9d",
  "ok": false,
  "data": {},
  "stdout": "",
  "stderr": "unknown daemon method: missing.method",
  "exit_code": 1,
  "error": {
    "code": "unknown_method",
    "message": "unknown daemon method: missing.method"
  }
}
```

| 字段 | 类型 | 规则 |
| --- | --- | --- |
| `request_id` | non-empty string | daemon 生成的关联 ID |
| `ok` | boolean | method 是否成功 dispatch/执行到 handler result |
| `data` | JSON value | 默认 `{}`；method-specific result |
| `stdout` | string | 默认 `""` |
| `stderr` | string | 默认 `""`；daemon error 时等于 `error.message` |
| `exit_code` | integer | 默认 0；boolean 无效 |
| `error` | object/null | `ok=false` 时包含 string `code`、`message` |

`ok=true` 表示 daemon boundary 成功，不保证领域操作成功；handler 可以返回
`ok=true, exit_code!=0`。调用方必须先判断 `ok`，再解释 method 的 `data` 和
`exit_code`。`ok=false` 时不得把 `data` 当作业务结果。

V1 parser 对 `ok` 与 `error` 的组合不做交叉字段强校验，但兼容服务端必须遵循：

- 成功响应不包含 `error`；
- 错误响应包含 `error`，`data={}`、`stdout=""`、`exit_code=1`；
- 不向 `internal_error` 响应泄露任意 exception、traceback 或 secret。

## 5. 稳定错误目录

| code | 触发边界 | V1 默认 message 形式 | 可重试含义 |
| --- | --- | --- | --- |
| `bad_request` | UTF-8/JSON/envelope/method params 校验失败 | 具体字段错误 | 修正请求后再试 |
| `unknown_method` | method 不在 allowlist | `unknown daemon method: <method>` | 升级/修正 method |
| `payload_too_large` | request 或 response 超过 byte limit | `request|response payload exceeds <n> bytes` | 缩小 payload |
| `timeout` | request read 或 method deadline 超时 | `daemon request timed out after <n> ms` | 状态可能不明确，写操作不得自动重放 |
| `busy` | 活动连接达到上限 | `daemon is busy` | 可由调用方显式重试；不得默认本地重放写操作 |
| `unavailable` | capability/job/runtime path 当前不可用 | capability-specific message | 依原因处理 |
| `internal_error` | 未预期异常、无效 handler result、序列化失败 | `daemon internal error` | 报告实现错误 |
| `shutdown` | daemon 已停止接收工作或在途任务被取消 | `daemon is shutting down` | 重新连接新实例 |

所有当前错误 exit code 都是 1。`ResponseTooLarge` 也映射为
`payload_too_large`，不新增 wire code。

若配置的 response byte limit 小到连结构化错误响应也无法容纳，服务端可以关闭连接而
不发送 frame；client 将其分类为 transport/protocol failure，而不是 daemon response。

## 6. Method catalogue

### 6.1 通用参数类型

#### Pagination

- `limit`：正 integer，默认 100，最大 1000；`obs.timeline.get` 默认 1000。
- `offset`：非负 integer，默认 0，最大 `9223372036854775807`。
- boolean 不能作为 integer。
- page response 的 `next_offset` 在仍有下一页时为 `offset + limit`，否则为 `null`。

#### Time range

时间范围可以使用：

- `since` / `until`：非空 ISO-8601 string；naive time 按共享 timestamp 规则归一到 UTC；
- `start_ns` / `end_ns`：Unix epoch nanoseconds integer，转为 UTC ISO-8601。

`since` 与 `start_ns` 互斥，`until` 与 `end_ns` 互斥；start 不得晚于 end。

#### Security-event filters

以下 filter 可供 `sec.summary`、`sec.events.list` 和 `sec.events.count_by` 使用：

| 字段 | 类型/值域 |
| --- | --- |
| `event_type` | optional non-empty string |
| `category` | optional non-empty string |
| `result` | `succeeded` 或 `failed` |
| `trace_id` | optional non-empty string |
| `session_id` | optional non-empty string |
| `run_id` | optional non-empty string |
| `call_id` | optional non-empty string |
| `tool_call_id` | optional non-empty string |
| `verdict` | optional non-empty string |
| time range | 上述两组时间字段 |

optional string 的空白值归一为未设置。

### 6.2 `daemon.health`

参数：当前忽略所有 `params`。

```json
{
  "status": "ok",
  "pid": 1234,
  "uptime_seconds": 12.5,
  "socket": "/run/user/1000/agent-sec-core/daemon.sock",
  "prompt_scan": {
    "status": "ready",
    "model": "native",
    "loaded": true,
    "last_error": null,
    "last_started_at": null,
    "last_finished_at": null
  },
  "jobs": [
    {
      "name": "skill-ledger-activation",
      "state": "running",
      "last_error": null,
      "last_tick_at": null
    }
  ],
  "queues": {"inflight": 1, "queued": 0}
}
```

`prompt_scan` 是兼容 stub，不表示 daemon 提供 prompt scan method。

### 6.3 `skill_ledger.skillfs_notify_change`

参数 schema version 2，五个字段全部必填且禁止未知字段：

```json
{
  "schemaVersion": 2,
  "canonicalSkillDir": "/absolute/lexically/normalized/skill",
  "skillId": "opaque/reported-id",
  "eventKind": "write",
  "paths": ["SKILL.md", "scripts/run.sh"]
}
```

规则：

- `canonicalSkillDir` 必须是绝对、lexically normalized 的 canonical Skill path；
- `skillId` 是非空 opaque string，不要求等于 basename；
- `eventKind` 恰好是 `mkdir/create/write/rename/unlink/rmdir/setattr/truncate/reconcile`
  之一；
- `paths` 是相对 `canonicalSkillDir` 的非空 string 列表；每个 path 不得是 absolute、
  包含 NUL 或 `..`；列表本身当前可以为空，`reconcile` 使用空列表表示全量协调；
- 全部 path 的第一段都是 `.skill-meta` 且列表非空时，接受但忽略，不 enqueue。

普通 enqueue 响应：

```json
{
  "schemaVersion": 2,
  "accepted": true,
  "ignored": false,
  "queued": true,
  "coalesced": false,
  "skill": {
    "canonicalSkillDir": "/absolute/skill",
    "skillName": "skill",
    "reportedSkillId": "opaque/reported-id",
    "eventKinds": ["write"],
    "paths": ["SKILL.md"]
  }
}
```

metadata-only 响应包含 `ignored=true`、`reason="metadata-only change"` 和 `skill`，
不包含 `queued/coalesced`。job 未注册或未运行时返回 `unavailable`。

### 6.4 `sec.summary`

参数：security-event filters，加 `latest_limit`（正 integer，默认 5，最大 50）。

返回：

```json
{
  "total": 0,
  "by_category": {},
  "by_event_type": {},
  "by_result": {},
  "affected_sessions": 0,
  "affected_runs": 0,
  "latest_events": []
}
```

`latest_events` 不包含 `details`。

### 6.5 `sec.events.list`

参数：security-event filters、pagination、`include_details` boolean（默认 false）。

```json
{
  "items": [],
  "total": 0,
  "limit": 100,
  "offset": 0,
  "next_offset": null
}
```

每个 event 使用 canonical SecurityEvent envelope；`include_details=false` 时删除
`details`。若 details 可投影出 verdict，item 顶层增加 `verdict`；Skill Ledger event
还可以增加 `command`、`skill_name` dashboard 字段。

### 6.6 `sec.events.get`

参数：必填非空 string `event_id`。

```json
{"found": false, "event": null}
```

找到时 `event` 包含完整 `details` 和 dashboard projection 字段。

### 6.7 `sec.events.count_by`

参数：必填 `group_by`、security-event filters。`limit` 和 `offset` 明确禁止。

`group_by` 值域恰好为：

`category`、`event_type`、`result`、`trace_id`、`session_id`、`run_id`、`call_id`、
`tool_call_id`、`verdict`。

```json
{
  "group_by": "category",
  "items": [{"value": "code_scan", "count": 3}]
}
```

空/null group 被排除；按 count 降序、value 字符串升序排列。

### 6.8 `obs.sessions.list`

参数：time range 和 pagination。

```json
{
  "items": [
    {
      "session_id": "session-1",
      "first_seen_epoch": 1.0,
      "last_seen_epoch": 2.0,
      "turn_count": 1,
      "observability_event_count": 2,
      "security_event_count": 1
    }
  ],
  "total": 1,
  "limit": 100,
  "offset": 0,
  "next_offset": null
}
```

### 6.9 `obs.runs.list`

参数：必填非空 `session_id`、time range 和 pagination。

每项字段：`run_id`、`started_at_epoch`、`ended_at_epoch`、`user_input_preview`、
`observability_event_count`、`security_event_count`。顶层还包含请求的 `session_id`、
`total/limit/offset/next_offset`。

### 6.10 `obs.timeline.get`

参数：必填非空 `session_id`、`run_id`，time range，pagination，以及
`include_security` boolean（默认 true）。

返回顶层：`session_id`、`run_id`、`limit`、`offset`、`items`；当前不返回 total 或
next offset。items 按 `(timestamp_epoch, kind)` 升序排列：

- `kind=observability`：包含 `id/hook/timestamp/timestamp_epoch`、correlation IDs、
  `metadata` 和 `metrics`；无效或非 object JSON metadata/metrics 归一为 `{}`。
- `kind=security`：包含完整 security event、被关联的 observability context，以及
  `match.reason/rank/time_delta_seconds`。

### 6.11 **[TARGET V2]** PAP administration

PAP administration 是新增的 V2 method family，不是九个 V1 method 之一。第一版接口采用
互斥的 `{requestId,result}` 或 `{requestId,error}` 响应，不保留 POC 的 `poc.*` method、
`ok/data/stdout/stderr/exit_code` envelope 或兼容分支。输入和输出以
`v2/crates/daemon/asc-daemon-protocol/tests/fixtures/pap-methods.json` 为可执行清单。
`requestId` 是 daemon 为每次 dispatch 生成的 UUID；`error` 固定为 `{code,message}`，
success 不得再包一层 `{policy}`、`{scope}` 或 `{binding}`。

`v2/crates/daemon/asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json` 进一步冻结覆盖
15 个 method 的有状态 CRUD 场景及完整 response value，包括 Canonical Policy IR、Scope
template、Binding 内嵌快照、revision、status 和确定性 digest。daemon 生成的 request/resource
UUID 使用具名占位符：fixture 不冻结随机值本身，但必须验证 UUID 格式、CREATE 捕获值在后续
请求/响应中的一致性，以及不同资源 identity 不混用。该 fixture 同时由 protocol 类型测试、
使用服务端授权测试 Principal 的必跑 UDS integration E2E，以及真实 `asc-daemon` 子进程
bootstrap E2E 消费。binary 测试通过服务端启动配置 `--policy-admin-uid <测试 UID>`
执行完整成功场景，无需 root；另行验证非 root 默认 `permission_denied`。root 环境同时
验证默认授权成功路径，不使用跳过授权的测试开关。
`v2/crates/daemon/asc-daemon-protocol/tests/fixtures/pap-invalid-requests.json` 冻结 method
params 构造失败时的 `invalid_request` code 和有界、安全 message，并由真实 UDS integration
fixture 消费。

所有方法都要求 daemon 根据 kernel peer credentials 和服务端策略构造
Policy Administrator Principal；request 中不得携带可信 UID、role 或 scope。RPC 直接复用
`asc-policy-types` 的 `PolicyTemplate`、`ScopeSelector`、`PreparedPolicy`、
`PreparedScope`、`BindingView` 以及 foundation 的 `ResourceId`、`Revision`，不得复制
Policy domain DTO。

当前 bootstrap 始终允许 UID 0 管理 Policy，其它 UID 默认拒绝。部署者可在 daemon
启动时用可重复的 `--policy-admin-uid <UID>` 配置额外管理员，值为十进制 u32；授权仍
依据内核 peer UID，不接受请求中的 UID/role。控制启动配置属于服务端部署权限；生产
服务的启动参数由部署者管理。这不会改变 system-level 部署形态或提升进程 OS 权限，
也不会修改 socket 文件访问权限。root 仍可通过内部 `allow_uid` API 动态委派，但被授权
管理员不能继续委派。名单仅存放在进程内存，每次启动需重新提供配置；不带该参数重启
恢复 root-only。配置文件加载、持久化和管理 RPC 不在本 slice。

| method | params | result |
| --- | --- | --- |
| `policy.templates.create` | `{policyName, template}` | `PreparedPolicy` |
| `policy.templates.update` | `{policyId, policyName, template}` | `PreparedPolicy` |
| `policy.templates.get` | `{id, revision}` | `PreparedPolicy` |
| `policy.templates.list` | `{limit=100, offset=0}` | `{items: PreparedPolicy[], total}` |
| `policy.templates.delete` | `{id, revision}` | `PreparedPolicy` |
| `policy.scopes.create` | `{selector}` | `PreparedScope` |
| `policy.scopes.update` | `{scopeId, selector}` | `PreparedScope` |
| `policy.scopes.get` | `{id, revision}` | `PreparedScope` |
| `policy.scopes.list` | `{limit=100, offset=0}` | `{items: PreparedScope[], total}` |
| `policy.scopes.delete` | `{id, revision}` | `PreparedScope` |
| `policy.bindings.create` | `{policyId, policyRevision, scopeId, scopeRevision}` | `BindingView` |
| `policy.bindings.update` | `{bindingId, policyId, policyRevision, scopeId, scopeRevision}` | `BindingView` |
| `policy.bindings.get` | `{id}` | `BindingView` |
| `policy.bindings.list` | `{limit=100, offset=0}` | `{items: BindingView[], total}` |
| `policy.bindings.delete` | `{id}` | `BindingView` |

CREATE 的 stable identity 由 PAP 生成；UPDATE 不兼作 upsert。Policy/Scope GET 和 DELETE
要求精确 current revision；Binding GET 读取唯一 current record。Binding CREATE 和新
Apply/Delete 意图返回对应 pending 状态；幂等请求返回已有状态（如 READY、APPLYING、
DELETING）。daemon handler 不等待 Reconciler，不把 acceptance 表述为目标完成。

[TARGET V2] Binding 入队被拒且条件失败写入成功时，命令返回错误：队列满为
`resource_exhausted`，提交后队列停止为 `unavailable`。message 包含已保存的 Binding
ID/revision 及具体原因。Failed 写入未确认时返回 `internal`，明确后台仍可能执行；
worker 已抢先认领则返回当前状态，不覆盖为 Failed。BindingView 的 status 从字符串改为
`{phase, error?: {kind, code}}`；无错误时省略 error，显式重试及 worker 认领时清除旧错误。
Repository 不保存尝试次数、重试时间或策略；它们仅为进程内调度信息，重启不继承。调度原因码为
`RECONCILE_QUEUE_FULL`/`RECONCILE_QUEUE_STOPPED`，kind 为 `REJECTED`。
这是未发布 V2 契约修订，不改 V1；详见
[调度拒绝验收及并发边界](BINDING_QUEUE_ADMISSION_ACCEPTANCE_zh.md)。

[TARGET V2] `bindingRevision` 仅在 spec 改变时递增；同 spec 的 ApplyFailed 重试、
Delete 和 DeleteFailed 重试均保留 revision。Delete 保留完整 spec 与既有部署记录，
允许在 Applying 时受理；PendingDelete/Deleting/DeleteFailed 拒绝所有 UPDATE，不能
撤销删除。全部目标确认 Absent 后，Reconciler 原子移除 Binding 及运行记录；此后
GET/UPDATE/DELETE 返回 not_found，LIST 不包含该 ID。重新部署须 CREATE 新 ID、revision 1。
`Deleted` 只作内部完成标记，不作为持久化 current record。当前 daemon 尚未接入后台
reconcile worker；硬删除行为由 PAP + Reconciler 内存组合测试验证。

Policy CREATE/UPDATE 在 PAP 内同步调用 `PolicyCompiler::lower(TemplateEnvelope) ->
PolicyEnvelope`。当前产品 compiler 只实现 `prevent_file_deletion`，其输入与完整 Canonical
Policy IR 输出由
`v2/crates/policy/asc-policy-engine/tests/fixtures/compiler-contract.json` 冻结；输出语义是
`ResourceOperation::Delete + FileResolution::PathEntry`。该模板只覆盖对匹配目录项的删除操作，
例如 unlink/rmdir；rename/move、link、truncate、内容修改和其它 namespace mutation 不在其
保护范围内。其它 `PolicyTemplate` kind 在各自 lowering 与直接 Adapter conformance 完成前
返回 `invalid_argument`，不得生成占位 IR。

参数 object 拒绝未知字段。ScopeSelector 只包含正数 PID 或 cgroup ID；PreparedScope
必须显式包含 selector，缺失或 null 均拒绝，不再从 scopeId 推导执行域身份。
PreparedScope 的输出仅含 `scopeId`、`revision`、`selector`；不再包含自动填充的
`template` 或 `templateDigest`。读取时显式携带这两个已移除字段会被拒绝，不能
把其中的 lifetime 等约束静默丢弃。Scope Update 直接按 selector 比较内容是否相同；
PreparedPolicy 保留 policyId/policyName/revision/template/canonicalPolicy，移除
templateDigest；Policy 的 authored template 保留，canonicalPolicy 不再含预留的 payloadDigest。
PreparedBinding 只含 bindingId/bindingRevision/policy/scope，不再接受顶层
executionDomainId。已删除的 Policy templateDigest 和 Binding executionDomainId
（包括显式 null）均按未知字段拒绝；Policy/Scope 的 retired 和 canonicalPolicy 的
payloadDigest 也不再接受。当前三个模型直接派生严格反序列化，不保留旧字段吞入逻辑。
LIST 的 `limit` 为 `1..=1000`，`offset` 为 `u32`；total 是
分页前总数。当前 aggregate byte budget 仍是 Repository/PAP/transport 联合 gate，在该 gate
完成前 LIST 只达到 integration contract，不构成 distribution-ready 大数据量查询能力。

TODO(policy-response-bounds)：当前 direct `PreparedPolicy`/`BindingView` mutation result 可能在
Repository commit 后才因 response frame 超限而失败；Binding GET/LIST 也会因内嵌完整 Policy
和 Scope 快照放大响应。在 durable Repository 或 distribution gate 前，必须完成 public result
shape/存储模型收敛，并对 mutation、单记录 GET 和 LIST aggregate response 建立 server-owned
encoded-size gate；本 slice 不以提高 transport limit 代替该修复。

PAP error 稳定投影如下：无法构造成 method params 的字段、类型或 bounded value（包括非法
identifier、revision、pagination 和 authored selector）→ `invalid_request`，同时返回最多 256
字节的参数解码原因；任意层级 JSON object 的 duplicate key 在进入 `serde_json::Value` 前拒绝，
按 malformed envelope 返回 `invalid_request / request envelope is invalid`。成功构造 params 后
发生的 authoring/compiler validation → `invalid_argument`，message 为最多 256 字节的稳定
`invalid policy name: <reason>`、`invalid policy: <authored-path>: <reason>` 或
`invalid scope: <authored-path>: <reason>`，不得暴露 canonical IR path、输入内容或内部 error
code。

not found → `not_found`，并按操作对象稳定区分 `policy was not found`、
`policy revision was not found`、`scope was not found`、`scope revision was not found`、
`binding was not found`、`referenced policy revision was not found` 和
`referenced scope revision was not found`。revision conflict 使用
`conflict / policy request conflicts with current state`，operation in progress 使用
`conflict / binding reconciliation operation is in progress`；revision exhaustion →
`resource_exhausted`；serialization/persistence、内部 identifier/Binding 构造失败 → `internal`。
Malformed envelope 使用 `invalid_request`，未注册 method 使用 `unknown_method`，授权失败使用
`permission_denied`。repository error、secret、内部 validation path 和内部 error code 不进入
wire message；所有 PAP 公开 error message 均不得超过 256 字节。

<a id="daemon-security-action-handler-contract"></a>

### 6.12 **[TARGET V2][PENDING DEFINITION]** Security action handler 扩展

#### 6.12.1 历史依据与注册模型

commit `ef0d75f27c389434cf6f4361f5dbcdeaff42ab72` 曾实现完整的 `scan-prompt`
daemon action 路径：`register_prompt_scan_methods()` 在默认 `MethodRegistry` 中注册
`MethodSpec`，handler 将 daemon trace context 传入 security middleware，在阻塞执行边界
外使用 thread offload，并把 `ActionResult` 映射为 `HandlerResult`。该提交中的 Python
模型 preload 和 scanner 实现已经退役，但 handler 注册与三层响应模型是本扩展的设计
依据。

迁移后的 action 仍采用**显式 handler allowlist**，不得开放一个接受任意 action 名称并
动态 import/dispatch 的通用 wire method。目标 canonical method 与 core action 的映射为：

| daemon method | core action |
| --- | --- |
| `action.sandbox_prehook` | `sandbox_prehook` |
| `action.harden` | `harden` |
| `action.verify` | `verify` |
| `action.summary` | `summary` |
| `action.code_scan` | `code_scan` |
| `action.prompt_scan` | `prompt_scan` |
| `action.pii_scan` | `pii_scan` |
| `action.skill_ledger` | `skill_ledger` |

这 8 个 method 是对当前 9 个 method 的候选向后兼容扩展。它们符合“所有入口进入
daemon-core、显式 allowlisted RPC”的 V2 方向，但 17 个 canonical method 只有在
asc-daemon-protocol 的 Definition Review 冻结 method 名、schema、authorization、timeout
和 compatibility version 后才能成为正式 V2 contract。

每个 method 必须由 `daemon/handlers/` 下的注册函数加入默认 registry，并明确自己的
`lifecycle="security action"`、queue/resource class、默认 timeout 和 access-log 策略。
历史 `scan-prompt` 名称不会因本扩展自动恢复；如发布兼容性证据证明仍有调用方，必须把它
作为显式 alias 注册，并与 `action.prompt_scan` 使用同一 handler 和 contract fixture。
该 alias 不计入 17 个 canonical method；是否启用必须在发布前形成明确决定，不能由实现
自行推断。

#### 6.12.2 Request 映射

action request 继续使用 V1 顶层 envelope：

- `method` 决定唯一的 core action，`params` 中不得再次接受调用方提供的 `action`；
- `params` 直接使用
  [`SECURITY_ACTIONS_REFERENCE_zh.md`](SECURITY_ACTIONS_REFERENCE_zh.md) 对应 action 的
  params schema；
- `caller` 和规范化后的 `trace_context` 进入 `ActionContext`，不得混入业务 params；
- handler 只调用一次 asc-daemon-core action use case，由 asc-action-runtime 选择唯一
  CapabilityExecutor；daemon adapter 不重复执行 lifecycle、event projection 或 redaction；
- 同步文件、SQLite、模型、CPU 或 subprocess 操作不得阻塞 socket runtime 的 accept/
  timeout loop；Python 参考实现使用 thread offload，Rust 使用等价 blocking boundary。

##### 6.12.2.1 参数错误边界

handler 与 core 的校验职责必须保持以下三层边界：

1. 顶层 envelope、JSON 类型、`method/params/trace_context` 形状或 method-specific wire
   shape 不合法：daemon 返回 `ok=false, error.code=bad_request`，不调用 core；
2. request wire shape 合法，但 action 领域输入不合法，例如空 prompt、非法 scan mode、
   缺少 Skill Ledger command：handler 必须调用 core，由 core 返回失败 `ActionResult`，
   daemon 投影为 `ok=true` 和非零 `exit_code`；
3. adapter 只执行 action contract 明确规定的默认值和 normalization，不得复制一套会改变
   failure layer 的领域校验。

同一 invalid-input fixture 必须同时覆盖 V1 oracle 和 V2 daemon compatibility adapter，确保
错误不会因语言或入口实现不同而在 `bad_request` 与失败 `ActionResult` 之间漂移。

旧 daemon 对 `action.*` 返回 `unknown_method`，证明该 action 没有被接受执行；V2 agent-sec-cli
必须报告稳定的 version/capability mismatch，不转入 PyO3 或本地业务路径。request 已发送
后的 timeout、EOF 或协议错误仍然状态不明，不能由 client 重放。

#### 6.12.3 三层结果

沿用历史 `scan-prompt` 协议的三层解释，并推广到全部 action：

1. transport/protocol failure：没有合法 `DaemonResponse`，也没有 `ActionResult`；
2. daemon failure：`ok=false`，`error` 是 daemon error，`data` 不是 action result；
3. action result：handler 已返回，`ok=true`；即使 `ActionResult.success=false` 或
   `exit_code!=0`，也不能改写成 daemon failure。

action response 沿用历史 V1 projection，不新增 action-specific 顶层 envelope 字段：

| daemon response | ActionResult 来源 |
| --- | --- |
| `data` | `data` |
| `stdout` | `stdout` |
| `stderr` | `error` |
| `exit_code` | `exit_code` |

```json
{
  "request_id": "daemon-generated-uuid",
  "ok": true,
  "data": {"ok": false, "verdict": "error"},
  "stdout": "{...}",
  "stderr": "scanner unavailable",
  "exit_code": 1
}
```

`ActionResult.success` 和 `ActionResult.error_type` 是 core/middleware contract，不是历史
V1 daemon wire 字段。daemon handler、lifecycle 和本地 diagnostic 可以使用它们，但 V1
client 不得要求从 daemon response 无损重建内部 `ActionResult`。scanner/领域错误应优先在
method-specific `data` 中返回结构化 error/verdict；没有结构化 data 的 validation/product
failure 使用 `ok=true`、非零 `exit_code` 和 `stderr`。如果未来需要把稳定 product error
type 暴露给 wire client，必须作为单独的版本化协议扩展，而不是向本兼容 projection 临时
增加顶层字段。

#### 6.12.4 **[TARGET V2]** system daemon 安全边界与 method metadata gate

V2 是 one daemon per host 的 system-level service，并服务多个本地 UID/Agent。daemon 从
内核 peer UID/GID/PID、token binding 和服务端配置构造 trusted Principal；客户端提供的
`caller`、UID、role、scope 和 trace context 均不能成为授权依据。method authorization 和
QueryScope 由服务端生成，normal principal 只能访问 owner scope，auditor/admin 跨 UID
访问需要显式授权。system-level 不等于必须以 root 运行，也不得产生未声明的 privilege
uplift。

在开始 Rust daemon action handler 实现前，contract fixture 必须为每个 canonical method
冻结以下 `MethodSpec` 行为：`lifecycle`、`queue/resource class`、默认 `timeout_ms`、
`access_log`、blocking boundary 和 cancellation policy。历史 `scan-prompt` 的 30 秒 timeout
只证明旧方法的行为，不能自动成为全部 action 的默认值。未完成该 fixture 时，action
handler target 视为尚未达到 contract freeze，不得由实现自行选择默认值。

## 7. Canonical SecurityEvent envelope

query method 返回的 event 具有以下稳定字段：

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
  "details": {}
}
```

`result` 值域仅为 `succeeded/failed`。安全 verdict 是 details/result 中的领域字段，
不得和 event `result` 混为一谈。

## 8. SkillFS notify authentication extension

### 8.1 启用条件

设置 `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` 后启用。auth version 为 string `"1"`，
auth JSON content 最大 4096 bytes，握手总 deadline 最大 5 秒并受 request read timeout
更小值限制。nonce 和 HMAC-SHA256 proof 均为 32 bytes，以 canonical Base64 表示。

### 8.2 握手和业务帧

```text
client -> {"authVersion":"1","type":"auth.init"}\n
server -> {"authVersion":"1","type":"auth.challenge","nonce":"..."}\n
client -> {"authVersion":"1","type":"auth.proof","proof":"..."}\n
server -> {"authVersion":"1","type":"auth.ok","proof":"..."}\n
client -> <request-json-without-LF>\n
client -> {"authVersion":"1","type":"auth.frame","proof":"..."}\n
server -> <response-json-without-LF>\n
server -> {"authVersion":"1","type":"auth.frame","proof":"..."}\n
```

握手 proof 使用 domain-separated HMAC：

- client：`anolisa.skillfs.notify.client.v1`；
- server：`anolisa.skillfs.notify.server.v1`。

业务 frame proof 还绑定方向、session nonce、payload 长度和原始 payload bytes。
认证必须在 JSON request parse 和 dispatch 前完成。

auth frame 必须字段精确、拒绝 duplicate field、拒绝非 canonical Base64。任何包含 auth
特征但不是合法 `auth.init` 的第一帧都必须 fail closed。auth 失败、timeout、EOF、
tamper、oversize 时静默关闭，不返回普通 error response。

### 8.3 与普通协议共存

- 配置 key 后，普通未认证 `daemon.health` 和 query 继续可用。
- 配置 key 后，普通未认证 notify 被静默关闭。
- authenticated session 只允许 notify。
- authenticated notify 的业务 `bad_request`、成功 response 都必须带 server frame proof。

## 9. Client failure分类

### 9.1 **[CURRENT]** Python client 的确定行为

当前同步 client 每次调用新建一个 Unix stream connection，发送一个 LF 结尾 request，并
读取到第一处 LF 或 EOF；只解析第一条 response。没有任何 response bytes 是 transport
failure，超过 4 MiB 或 envelope 解析失败是 protocol failure。缺失有效 `trace_id/traceId`
时，client 使用当前 invocation ID 补充 `trace_id`。

当前 socket timeout 被分别设置到 connect/send/recv 等阻塞操作上，不是覆盖整个调用的
单一 monotonic deadline；读取循环中的每次 `recv` 都可能重新消耗该 timeout。该细节在
Python 中是明确的，但此前没有被契约冻结，也不属于必须复制的兼容语义。**[TARGET V2]** Rust
client 使用一次调用级 deadline，并继续把 timeout 后“请求是否已执行”视为未知。

`daemon_health_reachable()` 当前默认使用 250 ms，只有收到可解析且 `ok=true` 的
`daemon.health` 才返回 true。该 probe 用于区分 live daemon 与 stale socket；它不是完整
Job/backend readiness，也不读取 ambient process trace context。

client 读取第一条 response 后忽略同一次 read 已取得的 trailing bytes。服务端本来就只应
返回一条业务 response，因此 chunk size、recv 次数和 trailing-byte 容忍属于 **[CURRENT]**
实现细节；跨语言 fixture 只冻结“一连接一请求一响应”、上限和失败分类。

### 9.2 **[CURRENT][PRESERVE V1]** 失败分类

以下情况没有合法 `DaemonResponse`，client 必须作为 transport/protocol failure 处理：

- socket 不存在、connect/read/write 失败；
- client-side timeout；
- EOF 前没有任何 response bytes；
- response 超过 client byte limit；
- response 不是合法 UTF-8 JSON object 或 envelope 字段类型错误；
- auth 模式下握手或 frame authentication 失败。

这类失败不证明 daemon 未执行请求。V2 没有通用本地 fallback；request 是否发送都不得由
agent-sec-cli 触发 PyO3、Python backend 或第二套本地业务执行。

## 10. 兼容性规则

1. V1 调用方必须忽略 response 未知字段。
2. V1 服务端可以忽略 request 未知顶层字段，但 method params 按本文件处理。
3. 新增 optional response 字段是向后兼容变更；改变已有字段类型、意义或默认值不是。
4. 新增 method 不得改变现有 method；删除/重命名 method 需要协议 major 变更或明确
   deprecation window。
5. 新增错误 code 需要同步 client error/version policy；不得把已有错误换码来规避兼容测试。
6. method 的非确定字段可以在 golden test 中规范化，但 schema 和副作用不能。
7. Python 与 Rust serializer 的 JSON key 顺序和空白不要求字节相同；解析后的 JSON
   value 必须等价。认证 proof 比较除外，它绑定发送时的原始 payload bytes。

## 11. V1/Rust conformance

### 11.1 **[CURRENT][PRESERVE V1]** V1 基线

共享 fixture 至少覆盖：

| ID | Fixture |
| --- | --- |
| DPV1-001 | partial frame、LF frame、EOF frame、coalesced frames |
| DPV1-002 | malformed UTF-8/JSON、非 object、空 method、错误 params/context 类型 |
| DPV1-003 | caller trim、caller invalid、调用方 ID 被忽略、daemon UUID |
| DPV1-004 | timeout 0/bool/负数/最大值/超最大值 |
| DPV1-005 | request/response 正好上限和超过上限 |
| DPV1-006 | 8 个稳定 daemon error code 及 envelope |
| DPV1-007 | 当前 9 个 method 的成功、空结果、分页、过滤和参数错误；新增 method 后这些行为仍不变 |
| DPV1-008 | snake/camel trace alias、256 字符截断、空 trace |
| DPV1-009 | notify schema v2 全字段、未知/缺失字段、path traversal |
| DPV1-010 | auth happy path、wrong key、tamper、slowloris、EOF、oversize、method 限制 |

### 11.2 **[TARGET V2]** Rust 与 action handler 验收

| ID | Fixture |
| --- | --- |
| DPV1-011 | V1 Python client/daemon oracle 与 Rust client/daemon compatibility adapter 交叉矩阵；Python 仅为测试 oracle |
| DPV1-012 | 任意 daemon failure 都不触发 PyO3、Python backend 或隐式本地重放 |
| DPV1-013 | 当前 9 个 method 完成兼容分类；八个 `action.*` method 经 Definition Review 后固定 method/action 与 params contract |
| DPV1-014 | `ActionResult.data/stdout/error/exit_code` 分别投影为 V1 `data/stdout/stderr/exit_code`，并保持三层 response 分离 |
| DPV1-015 | 旧 daemon 返回 `unknown_method` 时返回稳定 version/capability mismatch；timeout/EOF 时同样不本地执行 |
| DPV1-016 | envelope/wire-shape 错误与 action 领域输入错误稳定落入不同 response layer |
| DPV1-017 | 八个 action method 的 timeout、queue/resource、access-log、blocking 和 cancellation metadata 已冻结并逐项验证 |
| DPV1-018 | 多 UID 共用 system socket；trusted Principal/QueryScope 隔离 owner，`caller/trace_context` 不参与授权 |
| DPV1-019 | CLI/TUI 不能用 RPC filter 绕过服务端 QueryScope，也不能直读 SQLite 替代授权查询 |
| DPV1-020 | 15 个 PAP method 的 strict params、完整请求/响应 CRUD fixture、直接领域 result、错误投影、server-owned Principal；必跑 UDS integration 经 Dispatcher/PapHandler → PapService → Policy Compiler/Repository 执行完整 fixture，真实 `asc-daemon` 子进程通过启动管理员 UID 配置完成非 root 成功场景，同时验证默认拒绝；root 环境验证默认成功 |

协议测试必须使用 socket bytes 和解析后 JSON 比较；只测试某个 Python dataclass 或 Rust
struct 的构造函数不足以证明 wire compatibility。

## 12. 当前实现证据

- framing/envelope：`agent-sec-cli/src/agent_sec_cli/daemon/protocol.py`。
- client：`agent-sec-cli/src/agent_sec_cli/daemon/client.py`。
- server transport/auth：`agent-sec-cli/src/agent_sec_cli/daemon/server.py`。
- stable errors：`agent-sec-cli/src/agent_sec_cli/daemon/errors.py`。
- method registry：`daemon/server.py:create_default_registry`。
- method handlers：`daemon/health.py`、`daemon/handlers/security_query.py`、
  `daemon/handlers/skill_ledger.py`。
- tests：`tests/unit-test/daemon/test_protocol.py`、`test_client_server.py`、
  `test_security_query_handler.py`、`test_skill_ledger_handler.py` 和
  `tests/e2e/daemon/test_daemon_e2e.py`。

**[HISTORICAL] TARGET handler 的设计证据：** commit
[`ef0d75f27c389434cf6f4361f5dbcdeaff42ab72`](https://github.com/alibaba/anolisa/commit/ef0d75f27c389434cf6f4361f5dbcdeaff42ab72)
中的 `daemon/handlers/prompt_scan.py`、`prompt_scan_protocol.md`、
`test_prompt_scan_handler.py` 和 CLI daemon call path。

## 13. [TARGET V2] SkillGuard 第一阶段批准替代

本节将 §6.12 中 `action.skill_ledger` 候选明确替换为 `action.skill_guard`，采用已落地 V2
`{method,params}` 请求及 `{requestId,result|error}` envelope。普通 V1 RPC 不兼容；SkillFS
HMAC/notify 单独在适配层保留，不扩大通用解析器接受字段。

`params.command` 为封闭枚举：init/check/analyze/scan/certify/status/audit/list-scanners/
decide/show/export/activate/rotate-keys。未知 command、未知字段和伪造 uid/ioDir 拒绝解析。
每个方法按内核 peer credentials 鉴别本地用户；换钥和 forceKeys 进一步要求 UID 0，PAP 的
policy-admin-uid 不赋予换钥权限。正常用户可操作全部受管 Skill，用户隔离留在领域配置 TODO。

结果字段为 `success`、`exitCode`、`error`、`errorType`、`data`。方法解析失败仍为 daemon error；
执行失败为正常 envelope 内的失败业务结果；风险判定不必带执行错误。CLI 只投影 data 与退出码。
`timeoutMs` 为 1–120000，缺省 60000；只改变 SkillGuard 的 dispatch 预算，其他方法不变。
超时之后不得重放写请求，应通过 show/check 查明状态。大于 3 MiB 的业务响应改为明确
ResponseTooLarge，注明 operationMayHaveCommitted；完整版本可用 export 读取。

可执行合同：`v2/fixtures/skillguard/consumer.json`、`v2/apps/asc-cli/tests/skill_guard.rs`、
`v2/crates/daemon/asc-daemon-handler/src/skill_guard.rs` 测试。样例覆盖正常、风险、未初始化、超时、
执行失败与激活未完成，不能作为 Agent Hook 或真实 FUSE 联调证据。
