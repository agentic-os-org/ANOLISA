# AgentSec daemon 协议 V1

> PII 第一阶段 V2 已实现扩展见本文末尾专节；其版本化差异不修改 V1 oracle 基线。

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

以上为冻结的 Python V1 上限；Rust 原生 PAP 的请求分项预算与错误类别见 §13。

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

#### V2 UID 范围与 session 定位

四个 `sec.*` method 的读取范围由 transport 认证的 peer UID 决定（DPV1-018/019）：

- root peer 读取全部 UID；非 root peer（包括 `PolicyAdministrator`）只能读取自身 UID。
- 跨 UID 的 `session_id` 碰撞时，root 的事件、latest_events 和 session 分组返回
  `UID_session_id`；唯一 session 和普通用户返回原始 ID。碰撞判断覆盖完整 store，
  不受当前分页、时间或其它筛选影响；`affected_sessions` 分别统计各 UID 的同名 session。
- `session_id` 筛选对所有调用者按原始存储 ID 匹配；root 返回所有 UID 下的匹配事件。
  返回中的 `UID_session_id` 仅用于区分同名会话；查询不解析该展示前缀。
  原始 ID 本身带数字前缀时，同样按字面值匹配。

存储存在但不可读（页损坏、I/O 错误、查询被中断）时，四个 method 返回 `unavailable`
错误而不是成功的空结果；数据库尚未创建时仍返回空结果（首次写入前的正常状态）。
SQL 执行受 5 秒内核级执行预算约束（progress handler 中断），`count_by` 收集的分组
桶数上限为 100000。

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

`total` 是应用 filter 后、分页前整个 scope 的行数（不折叠 offset）；`next_offset`
是绝对偏移，仅当 `offset + 本页返回行数 < total` 时存在，末页为 `null`。`limit` 为
`1..=1000`；`offset` 为非负 64-bit integer（上限 9223372036854775807，与 V1 校验
一致）。

### 6.6 `sec.events.get`

参数：必填非空 string `event_id`。

```json
{"found": false, "event": null}
```

找到时 `event` 包含完整 `details` 和 dashboard projection 字段。

### 6.7 `sec.events.count_by`

参数：必填 `group_by`、security-event filters、可选 `offset`（默认 0，先在 SQL 内跳过
最新 N 行再分组——与 v1 直读 SQLite 时的单条 GROUP BY 语义一致，分组计数来自同一次
一致快照，避免客户端 OFFSET 分页在并发写入下重复计数）。`limit` 明确禁止
（分组聚合不分页）。

`group_by` 值域恰好为：

`category`、`event_type`、`result`、`trace_id`、`session_id`、`run_id`、`call_id`、
`tool_call_id`、`verdict`。

```json
{
  "group_by": "category",
  "items": [{"value": "code_scan", "count": 3}]
}
```

SQL `NULL` 桶以 `value: null` 交付（客户端按 v1 的 SQL 拼写渲染为 `"null"` 对象键）；
空字符串 group 被排除；按 count 降序、value 字符串升序排列。

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

本节 15-method CRUD 表描述现有 V2 实现基线。新的对象目标以
[生命周期契约](POLICY_SCOPE_BINDING_CONTRACT_zh.md)和第 14 节为准：保留 Policy update，
Scope 无自身 revision 且不可更新；Binding 仅供用户查询。实施前不把旧方法表当作新目标，
也不把目标文档变化解释为当前二进制已经移除相应方法。

PAP administration 是新增的 V2 method family，不是九个 V1 method 之一。第一版接口采用
互斥的 `{requestId,result}` 或 `{requestId,error}` 响应，不保留 POC 的 `poc.*` method、
`ok/data/stdout/stderr/exit_code` envelope 或兼容分支。输入和输出以
`v2/crates/asc-daemon-protocol/tests/fixtures/pap-methods.json` 为可执行清单。
`requestId` 是 daemon 为每次 dispatch 生成的 UUID；`error` 固定为 `{code,message}`，
success 不得再包一层 `{policy}`、`{scope}` 或 `{binding}`。

`v2/crates/asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json` 进一步冻结覆盖
15 个 method 的历史有状态 CRUD 场景及完整 response value、Scope
template、Binding 内嵌快照、revision、status 和确定性 digest。daemon 生成的 request/resource
UUID 使用具名占位符：fixture 不冻结随机值本身，但必须验证 UUID 格式、CREATE 捕获值在后续
请求/响应中的一致性，以及不同资源 identity 不混用。该 fixture 同时由 protocol 类型测试、
使用服务端授权测试 Principal 的必跑 UDS integration E2E，以及真实 `asc-daemon` 子进程
bootstrap E2E 消费。binary 测试通过服务端启动配置 `--policy-admin-uid <测试 UID>`
执行完整成功场景，无需 root；另行验证非 root 默认 `permission_denied`。root 环境同时
验证默认授权成功路径，不使用跳过授权的测试开关。
`v2/crates/asc-daemon-protocol/tests/fixtures/pap-invalid-requests.json` 冻结 method
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
`Deleted` 只作内部完成标记，不作为持久化 current record。现有 daemon 已接入后台
reconcile Runtime。当前 Scope 自动管理 Binding，生产使用 SQLite，最后一条 Binding 与
已停止 discovery 的 Deleting Scope 同事务删除；恢复证据见
[Policy SQLite 持久化设计](POLICY_SQLITE_PERSISTENCE_DESIGN_zh.md)。内部 status_version、
deployments 与写回执不进入公共响应。历史内存组合测试不作为真实 PEP 或物理断电验收。

Policy CREATE/UPDATE 在 PAP 内校验 `PolicyTemplate` 并保存完整模板。
PolicyTemplate 是可被多个 Scope 复用的策略；内容使用 `specVersion`、可选 description 和
非空 rules，每条规则包含 effect/category/action/typed target 及可选 where/previous/because。
当前定义文件目标和 read/write/exec 动作，合法但 AgentSight 不支持的规则可以保存。
Adapter 只转换不带历史条件的 block + file/write + operation=delete 规则；任意一条不支持，
整个 Binding 失败，错误码包含规则序号，不部分下发。旧 kind/files 格式拒绝。
完整结构与校验边界见 [对象契约](POLICY_SCOPE_BINDING_CONTRACT_zh.md#7-通用规则结构改造计划)；
固定输出见 `v2/fixtures/adapters/agentsight/prevent-file-deletion/`。

参数 object 拒绝未知字段。基线 ScopeSelector 支持正数 PID 或 cgroup ID；第 14 节
SCOPE-CR-001 扩展 name/path process selector 与 policyTemplates assignment。PreparedScope
必须显式包含 selector，缺失或 null 均拒绝，不再从 scopeId 推导执行域身份。
Legacy PreparedScope 的输出含 `scopeId`、`revision`、`selector`，assignment 增加 `policyTemplates`；不再包含自动填充的
`template` 或 `templateDigest`。读取时显式携带这两个已移除字段会被拒绝，不能
把其中的 lifetime 等约束静默丢弃。Scope Update 直接按 selector 比较内容是否相同；
PreparedPolicy 包含 policyId/policyName/revision/template。Scope 和 Binding 中的内嵌
Policy 使用相同结构。
PreparedBinding 只含 bindingId/bindingRevision/policy/scope，不再接受顶层
executionDomainId。已删除的 Policy templateDigest 和 Binding executionDomainId
（包括显式 null）均按未知字段拒绝；Policy/Scope 的 retired 也不再接受。当前三个模型直接派生严格反序列化，不保留旧字段吞入逻辑。
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
发生的 template/selector validation → `invalid_argument`，message 为最多 256 字节的稳定
`invalid policy name: <reason>`、`invalid policy: <authored-path>: <reason>` 或
`invalid scope: <authored-path>: <reason>`，不得暴露 内部字段路径、输入内容或内部 error
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
| DPV1-018 | 多 UID 共用 system socket；trusted Principal/QueryScope 按 UID 隔离，`caller/trace_context` 不参与授权；root 为 All，非 root（含 PolicyAdministrator）仅限自身；sec 按原始 session ID 查询，root 返回所有匹配 UID 的记录，返回标签区分同名 session |
| DPV1-019 | CLI/TUI 不能用 RPC filter 绕过服务端 QueryScope，也不能直读 SQLite 替代授权查询 |
| DPV1-020 | 15 个 PAP method 的 strict params、完整请求/响应 CRUD fixture、直接领域 result、错误投影、server-owned Principal；必跑 UDS integration 经 Dispatcher/PapHandler → PapService → Repository 执行完整 fixture，真实 `asc-daemon` 子进程通过启动管理员 UID 配置完成非 root 成功场景，同时验证默认拒绝；root 环境验证默认成功 |

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

## 13. **[TARGET V2]** 原生 PAP OTel carrier 扩展

本节只扩展 Rust 原生 PAP envelope，不改变前文冻结的 Python V1 `request_id/ok/data` 协议。
Rust 响应仍保留公开 `requestId` UUID 和既有 result/error 结构。

```json
{
  "method": "policy.templates.list",
  "params": {},
  "traceContext": {
    "version": 1,
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    "baggage": "agentsec.session.id=session-123"
  },
  "compatibility": {"version": 1, "traceId": "caller-label"}
}
```

- 两个扩展对象可省略/null。对象内 version 必须为整数 1；traceContext 的三个可选字段
  traceparent/tracestate/baggage 若出现必须是 string，不能是 null。compatibility 的可选
  traceId/invocationLabel 可省略/null，只作关联标签，不决定 SDK ID。
- 未知字段、重复 JSON key、错误类型/version 返回 `invalid_request`，不执行 PAP。
  W3C 内容错误独立降级：无效 parent 新建 root，保留有效 Agent Baggage；坏 tracestate 丢弃；
  坏 Baggage 整体丢弃。未知 Baggage key 不传播，其值不做 UTF-8 解码；
  member 结构/key/property/percent 语法仍须有效。允许 key 重复或值解码失败时整体丢弃。
- 仅允许 agentsec.agent.name、agentsec.session.id、agentsec.run.id、agentsec.call.id、
  agentsec.tool_call.id。字段上限 256 Unicode 字符；Baggage wire ≤16384 bytes/32 input members；
  traceparent/tracestate 各 ≤512 bytes。metadata 原值空白通过 percent encoding 保留；V1 trace-context 输入适配独立归一化。
- 所有正常 dispatch 从干净 Context 开始，没有 context 也生成 SDK root。归属字段不参与授权；
  Principal 仍来自 kernel peer 和服务端 policy。非法 envelope 不从坏 JSON 抢救 parent。
- LF-inclusive 请求业务预算 4 MiB，传播成员另有 32 KiB，总 wire 上限 4 MiB+32 KiB。
  按原始字节计量；超出业务/传播预算返回 `invalid_request`，超出 transport 总上限沿用
  `resource_exhausted`。原先超过 4 MiB 就被 transport 拒绝的帧，如今在总上限内
  若违反分项预算则返回 `invalid_request`，合法传播扩展则正常处理。响应仍为 4 MiB。caller 不会为了适配旧 server 自动去掉 context 重发。

支持范围为新 CLI 与支持 carrier 的新 daemon；旧 daemon 不属于兼容验收范围，
不增加 opt-in、版本协商或无 carrier 重试。部署时先升级 daemon，回滚时先回滚 caller。
正常 RPC 即使未传 tracing 参数也会注入 carrier；V1 `--trace-context` 与显式
`AGENT_SEC_INVOCATION_ID` 会影响 wire 归属/标签，保留输入语义不代表 wire 不变。
关闭 exporter 不关闭 carrier。
兼容记录 OTEL-CR-001/002/005/006/007、执行 fixtures、直接消费者和回滚见
[V2 OTel 验收](V2_OTEL_ACCEPTANCE_zh.md)。冻结 V1 运行时没有增加 OTel 依赖。

## PII 第一阶段 V2 协议扩展

**[TARGET V2，已实现]** 新增唯一方法 `action.pii_scan`，角色为 `LocalUser`，保持现有
V2 顶层 OTel 信封及第 13 节独立预算（请求业务 4 MiB + 传播 32 KiB，响应 4 MiB）。
本文原有 V1 默认 catalogue 不因此改变。

| `params` 字段 | 类型与默认值 |
|---------------|----------------|
| `text` | 必填 string，允许空文本 |
| `source` | string，默认 `unknown`；允许 user_input/model_input/tool_input/tool_output/model_output/observability/manual/unknown |
| `includeLowConfidence`、`rawEvidence`、`redactOutput` | bool，默认 false |
| `maxBytes` | 可选正整数或 null |
| `inputTruncated` | bool，默认 false |
| `inputBytesScanned` | 可选非负整数或 null；必须等于收到文本字节数，截断时可多出最多 3 个 UTF-8 尾字节 |

拒绝未知字段，包括未发布的 `params.traceContext`；不接受输入文件或规则路径，
也不接受调用者 UID/GID/PID。关联信息仅使用公共顶层 `traceContext` / `compatibility`。
识别方法并完成授权后，DTO/schema 错误返回 `invalid_request`，无效 source 或语义限制返回
`invalid_argument`；错误消息固定为 `PII scan parameters are invalid`，通过公共 Finalizer
记录安全终态。扫描结果（含执行失败报告）置于 daemon success 响应；CLI 对扫描失败退出 1。
未处理的执行 panic 由公共 Runtime 捕获并收尾，再返回 `internal` 和固定消息
`capability execution failed`，不把原始异常内容返回给调用者。

可执行契约为 `tests/v2/e2e/test_pii_cli_e2e.py`，实现 DTO 为
`v2/crates/asc-daemon-protocol/src/action.rs`。当前/未来链路、兼容性和回滚见
[PII 两阶段设计](PII_V2_MIGRATION_zh.md)。

## 14. [TARGET V2] SkillSec 第一阶段批准替代

本节将 §6.12 中 `action.skill_ledger` 候选明确替换为 `action.skill_sec`，采用已落地 V2
`{method,params}` 请求及 `{requestId,result|error}` envelope。普通 V1 RPC 不兼容；SkillFS
HMAC/notify 单独在适配层保留，不扩大通用解析器接受字段。

`params.command` 为封闭枚举：init/check/analyze/scan/certify/status/audit/list-scanners/
decide/show/export/activate/rotate-keys。未知 command、未知字段和伪造 uid/ioDir 拒绝解析。
每个方法按内核 peer credentials 鉴别本地用户；换钥和 forceKeys 进一步要求 UID 0，PAP 的
policy-admin-uid 不赋予换钥权限。正常用户可操作全部受管 Skill，用户隔离留在领域配置 TODO。

结果字段为 `success`、`exitCode`、`error`、`errorType`、`data`。方法解析失败仍为 daemon error；
执行失败为正常 envelope 内的失败业务结果；风险判定不必带执行错误。CLI 只投影 data 与退出码。
`timeoutMs` 为 1–120000，缺省 60000；只改变 SkillSec 的 dispatch 预算，其他方法不变。
超时之后不得重放写请求，应通过 show/check 查明状态。大于 3 MiB 的业务响应改为明确
ResponseTooLarge，注明 operationMayHaveCommitted；完整版本可用 export 读取。

可执行合同：`v2/fixtures/skillsec/consumer.json`、`v2/apps/asc-cli/tests/skill_sec.rs`、
`v2/crates/asc-daemon-handler/src/skill_sec.rs` 测试。样例覆盖正常、风险、未初始化、超时、
执行失败与激活未完成，不能作为 Agent Hook 或真实 FUSE 联调证据。

### SkillSec 第一阶段的 SkillFS 兼容实现

当前 Rust 适配层在 V2 公共 socket 上识别 `auth.init`，完成现有 SkillFS HMAC 握手后，只接收
notify schema version 2。请求保留 `id/method/params/trace_context/timeout_ms`，响应保留
`id/ok/data` 和 `accepted/queued/coalesced`；合并事件仍为 `queued=true`，保留 6.3 节
的 `skill/reason` 数据及 V1 `request_id/stdout/stderr/exit_code`、结构化错误字段。
`request_id` 由 daemon 生成，不信任客户端 `id` 作为服务端身份。认证失败关闭连接，认证后的业务拒绝也带有
server frame proof。普通 V2 请求仍由原 closed envelope 解析，不接受旧 RPC 顶层 `id`。

每个连接只接受一次 notify；会话入站上限四帧、单帧 64 KiB、首帧后的会话期限五秒。
实时 pending 上限为 256 个不同 canonical Skill，满队列返回签名业务拒绝；已确认接受不表示
持久化或已完成激活。完整配置、权限和恢复语义见
[SkillSec 第一阶段迁移](SKILL_SEC_PHASE_ONE_zh.md#第六批-skillfs-边界)。

## 15. [TARGET V2] 可观测单条采集

新增显式 allowlist 方法 `obs.record`，按 `LocalUser` 授权。业务参数仅含
`hook / observedAt / metrics`，关联 metadata 通过第 13 节 OTel carrier 的 Baggage 传递。
成功响应为 `{requestId,result:{}}`，且发生在 JSONL、SQLite 两次写入成功之后。
完整输入、错误、部分成功语义、V1 CLI 兼容边界与 OBS-001..006 executable fixtures 见
[V2 可观测单条采集契约](V2_OBSERVABILITY_INGESTION_zh.md)。这不是 V1 新增 RPC，
不改变本文 CURRENT V1 方法清单；后续 obs 查询扩展见第 17 节。

## 16. [TARGET V2][IMPLEMENTED] Scope assignment（SCOPE-CR-002）

对象与生命周期权威定义为
[Policy/Scope/Binding 生命周期契约](POLICY_SCOPE_BINDING_CONTRACT_zh.md)。本节替代第 6.11
节及 SCOPE-CR-001 中 Scope revision、引用阻止模板更新/删除和手动 Binding CRUD 语义。

当前公开 12 个 PAP 方法：`policy.templates.{create,update,get,list,delete}`、
`policy.scopes.{create,get,list,delete,retry}`、`policy.bindings.{get,list}`。
Scope update、Binding mutation 返回 `unknown_method`；Scope revision 参数拒绝。

```json
{"method":"policy.scopes.create","params":{"selector":{"kind":"process","match":{"processName":"openclaw"}},"policyTemplates":[{"policyId":"policy-one","policyRevision":1}]}}
```

`policyTemplates` 对全部 selector 均必填，1–32 个不同 Policy；服务端解析并在提交锁内
核对精确 current revision。并发更新/删除导致该版本不可用时返回 `not_found`，不替换版本。
Scope 返回 `scopeId`、`selector`、`policySnapshots`（完整 PreparedPolicy）和 `status:ACTIVE`；
没有 revision，不可 update。模板更新/删除不影响该 Scope 或未来实例。

`match` 必须且只能包含 `processName`、`executable` 或 `cmdlineArg`。名称精确匹配
`/proc/PID/comm`（1–15 bytes），路径精确匹配 `/proc/PID/exe` 的规范化绝对路径；
`cmdlineArg` 为规范化绝对路径，`/proc/PID/cmdline` 按 NUL 切分后任一参数逐字节相等即命中，
不解析符号链接、不推断参数位置。单进程 cmdline 最多读取 64 KiB，被截断的末尾参数不参与
匹配。均不做 glob/子串/前缀匹配。
PID selector 固定首次观察到的实例；当前拒绝 cgroup assignment。未配置发现、调度不可用或
32 个 discovery jobs 容量耗尽时返回 unavailable。

Scope get/delete/retry 参数均为 `{"id":"scope-id"}`。删除关闭准入、停止发现并清理所属
Binding，返回 `{"scopeId":"scope-id","completed":false}`，完成时 completed 为 true。
未完成 Scope 为 `DELETING`；重复 delete 不重置预算。全部 Binding 删除成功后，已停止
discovery 的 Scope 及其快照随之删除；无 Binding 时停止 discovery 后直接删除 Scope。
`scope retry` 返回 Scope 快照，仅重试所属终态失败 Binding。Binding 查询包含单策略快照、
Scope 来源和固定进程身份，不包含完整 Scope 或 scopeRevision。

编码后 Policy/Scope 单记录上限 1 MiB，列表条目预算 3 MiB。`total` 是分页前总数，
下页 offset 按实际 items 数推进。方法/字段/错误由 `pap-methods.json`、
`pap-invalid-requests.json`、`pap-crud-e2e.json` 冻结，CLI 与 daemon 成套升级。

真实 procfs + scripted Client 组合验证自动下发/清理；UDS、CLI 和 bootstrap 验证公开接口。
这不构成磁盘恢复、真实 AgentSight 或 kernel enforcement 证据。旧 Probe 入口继续拒绝。

## 17. [TARGET V2：obs 已实现] 当前用户查询

V2 已注册 `obs.sessions.list`、`obs.runs.list`、`obs.timeline.get`，使用 V2 信封；
CLI report/TUI review 只通过 daemon 读取。`sec.summary`、`sec.events.list/get/count_by` 的契约见第 6 节。
`agent-sec-cli events` 的 V1 诊断兼容范围：未知 `event_type`/`category` 输出
`Warning: Unknown ...` 和排序后的已知值列表，仍执行原筛选查询；非法整数 `--limit`/`--offset`
保留 `Invalid value for '...': '...' is not a valid integer.`，缺少 `--event-type`/`--category`
值保留 `Option '...' requires an argument.`，均退出 2。非法 `--since`/`--until` 输出
`Error: Invalid time format for ...: '...'. Expected ISO 8601 format.` 并退出 1。
参数解析错误保留诊断正文与退出码，不复刻 Typer 的终端边框排版。
obs 普通用户范围固定为内核认证的 UDS peer UID，SQL 按 `uid = peer_uid` 过滤；
root 可查询全部，包含历史 `NULL` UID。响应的历史无归属 `uid` 为 `null`，不代表 UID 0；
其安全计数为 0、分类聚合与关联为空，不推断安全事件归属。
`obs.record` 的 writer 仍必须携带可信 `uid: u32`，客户端不能指定或覆盖 UID。
参数、分页、关联与错误契约，以及 QRY-001..011 完整验收矩阵和当前证据见
[V2 安全事件与 Observability 查询设计](V2_SECURITY_OBSERVABILITY_QUERY_zh.md)。
obs 查询采用 LocalUser access policy 和服务端 QueryScope，不改变本文 CURRENT V1 事实。
`obs.sessions.list` 接受可选精确 `session_id`，先应用 UID、session、时间过滤，再统计与分页；
跨 owner 同名 session 仍返回独立条目；仅 root 的重名条目使用 `UID_SessionId` 作为下钻定位符，
历史未知归属使用 `unknown_SessionId`；未重名的 ID 保持原样。组合名称仍有歧义时拒绝；原始存储 ID 和安全关联不变，详见查询设计 §4.1。
内部 `DaemonDispatcher::handle` 使用命名的 5 秒 convenience budget；真实 UDS dispatch
使用 transport 配置生成的 `DispatchControl`，不受该默认值替代。

## 18. [TARGET V2] Secret Gateway 管理

`gateway.*` 是 V2 method family。设计见 [V2 Secret Gateway 设计](V2_SECRET_GATEWAY_DESIGN_zh.md)，
进程与权限见 [部署契约 §14](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md#14-target-v2-secret-gateway-进程权限与数据目录dproc-gw)。
Cedar HTTP 网关六个方法的清单、
分发与访问级别由 `asc-daemon-protocol` 和 `pap_contract.rs` 对照；
`gateway.key.add`/KeyAdd、`gateway.credentials`/CredentialGrant 删除，旧名称无别名。

| method | 访问级别 | params | result |
| --- | --- | --- | --- |
| `gateway.enable` | UID 0 | 无 | `{}` |
| `gateway.disable` | UID 0 | 无 | `{}` |
| `gateway.status` | LocalUser，owner UID 隔离 | 无 | `GatewayStatus` |
| `gateway.key.remove` | UID 0 | `{pairid}` | `KeyOutcome` |
| `gateway.key.list` | UID 0 | 无 | `KeyList` |
| `gateway.ca` | LocalUser，认证本机 peer，无 owner 限制 | 无 | `{caBundlePath}` |

- 无参数方法只接受省略、`null` 或 `{}`；有参数方法拒绝未知字段。
- 管理方法要求内核认证的 UDS peer UID 0，非 root PolicyAdministrator 不获得凭据管理权。
  ownerUid 是 assignment 的目标身份约束，不替代 peer 身份。共享公开 CA 不是秘密，
  `gateway.ca` 不接受 Scope/UID 参数，不返回 fake/Token/环境变量映射，也不表示 Scope CA 隔离。
- `pairid` 是 Store 分配的 opaque pair ID，不是 `github`、文件路径或秘密摘要。
  `key.remove` 全局退役该配对及引用授权，不影响其他配对；旧 fake 永久退役不可重导入复活。
  real/fake 在 root 提交 Cedar Policy 后自动登记，不提供单独 key add。
- `policy.templates.create/update` 仍接收 `PolicySource`。HTTP Cedar 引用精确
  `seccore.system@2`，注入 permit 的 `@modify`、`@real_token_file`、`@fake_token_file`、
  `@inject_header`、`@inject_format`、`@response_rewrite` 均必填；最后一个仅接受精确
  `"true"` / `"false"` 字符串并降低为 bool，不接受旧 `buffered/none` 或缺省值。
  两个 token_file 是 daemon 主机的不同规范绝对路径，CLI/RPC 原样传递；相对、
  home/env 展开、NUL、`.`/`..` 在读文件前拒绝。先纯校验及 root 授权，再安全读取
  private/root-owned/regular/single-link 文件；Store 原子登记，策略只保存可信非秘密
  规则→pair ID 关联，作者不能提交派生关联。生成/登记共用等长整串至少 80% 差异校验。
- 原生 JSON 文件策略、schema revision 1 和既有 OS PDP 不变；原生凭据执行删除。
  HTTP 不接入 OS PDP。发现旧 Gateway 数据明确拒绝，不迁移、不覆盖、不推断转换。
  只有凭据 Policy/Scope 额外要求 root，普通 file 用法、权限及快照不变；不同 file/credential
  Policy 可在同一个 Scope 分别执行，单 Policy 不混合。
- 凭据 Scope 固定 Policy revision 和 pair 关联、非 root ownerUid 与持续 process selector。
  无进程时也可准备已登记 fake/port/plan。先 enable、查询共享 CA、自行加载 fake 文件
  后再启动 Agent；discovery 自动建立 Binding，不保证 first packet。查询不迁移或修改环境。
- 多 Scope 可显式共用指定 fake；删除一个仅撤销该 Scope 授权，其他 Active Scope 仍有效。
  最后 Active Scope 删除全局永久 tombstone，包括空 Scope；实例退出不退役。
  恢复全量核对 canonical Scope/Binding/关联及身份，不单凭 PID 或重读源文件恢复。
- `agent-sec-cli key generate-fake --real-token-file ... --output ... --format auto`
  为批准的纯本地入口（默认 auto），没有 RPC、不登记或授权。未知启发式明确失败，
  以 O_EXCL 创建 0600 输出，不打印 Token；不是 daemon 失联 fallback。

本节是未开放 Gateway 的内部终态契约，以 ANOLISA `89fe4fec0` 为基线，结合
`46738d58d` Cedar 注入设计；响应算法沿用 `46e2a8682`。完整语义见
[网关设计 §4.3](V2_SECRET_GATEWAY_DESIGN_zh.md#43-响应改写)：开启时重写实际注入响应
的 headers、未压缩连续正文及 trailers，跨 chunk 匹配有界；gzip 等压缩 body 原样透传，
仍可能把 real 返回 Agent。关闭时三者均不改写，请求注入独立。
Cedar 注解是 `"true"` / `"false"` 字符串，Plan/Decision 是必填 JSON bool，拒绝缺失、null、
数字及字符串，不混用两者。保留固定队列限制、EOF/framing 和故障 fail-closed 边界，
不恢复整包缓存，不增加可配置参数，也不保证已发响应可撤回。

这是旧 Gateway 内嵌数据的不兼容修订：旧 `PreparedPolicy`、Scope `policySnapshots`、
Binding 内嵌 Policy、pinnedScope、Store/Plan/投影及旧注解模式均明确拒绝；旧原生凭据
规则即使含 bool，也不是新 Cedar 配对策略。本次无旧数据迁移或滚动升级支持，不推断
转换、不丢弃秘密/tombstone，也不覆盖不可变快照；仅更新 latest Policy 不会更新 Scope。
内部旧环境先备份、在旧版本按生命周期撤销和清理，再显式登记新配对及新 Policy/Scope。
回退边界见 [网关设计 §5.1](V2_SECRET_GATEWAY_DESIGN_zh.md#51-responserewrite-的持久化兼容边界)。
响应验收使用内部 mock，外部 API 不作为验收条件；源码检查或测试矩阵不证明最终安装
制品通过，不提供旧数据已迁移的保证。

批准的 result 契约使用 camelCase：

- `gateway.enable`、`gateway.disable` 成功返回 `{}`；失败仍明确返回错误。
- `GatewayStatus`：`{enabled: bool, ready: bool, scopes: ScopeStatusView[]}`。`ready` 必须同时满足
  enabled、cgroup v2 检查通过、代理 unit active、loaded generation 与内部当前 generation
  匹配、CA 可用。已启用时每次查询重新只读检查 cgroup，失败返回 `ready=false`，不改变状态。
  网关 ready 不替代实例 Binding `READY` 与引流完成的等待。
- `ScopeStatusView`：`{scopeId, bindings}`。bindings 为绑定责任 ID 数组；空数组不代表 Scope
  无授权，其 fake/plan 可先于进程准备。
- remove 成功结果：`{pairid}`，值为 opaque pair ID。
- list 成功结果：`{pairids: string[]}`，只列 active opaque pair IDs，不输出秘密或摘要。
- CA 查询：仅 `{caBundlePath: string}`，为各 Scope 共用的公开 CA bundle 绝对路径。
  每次重新检查 enabled、cgroup v2、unit、loaded generation 与 CA ready；未就绪明确失败，
  不需要 active Scope 或凭据 owner，不返回部分环境配置。

cgroup v2 检查要求 Linux `/sys/fs/cgroup` 的 `statfs` 类型为 `CGROUP2_SUPER_MAGIC`，
且 daemon 自身 `/proc/self/cgroup` 视图有 `0::` 条目；证据不可读或非 v2 时失败。
`gateway.enable` 在改变 enabled 前检查，apply 在每次发布前检查（含已启用状态恢复）。
默认关闭恢复不运行此预检，不会仅因 cgroup v2 不可用而失败；不自动降级为 v1、不改挂载。
这不是完整内核/nft 能力预检：不独立探测 nft `socket cgroupv2`，实际规则应用仍可能失败。

内部保留 generation、撤销 fence 和永久 tombstone，用于发布、恢复与拒绝旧 fake；这些状态
不构成管理查询的展示字段。

| code | 场景 |
| --- | --- |
| `invalid_argument` | 参数、pair ID、Scope ID、Cedar 注解或配对文件不合格 |
| `not_found` | canonical Scope 或 pair 不存在 |
| `conflict` | canonical Scope 已关闭或快照/关联/执行身份不一致；fake/real/tombstone 冲突 |
| `permission_denied` | 非 root 凭据管理或越过 owner UID 的 status 查询；公开 CA 无 owner 限制 |
| `resource_exhausted` | 监听端口或 fake 等有界容量耗尽 |
| `unavailable` | gateway 未配置/禁用、账户缺失、cgroup 不可检查或非 v2、unit/CA/generation 未 ready |
| `internal` | 数据面 generation 拒绝、存储损坏或宿主操作失败 |

gateway dispatch 预算为 45 秒，CLI 默认 60 秒，generation 加载最长 20 秒。错误可以含路径、
Scope ID 或 UID，不含真实值/fake。源码接口核对不替代安装态与协议运行验收。

## 19. [TARGET V2，已实现] Intent 任务策略候选

新增 `intent.propose`，使用 V2 `{method,params}` / `{requestId,result|error}` 信封。
方法按 `LocalUser` 授权，UID 来自内核 peer credentials，不接受调用者声明身份。
这是初次任务提案接口，不改变 CURRENT V1 清单，也不调用 PAP、批准或下发接口。

| `params` 必填字段 | 契约 |
|---|---|
| `format_version` | 精确字符串 `asc.intent.request.v1` |
| `task` | 非空 UTF-8 正文，最多 64 KiB |
| `ttl` | 整数 1–1800 秒，从 daemon 接受生成请求时起算 |

拒绝未知字段及版本，包括路径类字段、自报身份、
模型配置、摘要和批准状态。完整请求含 OTel 信封、空白、转义与 LF 最多 256 KiB。
daemon 不打开调用方路径，不采集文件正文、目录列表或 Git 变更。配置的 HTTPS 模型只接收任务
正文和服务端生成的来源标识，并在 system 中接收统一 schema；任务中的敏感文本会发送，
可能产生调用费用。

`result.outcome` 有且只有以下两种状态：

- `candidate`：完整 `asc.intent.candidate.v1`，保留 `asc.intent.draft.v1` 原始草稿、
  `artifact`、TTL、`notes` 及内容哈希；恒为
  `CANDIDATE` / `authorization_required=true`。`artifact` 带 language/text/schema_sha256。
  合法策略即使包含非空 `unresolved` 也返回候选；CLI 展示后退出 2，不开放批准菜单。
  模型 policy 为空时 text 为空；不包含 rules/mapping/back_translation、绑定或保护列表。
- `needs_clarification`：原始草稿及 `reasons`，用于非规范路径或不受支持的模板、身份、注解，
  没有可批准候选。Cedar 语法或 Strict 失败返回错误，不返回部分策略。

`intent_extraction.rs` 的 `generate_policy_draft` 只将 task 文本和统一 schema 交给模型，
模型仅返回 `{"policy":"...","unresolved":[]}`。草稿保存原始响应及任务来源。
提示词从 `policy-generation.md` 编译嵌入，标记为 `cedar-policy@1`。实体、动作及字段语义
直接复用内嵌 schema 和注释，提示词仅保留生成约束。
`policy_validation.rs` 解析原生 Cedar、检查路径表达式并进行 Strict 校验，不追加任何规则；
`policy_generator.rs` 封装 TTL 和完整候选哈希。

链路为 task + schema → 模型 policy/unresolved → 官方 parser 与路径表达式检查 →
官方 Strict → 未批准候选。前端不通过字符串替换重写模型语义；路径仅接受
规范绝对字符串的 ==/!= 或目录末尾 /* 的 like，拒绝动态路径、自报 Subject/File UID、
未绑定模板及可选 @id 之外的注解。统一 schema 来源仍是
`asc-policy-cedar/schema/seccore.cedarschema`，本接口不生成或扩展 schema。
CLI 复核版本、任务来源、TTL 和完整哈希，并使用内嵌 schema 从 draft.policy 重建
预期 artifact；替换模型输出后重算哈希仍会被发现。CLI 与 daemon 必须同步升级，旧版本不适配。
历史候选保持原版本；不能重新标记为新候选，须重新生成。

提示词要求必要身份、条件、审批或义务无法表达时放入 unresolved，不得删掉后生成无条件
permit；工具调用不能替换为程序执行。但 unresolved 是模型自报，不是完整性或冲突证明。
程序不求解策略冲突。普通 permit 不排斥其它策略的允许；排他要求须在原生
Cedar 中明确补集 forbid，write 补集须限定同一 operation。禁删不等于不可变；task 要求
“不删除 a.txt”时由模型自行写出对应 forbid，程序不再替用户补充保护规则。
Strict、空 unresolved 和哈希均不证明任务理解正确、全部要求已落实、可信批准或真实对象绑定。

模型 JSON 最多 16 KiB；unresolved 最多 128 个非空项，每项最多 4096 字节且不含控制字符。
最终 Cedar 最多 1024 条策略、256 KiB，超限不进入 Strict。
预算超限返回 `resource_exhausted`，无候选，不截断策略。

完整结果最多 512 KiB。CLI 默认等待 45 秒，真实 UDS 为该方法配置 35 秒派发预算，模型
预算最多 30 秒且受请求期限和 TTL 进一步收紧。每 UID（含 root）最多一个实际生成，全局
最多四个，无排队；连接取消或超时后，仍在阻塞的生成占用名额直到实际返回。失败无自动重试，
到期结果丢弃，不刷新有效期。

错误使用封闭类别：参数/版本错误 `invalid_argument`；超量/并发占满 `resource_exhausted`；
未启用、配置或模型响应无效 `unavailable`；取消/超时/到期 `deadline_exceeded`；内部编码或
Cedar 校验失败 `internal`。诊断不包含任务正文、原始模型响应或密钥。

配置于 daemon 启动时读取：`ASC_INTENT_ENABLED=true`、`DASHSCOPE_API_KEY`，可选
`ASC_INTENT_MODEL`、`ASC_INTENT_BASE_URL`、`ASC_INTENT_THINKING=disabled|provider`。
保留默认 `deepseek-v4.1-flash`，推荐作为速度优先的选择，不要求显式配置模型。可切换其它
可用模型和 OpenAI-compatible HTTPS 服务，响应协议及 Cedar 校验不绑定模型名称；切换服务时
API key 使用所选服务的凭据。不支持 DashScope 思考扩展的服务须设置 thinking=provider。
自动选择模型和本地回退尚未实现。
禁用并重启即可关闭模型入口；候选及批准请求不持久化，没有 Intent 状态迁移或部署回滚。
CLI 只渲染候选，不交互、不产生请求，也不产生授权；任务无法表达时照常输出 `reasons` 并以退出码 `0` 结束。

可执行契约位于 `v2/crates/asc-intent/tests/{policy_prompt,policy}.rs`、
`v2/apps/asc-cli/tests/intent.rs`，以及协议、handler、core 的 Intent 单元测试。
覆盖参数/版本拒绝、原始响应和任务哈希绑定、原生 Cedar 组合裁决、模型输出重建与篡改检查，
以及失败隔离、并发名额、过期及输出检查。
实际进程、路径别名、rename 两端与参数索引等事实仍由未来执行端保证；这些测试不是执行拦截证据。

### 19.1 [TARGET V2，已实现] 单次命令 Cedar API

`intent.propose_command` 面向 hook 集成方，仅从已有的 `tool_input.command` 和 `cwd`
推导单次命令所需权限的 Cedar 候选。它不是 `intent.propose` 的 task/TTL 接口，也不是
基于 base policy 的更新接口；不改变 CURRENT V1 清单。

```json
{"method":"intent.propose_command","params":{"format_version":"asc.intent.command.request.v1","command":"read -r line < report.csv","cwd":"/work"}}
```

`params` 仅接受这三个必填字段，拒绝未知字段与版本。`command` 为非空 UTF-8 shell 文本，
最多 64 KiB，不含 NUL；`cwd` 为最多 4096 字节的规范绝对路径，不含控制字符、反斜杠、
`.`/`..` 路径分量、重复 `/` 或非根尾部 `/`。cwd 是路径解释基准，不是允许访问的范围。
不要求宿主增加 task context，不采集文件正文、路径元数据或环境变量。

沿用 V2 `{requestId,result|error}` 信封，`result` 只有以下两种结构：

```json
{"outcome":"candidate","policy":"permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/work/report.csv\" };","caveats":[]}
```

```json
{"outcome":"needs_clarification","reasons":["需要确认运行依赖"]}
```

示例仅演示结构，不承诺模型对该命令一定返回相同策略。`policy` 是 Cedar 文本，不是
`permission.json` 或完整候选信封；命令很短也应有策略，只有命令完全没有文件、进程、网络
动作（如 `true`、`:`、`pwd`、`echo`、`date`）时才是空文本。程序沿用原生 parser、路径检查与 Strict 校验 ，不追加规则。

`caveats` 是候选已知但未覆盖的部分，例如 PATH 未解析导致 `file.exec` 目标无法确定、
脚本内部行为未知或管道数据流无法推导；它不阻断候选，但审批与后续 PDP 映射必须一并查看，
不得当成已授权。空 `policy` 是合法候选：命令没有文件、进程、网络动作时授权为空，
`caveats` 随候选返回，沙箱只差运行依赖。任何模型 `unresolved` 仍转成 `needs_clarification`
并丢弃部分策略；只有命令语义本身无法表达、给不出任何有意义权限时才使用它。非规范路径或
不支持的身份、模板、注解同样要求澄清。语法或类型错误返回 `internal`。空 `unresolved`
和 Strict 均不能证明需求完整、最小权限或审批。提示词要求可确定部分照常输出、不确定部分
写入 `caveats`，不得以根目录、整个 cwd 或全网络权限兜底；没有语义完备性证明器。

Rust 入口为 `asc_intent::propose_command(provider, model, &CommandInput, deadline)`。
调用链为 dispatcher → `asc_daemon_core::intent::IntentService::propose_command` → 意图模块。
认证、模型配置及错误分类沿用本节规则；与 `intent.propose` 共享每 UID 一个、全局四个实际执行额度。
完整请求含 LF 最多 256 KiB；模型响应最多 16 KiB，原因最多 128 项，每项最多 4096 字节。
超限不截断。UDS 派发预算 35 秒，模型及校验最多 30 秒，并受调用方更早期限限制。
没有 TTL、策略 revision、base policy 或候选持久化；取消或过期后丢弃结果。

需在 daemon 启动环境显式启用 Intent 并配置本节的模型凭据。command/cwd 会发往该模型服务，
可能包含敏感信息并产生费用。本接口不读取或改写 PAP/base policy，不计算 effective，也不执行命令。
PDP 仍负责将申请权限限制在 PAP 管理的 Cedar 策略授权范围内；CLI 的
`--format cedar` 输出未审批候选原文，审批与执行不在本接口范围内。程序、cwd 与 loader
不在候选中，需执行侧显式补齐，CLI 不做隐式补齐。
停用 Intent 并重启可关闭入口，无状态迁移或策略回滚需求。

候选是**命令自身**的需求集，不是完整沙箱视图：程序、loader 与 PATH 目录不在候选中，
`sandbox run` 要求 program 与 cwd 显式可读，需执行侧补齐。最终由 PDP 计算
`effective = 请求 ∩ PAP 策略授权`，默认 deny、deny 优先。程序和运行依赖须同时在申请中
声明并获 Cedar 授权；候选不列出这些路径不代表它们已获准，缺少授权会在 PDP 阶段拒绝。

CLI 不再提供 `--format permission` 投影。

CLI 通过 `agent-sec-cli intent propose-command --command <文本> [--cwd <目录>]
[--format json|text|cedar]` 暴露该 RPC。它只发送这两个业务字段；`--cwd` 缺省为 CLI
进程的当前工作目录，并在本地解析为规范绝对路径，不读取命令目标。默认输出 JSON 便于 hook 解析；
`--format text` 汇总权限并列出未覆盖项，`--format cedar` 把 `caveats` 写成注释，澄清时只输出注释；
CLI 在渲染前对返回的 Cedar 重新执行一次本地校验。两种结果退出码为 `0`，
本地可判定为非法的输入退出 `2` 且不发出请求。该子命令不执行命令，也不接入 sandbox。

可执行契约为 `v2/crates/asc-daemon-handler/src/intent.rs` 中的两个 `command_api_*` 测试和
`v2/apps/asc-cli/tests/intent.rs` 中的 `command_*` 测试：分别使用内存 fake provider，
经实际 dispatcher 和实际 CLI/UDS 链路，覆盖输入透传、两种结果、
版本/字段/路径/预算拒绝、异常模型响应、客户端复核及取消/过期结果丢弃。
测试不调用真实模型或执行 sandbox，不构成模型推导质量或完整运行链路的证据。

## V2 sandbox 授权扩展

`authorization.evaluate`、`authorization.claim` 与 `authorization.report` 使用现有 UDS、JSON 信封和 peer UID，
不修改 V1 oracle。这些方法面向已准入的运行时调用者，不接受调用方上传 Cedar 策略。
授权 `request_id`、`context.execution_id` 位于 params 内，与传输层生成的 request ID 分开。

| method | params / data | 处理 |
|---|---|---|
| `authorization.evaluate` | `OsAuthorizationRequest` → `Decision` | 读取一个有效策略版本、求值；不启动进程、不等待 ask。 |
| `authorization.claim` | `DecisionClaim` → `ClaimResponse` | accept 在期限内接收；execute 向同 UID 一次性转交原请求、决定与已固定的执行状态；abandon 关闭未执行交接。不重新授权。 |
| `authorization.report` | `ExecutionReceipt` → `ReceiptAck` | 校验当前 peer 对决策的归属和执行引用，确认日志持久化；不重新授权或执行。 |

类型定义见 [`authorization.rs`](../../v2/crates/asc-daemon-protocol/src/authorization.rs)。
公共 `AuthorizationRequest<C>` 是 PARC 信封，字段为 request_id、
principal、action、resource、context。三个实体引用均为 `{type, id}`；完整信封参与摘要。
OS 特化 `OsAuthorizationRequest` 的 context 包含 execution_id、agent、launch、
requested_permissions；launch 仅含 program、resolved_program、args、cwd。
OS 适配器固定 principal 为 `SecCore::Subject::"request"`，action 为
`SecCore::Action::"process.spawn"`，resource 为与 launch.program 一致的 `SecCore::Process`。
agent 只含可选 agent_type。未知 action、绑定不一致、未知字段和原 type=os 载荷拒绝。
通用 context 可供其它 consumer 定义；当前 RPC 只处理 OS 请求，未提供通用非 OS 决策端。
PEP 在本地固定环境与路径身份，跨进程交接时在 claim 的 accept 阶段一次性提交 execution。
execute 和 abandon 只传标识与阶段。daemon 与授权客户端须同步升级。
Decision 返回最终命令、有效权限、策略版本、接收期限和 warnings。仅当路径条件要求区分文件与目录时，require_files 列出须由执行侧核实为文件的路径。
`approval_requirements` 是 V2 sandbox 决定附带的展示来源，缺省为空，空列表不序列化。
每项包含 `target` 与可选的策略 `message`：target.kind 为 execution、filesystem 或 network。
execution 的 `original_request` 区分最终执行要求与改写前保留的审批要求；filesystem 包含规范路径
`path`、`right`（read/write）和 `excluded_paths`，表示不含更具体子区域的最终权限区域；network
表示完整宿主网络。PDP 将代表性子路径探测映射回真实区域，保留对应审批文案，不输出内部探测路径。
只记录 escalate 触发的事项；它们不新增授权、不代替 effective_permissions，也不是独立审批开关。
改写前的事项转为 original_request=true 的执行要求；最终资源项仅来自对最终命令的重新求值。
PEP 从已固定的路径事实及只用于标签的本地查询区分文件与目录，不根据命令或原因文字猜测权限。
新客户端可读取缺少该字段的旧决定并回退为整体原因。旧版严格解码器不接受非空新字段，因此本次
未发布的 V2 扩展应连同 daemon 与 CLI 一起更新；不改变 V1 oracle。对应 fixture 为
`asc-daemon-protocol/tests/authorization_display.rs` 和 `asc-pdp-runtime/tests/authorization.rs`。
公共信封和 OS 类型拒绝未知字段。daemon 默认启用授权，从 PAP 同一事务读取当前适用
策略集；策略更新成功后供下一次授权使用，空集返回 DENY，读取或加载失败不回退旧版本。

MODIFY 携带 PDP 已完整授权的最终命令与权限，绑定原请求摘要；PEP 本地核验后直接执行。
DEFER 没有执行权，PEP 停止并回执。STEP_UP 的本地确认通过后
直接执行绑定动作，不再 evaluate。PEP 须在签发后 60 秒的 expires_at 前一次性接收决定；接收后的审批等待和执行不再检查该期限。report 只接受同一 peer UID 的决策，包含 approval、
installed/started、exit_code/signal/cancel_signal、错误和记录时间。
已成功报告的决策拒绝重复回执，包括 DEFER。未通过 claim 接收的决策保留到 expires_at 后 24 小时；已接收交接保留到回执或 daemon 重启，仍受全局和每 UID 容量限制；它不限制已接收决定的执行。回执事件中的 warnings 来自 daemon 保存的 Decision。

领域 DENY 是成功 RPC 中的决策。默认 daemon 不再因缺少授权配置返回 `unavailable`。
错误使用既有目录：嵌入式 dispatcher 未接入授权服务时返回 `unavailable`，
非法载荷 `invalid_argument`，跨 UID `permission_denied`，过期/引用冲突 `conflict`，
容量不足 `resource_exhausted`，存储或内部失败 `internal`。只有所有配置的 SQLite/JSONL
记录成功后才 Ack；部分写入失败返回错误，不承诺跨存储事务或自动重试。
身份字段描述逻辑 Agent，由接入方提供；peer UID 约束决策与回执归属，不认证自报 agent_type。

fixture：`asc-daemon-handler/src/authorization.rs`、`asc-daemon-core/src/authorization.rs`、
`tests/e2e/linux-sandbox/authorized_cli.py`。部署和后端边界见
[Sandbox 主设计](LINUX_SANDBOX_PEP_DESIGN_zh.md)。
