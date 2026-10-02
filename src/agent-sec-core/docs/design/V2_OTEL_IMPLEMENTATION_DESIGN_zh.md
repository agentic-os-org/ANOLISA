# V2 原生 OpenTelemetry 上下文与 tracing 实现设计

`[TARGET V2]` 首期已实现 Rust CLI → UDS → daemon handler → PAP/compiler 的 tracing、
关联输入适配与本地关联日志。具体测试结果及限制见 [验收记录](V2_OTEL_ACCEPTANCE_zh.md)。
本文保留架构决策和接入约束；配置详表见 [V2 README](../../v2/README.md)。

本次是首次接入 OTel。V1/V2 指 Python/Rust 产品实现；`--trace-context` flat JSON 和
observability metadata 是继续支持的 caller 输入，carrier 的 `version: 1` 是新增 wire 格式版本。
本地 Agent 链路重组、业务记录存储/查询及未迁移安全能力不在本次范围内。
基础设施通过不代表全部 V1 用户能力已迁移。

## 1. 调用链与实现范围

```text
asc-cli main → 同步 asc-daemon-client → UDS service
  → spawn_blocking → DaemonDispatcher::dispatch / handle
  → PapHandler → PapService → Compiler / ProcessLocalPapRepository
```

两个产品 main 初始化 runtime；client、dispatcher、PAP 和 compiler 建立 span。
service 保持 protocol-independent，不解析 tracing JSON。当前 PAP 使用 process-local repository；
本次未实现 Action Runtime、Rust SecurityEvent sink 或 observability record RPC。

基础设施按 `GREENFIELD_CONTRACT` 验收，已有输入适配按 `ADAPTER_CONFORMANCE` 验收，
已迁移用户能力按 `MIGRATION_EQUIVALENCE` 验收。新增协议/部署行为需同步现有契约与 fixtures，
本文不替代语言无关行为契约。

## 2. 上下文与身份决策

OTel Context 是唯一的 tracing 上下文载体，包含 SDK span、Agent Baggage、有限兼容标签
和请求 span 引用。入口负责绑定，内部服务直接读取当前 Context，无需逐层传 metadata。
不另建 AgentContext store、进程 singleton 或自定义 tracing ID；授权 principal、deadline、
cancellation 等业务状态仍由各自契约管理。caller attribution 不能作为授权依据。

TraceId、SpanId、parentage、flags 和 tracestate 由 SDK/propagator 管理。无上游请求也创建
SDK root；生产固定未采样，关闭日志输出仍应具有有效身份和可读 Agent 字段。

| 身份或关联字段 | 处理 |
| --- | --- |
| session/run/call/tool_call ID | 保留 Agent 业务语义，不能用 TraceId 替代 |
| 自动生成的 invocation ID、仅诊断用 request ID | 不再独立生成；使用 SDK TraceId + SpanId |
| 子操作日志的请求归属 | 同时携带 `request_span_id`，引用 SDK 已创建的请求 span；与 TraceId 配对 |
| 公开 PAP `requestId` | 继续保留 UUID 契约，不能直接换成 SpanId |
| caller 显式 opaque `trace_id` / invocation 标签 | 同一 Context 的 `CompatibilityCorrelation` typed extension；不转换成 SDK ID |
| event/resource ID、支持查询/取消/重试的 operation ID | 保留业务身份和独立生命周期 |

兼容标签不进入 OTLP attributes 或第三方 HTTP header。支持中的 V1 输出/历史查询必须按原
标签解释，不能覆盖历史 ID；相应 reader/存储迁移由对应工作包验收。本次没有实现历史查询。
未显式提供标签时不生成兼容 UUID，也不强制给原可选输出补字段。
日志关联不能依赖 Collector 中的父子树，因为未采样时该树可能不存在。

## 3. Crate 与 API 边界

| 位置 | 职责 |
| --- | --- |
| [asc-observability](../../v2/crates/asc-observability/src/lib.rs) | `parent_span`、`request_context`、`snapshot`、状态和关联诊断 API |
| [fields.rs](../../v2/crates/asc-observability/src/fields.rs) | 五字段、输入适配、metadata 校验、只读投影与 `AgentFieldProcessor` |
| [propagation.rs](../../v2/crates/asc-observability/src/propagation.rs) | W3C carrier、白名单、编码与资源限制 |
| [runtime.rs](../../v2/crates/asc-observability/src/runtime.rs) | provider、subscriber、本地诊断、shutdown |
| [protocol trace.rs](../../v2/crates/asc-daemon-protocol/src/trace.rs) | 纯 wire DTO 和预算，不依赖 OTel SDK |
| CLI/daemon main | 选择 runtime feature 并初始化一次 |
| client/handler | DTO 与 Context 的边界适配；业务函数不新增 Context 参数 |
| PAP/compiler | 原生 `tracing` 埋点，不初始化 SDK |

`asc-observability` 不依赖 daemon/PAP/Policy types；propagation 使用 `Extractor/Injector`。
快照是只读消费结果，不是另一份可修改、持续传播的上下文。

依赖采用 tracing 0.1、tracing-subscriber 0.3、tracing-opentelemetry 0.33、OTel/SDK 0.32。
精确版本和 features 以 [Cargo.toml](../../v2/Cargo.toml) / Cargo.lock 为准；保持 Rust 1.88 MSRV，
不独立升级出不兼容的 OTel 类型。同步 CLI 不因导出而增加应用级 Tokio runtime。

## 4. Context、Baggage 与 span

bridge 显式开启 `with_context_activation(true)`：进入 tracing span 时激活对应 OTel Context，
退出后恢复。Context 更新返回新值，不原地修改父作用域或其他 task。

| Agent 字段 | Baggage / span attribute key |
| --- | --- |
| agent_name | `agentsec.agent.name` |
| session_id | `agentsec.session.id` |
| run_id | `agentsec.run.id` |
| call_id | `agentsec.call.id` |
| tool_call_id | `agentsec.tool_call.id` |

五个值最多各 256 Unicode 字符，超长按 V1 `...[truncated]` 后缀规则截断。
`--trace-context` 输入先选有效 snake_case，再选 camelCase；仅接受 string，按 Python strip
去首尾空白，空值/无效值忽略。Unicode 空白与字符截断由冻结 fixture 验证，不能直接假定 Rust trim 等价。
原生 Baggage 和 metadata 保留空字符串及首尾空白；metadata 的别名规则另见 9.4。
注入适配器使用 percent encoding 保留值，避免 SDK 注入时 trim 改变业务语义。

`AgentFieldProcessor::on_start` 将允许的 Baggage 投影到 recording span attributes。
未采样时消费者仍从 Context 读取，不能依赖 processor、span attributes 反查或 exporter 回查。
该回调晚于 sampling，当前 sampler 不按 Agent 字段决策。Agent ID 不用作 metrics label，
`agent_name` 也不决定进程 Resource 的 `service.name`。

## 5. UDS 协议与入口隔离

### 5.1 原生 carrier

保留 `method/params`，新增可选的 `traceContext` 和 `compatibility`：

```json
{
  "method": "policy.templates.list",
  "params": {"limit": 100, "offset": 0},
  "traceContext": {
    "version": 1,
    "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    "baggage": "agentsec.agent.name=openclaw,agentsec.session.id=session-123"
  },
  "compatibility": {"version": 1, "traceId": "trace-1"}
}
```

两个对象省略或为 null 均可；有对象时 version 必须为 1。
`traceContext` 仅接受可选 string `traceparent/tracestate/baggage`，缺失/空字符串表示该部分为空，
字段值 null 不合法。`compatibility` 仅接受可选 `traceId/invocationLabel`，缺失/null 表示 absent，
string 按 V1 归一化。未知字段、重复 JSON key、类型或版本错误返回 `invalid_request`，不执行业务。
原生 DTO 不接受 flat Agent 字段或别名，也不提供任意 span 名称/属性写入接口。

兼容标签通过显式 DTO 跨进程传递；typed extension 本身不会自动序列化。
支持中的 V1 CLI/UDS 输入由其版本化 adapter 承接，不能把原生 DTO 的严格规则直接套给 V1 caller。
无 context 不代表协议版本，也不授予调用任意方法的权限。

### 5.2 内容错误与预算

| 内容情况 | 结果 |
| --- | --- |
| 无合法 traceparent | SDK 新 root；合法 Baggage 可独立保留 |
| 合法 traceparent | 新 SERVER span 接续远端 parent |
| 合法 parent、SDK 判定 tracestate 无效 | 保留 parent，清空 tracestate；无合法 parent 时不使用 tracestate |
| 未知 Baggage key | 丢弃，不解码其值、不记录原文；member 语法仍校验，properties 不保留 |
| 允许 key 重复、member 语法无效、允许值 UTF-8 无效或超限 | 丢弃整个 Baggage，技术 parent 独立处理 |
| 允许值为空/超长 | 保留空值，按字符上限截断 |

内容错误使用固定诊断原因，不改变业务请求的接受性；JSON schema 和帧预算错误仍拒绝。
标准语法依赖锁定的 SDK。已知 SDK 0.32 `TraceState` 接受重复 vendor key，当前测试没有
证明完整 W3C 合规，不能把所有 malformed tracestate 都描述为必然拒绝。

traceparent/tracestate 各 ≤512 bytes；Baggage ≤16384 wire bytes、入站 ≤32 成员、过滤后 ≤5。
先过滤和归一化再构造 SDK Baggage，避免未知字段占用 SDK 8192 bytes 的原始值容量。
五个各 256 四字节字符的值连同 key，原始共 5210 bytes，percent encoding 后共 15459 bytes；
真实 client/server 往返测试验证容量。V1 原始输入仍遵守自身入口限制，不能提前套用编码后的上限。
8192 bytes 是 W3C 跨实现最低保证，允许更高限制；锁定 SDK 的标准 BaggagePropagator 对
超过 8192 的 encoded header 整体丢弃。当前全局标准 propagator 与本地 adapter 的上限不同。
缩小 ASCII 转义集合不会减少非 ASCII 的编码长度；跨服务传播须另行约定容量，不静默缩短 metadata。

请求保留 **4 MiB 业务预算（含 LF）+ 32 KiB 传播及结构开销预算**；handler 分别校验，
service 只执行总帧上限。业务计量保留原 JSON 空白和转义，不重新序列化后隐藏超限。
预算超限返回 `invalid_request`，service 总帧超限按 `resource_exhausted` 处理；响应仍限 4 MiB。

### 5.3 入站步骤

每次请求从 `Context::new()` 提取允许的 carrier 和兼容标签，创建 `parent: None` 的 tracing span，
在进入 span 前设置 OTel parent，再由 bridge 激活 Context，并绑定 `request_context()` 作为日志锚点。
不继承 daemon 环境、后台 span 或上一次请求的 Agent 字段；无 parent 时由 SDK 生成 root 身份。
初始化应验证 SDK 身份有效，观测不变量失败不能用 UUID/no-op provider 伪装恢复。
退出作用域恢复原 Context；观测故障不增加业务重试。

### 5.4 升级边界

先升级 daemon，再升级默认注入 carrier 的 client；未接入 carrier 的 daemon 会拒绝新增字段。
不探测后重发，不删字段重试写操作。回滚顺序相反；关闭日志不会关闭 carrier。
当前仅支持新 CLI 与支持 carrier 的新 daemon；旧 daemon 不在兼容验收范围。
部署先升级 daemon；这不等于全部 V1 method/输出已兼容。实际 Agent hook 在其 capability、
输出及查询消费者完成等价验收后才切换到 Rust；Python 不成为 V2 runtime 依赖。

## 6. CLI 与 client

`--trace-context <JSON>` 保留 flat 输入、等号形式、重复 last-wins、首个 command token/`--` 边界、
缺值错误及退出行为；该输入在 help 前校验。`--otel-context <JSON>` 接受原生 carrier，重复时拒绝。
两者组合时先绑定原生 parent/Baggage，再以 flat 输入的有效 Agent 字段覆盖同名值；缺失/无效值不覆盖。
opaque trace_id 只进入兼容标签。

CLI 无输入时创建干净 command root，不读取 Agent 配置、不猜测 session/tool ID，也不自动发现
`TRACEPARENT/BAGGAGE` 环境变量。显式 `AGENT_SEC_INVOCATION_ID` 保留为兼容标签，缺省不生成 UUID。
SDK 初始化在 help/version 返回之后。选项示例见 [Policy CLI 指南](../../../../docs/user-guide/zh/agent-security/agent-sec-core/policy-cli.md)。

`asc_daemon_client::call` 的显式 carrier 完整替换 ambient parent/Baggage/兼容标签，只采用 request
携带的 compatibility。没有显式 carrier 时继承当前 Context；显式 compatibility 对象替换 ambient 标签，
包括仅 `{"version":1}` 的清空操作，省略/null 才继承。库调用者负责初始化 SDK，不由库安装全局 subscriber。

client 创建 CLIENT child，从 child 注入请求副本，不能直接转发输入 parent 或修改 caller request。
CLI 消费初始 carrier 后让 client 继承 command scope，形成：

```text
external span（可选）→ cli.command → daemon.client → daemon.request → PAP/compiler
```

保持同步 connect/write/read 的单次 deadline、不重试、stdout 和业务退出码；编码/解码在既有 I/O
计时范围外。尚未迁移的真实 hook 整体 timeout 由对应工作包验收。

## 7. daemon span 生命周期

### 7.1 操作边界与名称

request span 从 envelope 解码后开始，覆盖授权、PAP、响应编码，到 dispatcher 实际返回为止；
不覆盖 socket 排队、读帧及最终写 socket。内部操作按实际执行范围建立 child。
span 名称由 callsite 自行定义，不设 enum 或注册表；稳定表示操作类别，不拼接 ID、路径或原始 method。
跨进程调用使用 CLIENT/SERVER，内部操作通常使用 INTERNAL。

### 7.2 公共入口

公开 `handle` 和 wire `dispatch` 共用 scope 构造及私有路由逻辑；dispatch 在 scope 内编码响应，
不经公开 handle 再创建一层 root。授权继续来自 kernel peer credentials 和服务器生成的 principal。

### 7.3 失败与取消

- 无效 envelope、dispatch 前已取消、Busy/read timeout/frame-too-large：独立干净 reject root，不解析坏 payload 获取 parent。admission 已满而直接关连接时不承诺每个 socket 都有 span。
- unknown method、permission denied、业务参数错误：在有效 request span 记录稳定 public error code；未知 method 使用安全占位值。
- 响应编码失败：在 request span 记录固定原因；service 写回失败不伪称已与该 request trace 关联。
- PAP panic unwind：guard 标记固定失败类别、关闭 span 并恢复 Context；不收集 panic payload，不改现有 panic hook。abort/SIGKILL 不保证正常结束或导出。
- service 等待超时：仅请求 cooperative cancellation。blocking 工作实际结束才关闭 span，分别记录实际 outcome 与 `cancel_requested`，不能提前声称副作用取消。

除传播帧预算外，保持现有 service 的 timeout、admission、drain 和 response 规则。

## 8. 同步、async 与线程传播

| 边界 | 接线 |
| --- | --- |
| 同步子操作 | 原生 span / `in_scope`，bridge 激活当前 Context |
| async future | `future.instrument(child_span)`，每次 poll 进入/退出 |
| spawn / JoinSet / spawn_blocking / OS 线程 | 先捕获完整当前 Context，以其为 parent 创建 fresh child；future 使用 instrument，线程内使用 in_scope |
| 仅 OTel Context 的第三方 future | `FutureExt::with_context`，避免与 bridge 绑定相矛盾的 scope |
| 跨进程 | 显式允许边界上的标准 carrier 和有限兼容 DTO |

不能只 clone tracing span：进入 span 后新绑定的 Baggage、typed 标签和 request anchor 也必须捕获。
局部 subscriber 测试同时传播 Dispatch。不得跨 await 持有 enter/attach guard，不把 live request span
长期保存在 repository/job queue。当前 daemon 在 blocking 闭包内建立请求 scope，无需跨外层 service 传播。

## 9. 状态、属性与消费者

### 9.1 结果语义

span 成功默认 Unset，失败显式 Error；`agentsec.execution.status` 记录执行结果，
安全 verdict 单独表达，不能把正常 deny 判断当作内部执行失败。PAP accepted/pending 也不代表实际 enforcement 完成。

### 9.2 属性与日志过滤

除五个 Agent 字段外，仅记录有界 method、operation、公开 requestId、public error code、execution status、
cancel_requested 等安全字段。每 span 最多 32 attributes、0 events、8 links；Agent 值上限各 1024 UTF-8 bytes，
其他诊断字符串最多 256 bytes。缺失字段省略。Resource 使用进程 service.name、构建版本及有限部署属性。

原生 instrument 使用 `skip_all` 再列安全字段；不得导出 params/result Debug、prompt/code/command、PII、
路径、token、原始 carrier、异常链或 panic payload。bridge 仅接收产品 span，普通日志 event 和 SDK
内部日志不进入关联诊断。新增埋点需验证 target filter 接收且 parent 没有被过滤。

`diagnostic` helper 输出固定 reason 与白名单快照，默认日志级别 warn，`RUST_LOG=info` 可启用。
诊断 producer 只写有界队列，由独立线程写 stderr；满队列/写失败可丢记录，
队列和关闭预算见 DPROC §11。普通日志/eprintln 不会自动补关联。日志 EnvFilter 与 OTel layer 分离，`RUST_LOG=off` 不关闭身份；
也不得用静态 max_level_off 编译掉请求埋点。现有业务 stderr 继续遵循原契约。

### 9.3 SecurityEvent

下游 sink 在 scope 内调用 `snapshot()` 捕获 SDK 身份、Agent Baggage、请求锚点和兼容标签。
异步落盘使用快照，不保留 live span、不检查 sampled/recording、不等待 exporter。
安全事件持久化及其失败语义独立于 best-effort 关联诊断；本次仅验证快照可读，未实现业务 sink 或历史 reader。

### 9.4 observability 消费者适配约定

传播方式与其他调用一致，**必填校验按记录接口区分**。caller 继续提供原有
`hook/observedAt/metadata/metrics`；入口 adapter 绑定记录专属 Context，内部服务无需 metadata 参数，
从 `validate_metadata(kind)` 取得关联。原生入口可使用相同 carrier 建立干净 Context。
普通请求缺少 Agent 字段仍可执行；记录缺少必需字段须明确报错并停止写入，不能用新 TraceId 代替。

`bind_metadata(parent, value, kind)` 先验证该记录自己的输入，再替换记录字段：

| 规则 | 行为 |
| --- | --- |
| 所有 kind | session_id/run_id 必填；ToolCall 另要求 tool_call_id |
| ModelCall / ToolCall | call_id 可缺失或 null |
| alias | camelCase 优先，即使为 null 也不能退回 snake_case |
| 值语义 | string 才合法；空字符串/首尾空白保留，只做长度截断；extra 按原 schema 忽略 |
| 绑定 | 替换 session/run/call/tool_call；缺失的可选字段和不属于该 kind 的字段清除，不能继承父值补齐 |
| 保留 | SDK parentage、request anchor、兼容标签、独立 agent_name；agent_name 不属于 V1 metadata schema |

OTel `with_baggage` 会合并，替换时先清除旧 Baggage 再绑定允许字段；不修改父 Context，退出记录 scope 后恢复。
原生父子传播正常继承，由消费者独立校验必填项。冻结 [metadata fixtures](../../v2/fixtures/tracing/README.md)
覆盖 46 输入 × 3 schema；空值可接受范围不能套用 trace-context 的 strip/忽略规则。

“从 span 拿 metadata”指读取当前 OTel Context；traceparent 不携带任意父 span attributes，五字段经 Baggage
传播。SpanContext 不提供 parent_span_id，后续消费者需要时须在创建边界捕获真实 parent，不能重造关系。
`hook` 是生命周期事件类型，`observed_at` 是 Agent 发生时间；不能用函数 span 名、daemon 接收时间或 RPC
耗时代替。`metrics` 是现有业务 payload 名称，不是 OTel Metrics signal；其模型信息、工具参数/结果等
保留原校验/脱敏语义，不进入 Baggage/OTLP。记录 schema、持久化、配对、乱序处理和链路重组由后续模块定义。

## 10. Runtime 与关闭

main 独占初始化一次真实 SDK provider/subscriber。生产使用固定 `AlwaysOff`，保留有效
TraceId/SpanId、父子关系及 Context/Baggage，snapshot 不依赖 span recording。
本期不提供公开 exporter、采样或 batch 配置；移除 OTLP HTTP client、batch worker 与依赖。
`OTEL_*` export/sampler/batch 环境设置不能开启导出或修改固定采样策略。
`--otel-context` 是入站上下文接口，继续保留，与 exporter 无关。

`RUST_LOG` 仅控制本地 JSON 关联诊断。runtime 另通过同一个有界 worker 输出 daemon
启动警告和运行错误，不受此 filter 控制。库内 reconciliation、JSONL 和 SQLite 诊断统一
使用 `asc_process_diagnostic` target，由同一 subscriber 写入有界队列，不自行初始化 SDK 或线程。
未安装 subscriber 的独立库宿主不输出该诊断；CLI 业务结果不走该 target。初始化失败的 `otel: <reason>` 用独立的临时
worker 尝试输出，最多等待 50 ms；线程创建失败直接丢诊断，没有同步 stderr fallback。
subscriber 冲突仍在接收请求前退出 1。

每进程正常 runtime 只有一个诊断 worker；队列最多 64 条，每条最多 32 KiB。
producer 不等待 stderr；超长、队列满、写失败允许丢诊断。没有按秒限速，接收方管理
rotation。CLI help/usage/业务结果与错误仍同步输出，保留用户输出契约。

生产初始化还将 Rust 默认的同步 panic hook 替换为同一有界 writer，仅输出固定
`runtime: panic`，不记录 panic payload，也不改变 unwind/abort 或业务错误映射。
内部 runtime 子进程测试验证 caught panic 的固定诊断及 payload 隔离。

退出时先结束业务 span，再关闭 provider，剩余时间排空诊断，共用下列预算：

- daemon：停止接收 → service 有界 drain → reconciliation join 最多 30 秒 → 应用 runtime 最多 1 秒
  → event sinks close → 同步 main 的 provider/诊断排空额外最多 2000 ms；全程持有 singleton lease。
- CLI：最终 command/client span 关闭后最多等待 50 ms；保持原业务 exit code。

OTel 不能强停已开始的 blocking 工作，诊断丢失不触发业务重试。
这些是功能预算，不是跨机器性能指标；watchdog 仅检测挂死。

## 11. 直接消费者与验证入口

基础设施、daemon 入站、CLI/client、PAP/compiler 埋点、关联输入适配五个切片已落地。
新增埋点按直接消费者验证父子关系、字段投影和生命周期；新增业务接口仍需自己的兼容 fixtures。
实际测试命令、执行结果、外部兼容边界统一维护于 [验收记录](V2_OTEL_ACCEPTANCE_zh.md)。

## 12. 验收目标

下表保留目标编号与断言；已执行子集及未覆盖组合以验收记录为准，不能仅凭编号宣称全部通过。
动态 ID 比较合法性、唯一性和关联关系，不写死随机值或删除全部 ID 后比较。
SDK 内存 span 检查与真实进程日志测试属于不同证据层；均不等于链路持久化或生产压力验证。

| ID | 场景与必须验证的断言 | 证据层 |
| --- | --- | --- |
| TO-001 | 原生宏/attribute 埋点：bridge 当前 ID/Baggage 一致、processor 投影正确；root→child→恢复；late-bound Context 扩展和显式/contextual parent | SDK 实例 + 内存 exporter |
| TO-002 | 无 parent / 空 carrier / 原始 UDS 无 context：非零 root ID、空 Baggage、不同请求不同 trace | SDK + 真实 UDS |
| TO-003 | 合法 remote parent：trace 相同、SERVER span ID 新生成、parent 正确、flags/tracestate 按标准传播 | carrier fixture + 内存 exporter |
| TO-004 | 只有 Agent Baggage：生成新 root，五字段逐项保留，未知 key 丢弃 | carrier fixture |
| TO-005 | 非法/全零 traceparent、非法 tracestate、超长/坏 Baggage、重复 key/编码/Unicode 边界符合第 5 节 | 正反 fixture |
| TO-006 | JSON 重复 key、类型错误、unknown fields、carrier version 错误仍为 invalid_request，无 PAP 副作用 | protocol + UDS |
| TO-007 | 并发 task/thread 使用屏障交错；各自 trace/Baggage 不串；线程复用、scope 退出、task abort 和 panic 后为空/恢复 | SDK + multi-thread Tokio |
| TO-008 | 正常 child、spawn、spawn_blocking 中完整 Context 一致，含兼容标签与请求 span 引用；跨 await 不持有 guard | SDK + 异步测试 |
| TO-009 | 固定未采样 SDK、RUST_LOG info/off 时可读有效 IDs/Baggage；环境 sampler 设置不改变生产策略 | SDK + 进程 |
| TO-010 | 本地诊断阻塞或队列满不影响请求响应、启动和关闭；不因日志失败重试业务 | writer 单测 + 真实进程 |
| TO-011 | 原始无 context 请求、合法/非法 context 请求的 UID 授权一致；伪造 role/uid Baggage 不能提权 | 真实 UDS peer |
| TO-012 | CLI→daemon 日志共享 trace 和五字段；内部逐层 parent、显式 carrier 隔离、请求不变由 Rust SDK 测试验证 | 双进程日志 + 内存 span 检查 |
| TO-013 | 当前 PAP CRUD/invalid-request goldens、revision、digest、CAS、输出与退出码保持当前基线语义 | 当前仓库测试 |
| TO-014 | busy/read timeout/invalid envelope/unknown method/permission denied 的 span 范围与类别符合第 7 节 | UDS + 内存 exporter |
| TO-015 | dispatch timeout 后 blocking 工作继续：不提前结束业务 span、不声称副作用 cancelled；实际完成才闭合 | 屏障 fake dispatcher + UDS |
| TO-016 | params/error/panic/普通消息/未知 Baggage canary 不进入新增观测诊断；用户输出另按原契约验收 | 本地日志 + 内存 span 检查 |
| TO-017 | SIGTERM、正常退出、stderr 阻塞及残留 blocking task：关闭有界、stdout 不污染 | 真实进程，外层 watchdog |
| TO-018 | new server 接受当前无 carrier 的原生 PAP 请求；client 不因 daemon 错误重发；只支持新 CLI/new daemon，旧 daemon 不作为兼容门禁 | 冻结当前 PAP 协议 fixture + 真实连接；不等于 V1 兼容 |
| TO-019 | [SUPERSEDED] 公开 OTLP wire/export 验收退出本期范围，不保留 mock Collector 测试 | 无当前交付要求 |
| TO-020 | 选定 features/lockfile 在 Rust 1.88 下编译；无重复不兼容 OTel 类型，CLI 无应用 Tokio 要求 | build/dependency evidence |
| TO-021 | 原边界大小请求加 carrier 仍可用；最大 Unicode/转义输入、独立预算、超过任一预算、原始空白计量和并发容量均有界；响应上限不变 | 实际序列化大小 + 真实 UDS + admission 配置 |

| ID | 用户能力与必须验证的断言 | 证据层 |
| --- | --- | --- |
| UF-001 | 现有 trace-context flat JSON、别名、选项位置、等号形式、重复 last-wins、缺值错误/退出码 | V1 CLI oracle + Rust CLI adapter |
| UF-002 | 空值、非 string、snake/camel 优先级、Unicode 空白、256 字符截断；长 Agent/session 不被二次丢弃 | frozen normalization goldens + 实际跨进程往返 |
| UF-003 | 现有 caller 提供的关联输入可由统一 adapter 接收；不要求改成 W3C JSON，业务命令迁移另行验收 | 冻结 caller 输入 + Rust adapter |
| UF-004 | 显式 trace-1 和 invocation 标签在日志/消费者快照中保留；不成为 SDK TraceId；缺省时无独立 invocation 生成器仍能串联 | 诊断输出 + 只读投影测试 |
| UF-005 | CLI 无 context、raw UDS 无 context、只有 Agent 字段三种入口：可用性、授权、输出保持；各请求独立 root | V1 fixture + 真实 UDS 进程 |
| UF-006 | 同一 trace 多请求、每请求多子 span、并发与线程复用：未采样且无 Collector 时每条日志仍有正确请求关联；记录 ID 替代映射 | 交错执行 + 捕获诊断输出 |
| UF-010 | stdout/退出码、既有错误与日志访问入口、真实 hook timeout/关闭预算不回归；PAP requestId 过渡符合公开 schema | 真实进程 + 实际 caller + protocol fixtures |
| UF-011 | caller 原 metadata 输入保持可用，按 hook 校验后替换记录字段；缺失/null 不被父值补齐，可选字段清除；消费者免传 metadata 参数，统一 Context 跨 carrier/task 后一致，未采样仍可读取 | V1 schema goldens + Context 投影/校验适配测试；实际 record RPC 另行验收 |

UF-007/008/009/012 的持久化、查询和重组验收不在当前工作包，编号保留不复用。
当前 UF 输入适配测试不能替代真实 hook、record RPC、历史查询或安全事件落盘验收。

## 13. 契约与变更记录

架构约束见 [Rust 迁移架构](AGENT_SEC_RUST_MIGRATION_zh.md) 和
[执行架构](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md)；原生 carrier、严格 schema、帧预算见
[daemon 协议](DAEMON_PROTOCOL_V1_zh.md)，provider/日志/关闭见
[进程部署契约](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md)。
[PAP 验收](PAP_DAEMON_API_ACCEPTANCE_zh.md) 与 [CLI 验收](POLICY_CLI_ACCEPTANCE_zh.md)
继续约束业务响应、输出和授权；安全事件接入还需遵守 [中间件契约](SECURITY_MIDDLEWARE_CONTRACT_zh.md)。

OTEL-CR-001…008 的已采纳决策、直接消费者证据及部署/回滚顺序统一记录于
[OTel 验收记录](V2_OTEL_ACCEPTANCE_zh.md)，不在设计中维护第二份同编号清单。
本次不改变持久化 schema，不修改 report/review 或 security-observability skill 的查询契约。

## 14. 后续接入

Action Runtime 的 security.invoke/capability、SecurityEvent sink、实际 observability RPC、
历史 reader 和本地链路重组由各自工作包实现。AgentSight client 的 HTTP 边界仍需单独接入 carrier，
并验证远端提取，才能声明跨服务 trace 连通。Reconciler/Job 使用独立 run span，明确 parent/link、取消、
关闭与重启行为，不延长整个 request scope，也不建立 daemon 全生命周期巨型 span。
