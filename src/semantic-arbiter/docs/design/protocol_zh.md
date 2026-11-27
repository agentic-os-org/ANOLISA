# 语义判断协议

[English](protocol.md)

## 范围与职责

`semantic-arbiter` 对冻结的任务、候选和证据输入求值，返回建议性判定。消费方负责输入认证、不可变身份/版本绑定、授权和执行验证。

| 入口 | 协议 / 模式 | 求值方式 |
|---|---|---|
| `check` | `semantic-arbiter/v1alpha1` / `deterministic_check` | 在记录的 `check_at_ms` 执行确定性约束和证据检查 |
| `evaluate` | `semantic-arbiter/v1alpha1` / `model_evaluation` | 在实时截止期限内应用完整性/资源硬门禁、本地快照策略与语义回退 |
| `describe` | 能力声明 | 支持的操作、协议版本、模式、场景、后端、候选控制、obligation 类型和消息大小上限 |

两种操作均使用内联冻结证据。`check` 不执行外部 I/O，也不重放 Agent 或其动作；模型求值将选定输入发送给配置的后端。两者都不采集文件系统事实或执行 Directive。调用者可选择 `preservation`、`task_scope` 和 `snapshot` 的任意非空、不重复子集。当前 `check` 生产方输出候选 `allow/block`；当前 `evaluate` 生产方输出 `nop/allow/block`。所有操作及未来场景的完整 CandidateControl 接口始终为 `nop < allow < ask < block`。

公共库已实现显式 EvidenceSource 采集、封闭场景 JudgmentUnit 替换、确定性 Outcome 组合和消费方侧 executor 契约。CLI 保持使用内置单元、固定组合、上述行为和协议版本。生产证据源、真实 Executor、原生 Adapter 集成、生产级持久 claim 与审计，以及效果核验仍待消费方集成。

公共定义位于 [`contract.rs`](../../src/contract.rs)、[`evidence.rs`](../../src/evidence.rs)、[`config.rs`](../../src/config.rs)、[`judgment.rs`](../../src/judgment.rs)、[`evaluation.rs`](../../src/evaluation.rs)、[`composition.rs`](../../src/composition.rs)、[`executor.rs`](../../src/executor.rs)、[`outcome.rs`](../../src/outcome.rs) 和 [`protocol.rs`](../../src/protocol.rs)。使用公共解码和校验入口执行语义约束。

## 请求字段

### 信封、绑定和任务

| 字段 | 含义与约束 |
|---|---|
| `api_version`、`mode` | 必须使用上表中精确的协议/模式组合 |
| `request_id` | 消费方拥有的不可变请求身份，响应和回执原样携带 |
| `binding.session_id` | 任务和候选所属会话 |
| `binding.instance_id` | 稳定的进程/会话实例身份；消费方须独立解析和认证 |
| `binding.workspace_id` | 消费方核验过的不透明工作区身份 |
| `binding.task_version` | 独立于历史记录留存的当前任务版本 |
| `binding.config_revision` | 消费方策略版本；模型配置必须携带相同值 |
| `binding.candidate_id`、`binding.candidate_revision` | 冻结候选的身份和版本；候选输入变化时必须更新版本 |
| `scenarios` | `preservation`、`task_scope`、`snapshot` 的非空、不重复子集；Rust 变体为 `Scenario::Preservation`、`Scenario::TaskScope` 和 `Scenario::Snapshot` |
| `task` | 权威任务投影；`null` 产生原因码为 `missing_task` 的未知答案 |
| `task.session_id`、`task.version` | 必须等于 `binding.session_id` 和 `binding.task_version` |
| `task.origin` | `operator` 或 `native_user`；消费方必须认证入口 |
| `task.text` | 非空白任务意图，最多 16384 个 UTF-8 字节；仅模型求值解释此文本 |
| `task.allowed_resource_ids` | 可选的资源上限；非空时未列出的资源即超出范围，空列表表示未声明上限 |
| `task.preserve_resource_ids` | 可选的保留要求，在任何单元之前检查；空列表表示未声明保留要求 |
| `candidate.changes` | 0–64 项变更，每项为 `{operation, resource_id}`：在绑定工作区内对不透明资源身份执行 `replace` 或 `delete`。调用影响的每个资源按消费方顺序出现一次；`resource_id` 重复返回 `invalid_input`。空列表是完全归因只读调用的诚实投影：不指向任何资源，因此保留与允许约束均不适用 |
| `candidate.native_tool` | 可选完整原生调用，仅包含 `name` 和 `input`；缺失与 `null` 等价 |
| `candidate.native_tool.name` | 非空白原生工具名称，最多 128 个 UTF-8 字节 |
| `candidate.native_tool.input` | 完整 JSON 参数对象，紧凑序列化后最多 65536 个 UTF-8 字节，不截断或仅提取命令 |

绑定必须保持不可变。每条证据的绑定必须与请求绑定完全相同。任务文本、来源报告和模型内容不能扩大结构化资源上限或授予执行权限。

存在原生数据时，两种协议及 EvidenceSource 采集都要求 `binding.candidate_revision` 精确等于 `contract::candidate_revision(&candidate)`。该函数校验候选，返回 `sha256:` 加 64 位小写十六进制字符。SHA-256 的输入为 `b"semantic-arbiter\0candidate\0"` 后接完整候选的紧凑 JSON：递归排序全部对象键（包括数组内对象），保留数组顺序，标量使用 serde_json 编码。任一操作、资源、变更顺序、原生名称或嵌套参数的变化都要求重新计算 revision 并重新绑定证据。不匹配时返回 `binding_mismatch`，而不是判断结果。名称非法或 input 非对象返回 `invalid_input`；input 超出字节上限返回 `message_too_large`。外层 262144 字节消息上限及严格 JSON 递归上限仍然适用。

例如候选可携带 `"native_tool":{"name":"Bash","input":{"command":"printf test","cwd":"/workspace"}}`；其中字符串属于数据，不是可执行指令或文件系统事实。完整原生数据原样进入模型 state。没有原生数据时，序列化省略该字段，调用方自定 revision 标签仍被接受。这种内容绑定不认证原生调用，也不授权执行。

### 证据、缺口和预算

| 字段 | 含义与约束 |
|---|---|
| `evidence[].id` | 答案和 Outcome 引用的唯一证据身份 |
| `evidence[].source_id` | 消费方提供的来源身份 |
| `evidence[].provenance` | `local_verifier`、`source_reported` 或 `model_echo`；仅 `local_verifier` 可用于重建检查 |
| `evidence[].binding` | 与请求的任务、候选、实例、工作区和策略精确关联 |
| `evidence[].captured_at_ms`、`valid_until_ms` | 采集时间和不包含端点的失效时间；采集必须早于失效 |
| `evidence[].fact.kind` | `rebuild` |
| `evidence[].fact.parameters.resource_id` | 验证器比较的资源 |
| `evidence[].fact.parameters.result` | `identical`：留存配方重现了原字节；`unique`：现有字节包含未重现的成果；`absent`：该资源当前没有字节；`unknown`：比较无定论 |
| `gaps[].source_id`、`scenario` | 某来源和场景的显式覆盖缺失；重复组合非法。启用缺口阻断 `check`，但在满足条件的 `evaluate` 回退中保持可见 |
| `budget.opened_at_ms`、`deadline_ms` | 原始事件开始时间和不包含端点的截止时间；截止必须晚于开始，求值时间不能早于开始 |
| `budget.max_evidence_items`、`max_evidence_bytes` | 调用者对证据条数及紧凑序列化证据数组字节数的限制，不能超过协议硬上限 |
| `check_at_ms` | 仅 `check` 使用，表示夹具记录时钟中的求值时间；模型请求无此字段 |

来源标签属于声明。消费方必须独立认证任务入口、证据来源和验证器结果。合成夹具仅声明这些事实。`source_reported` 和 `model_echo` 不能证明本地重建结果。

`check` 使用记录时钟。模型请求的预算和证据时间戳使用 Unix 毫秒。证据仅在 `captured_at_ms <= evaluation_time < valid_until_ms` 时可用。在 `check` 中，事件预算过期、超出调用者证据限制或其他确定性 unknown 均 fail-closed 为候选 `block`。时间字段非法、硬上限超限和绑定不匹配属于协议错误。

### 求值前证据采集

`EvidenceSourceRegistry` 是可选的生产侧库接口，不属于任一线协议。消费方在最终确定 check 或 evaluation 请求前显式注册进程内来源并选择 source ID。采集接收不含 evidence 和 gaps 的冻结请求上下文。注册表注入已注册的来源身份、provenance 和请求 binding；来源输出不能替换这些字段。结果包含确定排序的内联 `evidence`、`gaps`、脱敏的逐来源报告和一个最终 `collected_at_ms`。

每个协作式异步来源只在来源超时与原事件截止时间的较早者之前尝试一次。来源实现必须立即返回 future，且不得阻塞执行器线程；Tokio deadline 无法抢占进程内阻塞代码。不重试、不回退、无隐式来源、无插件发现，也不续期。来源不可用、超时，或返回未来、过期、超龄批次时，为已选择且来源支持的场景生成显式 gap。最终冻结时点会复检全部批次，后续来源消耗的时间不能让先前已经过期的证据进入结果。来源输出非法、证据 ID 重复，或超出条数/紧凑数组字节预算时，整次采集失败且不截断。

采集不认证来源，也不提升 provenance。内存示例保持 `source_reported`，其重建声明不能满足本地验证器要求。消费方把返回的证据和缺口填入请求；确定性请求还要将 `check_at_ms` 设为返回的 `collected_at_ms`。`check` 与 `evaluate` 自身不会调用注册表。

### 本地上下文（仅 `evaluate`）

可选 `local_context` 是冻结的消费方观察，不是策略配置或认证声明。缺失时省略，存在时进入 `input_digest`。`check` 不接受或使用它。

| 字段 | 约束与含义 |
|---|---|
| `candidate_revision` | 必须等于请求绑定 |
| `captured_at_ms`、`valid_until_ms` | 有序时间戳；仅在 `captured_at_ms <= now < valid_until_ms` 时可用 |
| `files` | 每个候选资源最多一项观察，不能引用其他资源 ID |
| `files[].resource_id`、`state` | 候选资源及 `absent`、`regular`、`directory`、`symlink` 或 `unknown` |
| `files[].delta` | `unchanged`、`changed` 或 `unknown`；已知 delta 要求普通文件和摘要 |
| `files[].existed_at_start` | 布尔值或 null；覆盖不完整不能推断不存在 |
| `files[].sha256` | null 或 `sha256:` 加 64 位十六进制，仅适用于普通文件 |
| `signals` | 最多 64 项 `{kind, resource_id}` 线索；资源 ID 为 null 或指向候选资源 |
| `gaps` | 最多 32 个符合标识符格式的稳定覆盖码 |

封闭动作词汇定义于 [`local_policy.rs`](../../src/local_policy.rs)。信号描述测试/配置/依赖变更、外部通信、凭据位置读取（`sensitive_read`）与工作树丢弃（`working_state_destruction`）等词法类别，不证明授权、恶意、共享生产用途或实际副作用。核心不采集文件系统信息。[Provider 采集器](../../../../providers/semantic-arbiter/README_zh.md#本地观察与判断) 负责有界清单/stat/原始字节观察；排除项和上限保持显式不确定性。

### 线格式上限

| 上限 | 值 |
|---|---|
| 契约消息 | 262144 个 UTF-8 字节 |
| 候选变更、证据条目、缺口记录、每个 ID 列表、obligation | 分别最多 64；完全归因的只读调用候选变更可为空 |
| 主体动作 | `freeze`、`quarantine`、`kill` 各最多一项 |
| 标识符 | 1～128 个 ASCII 字节：字母、数字、`.`、`_`、`-`、`:`、`@` |
| 单元置信度 | 整数 `0..=1000000` |

契约对象封闭；拒绝未知字段、重复字段、未知枚举值和尾随 JSON。原生 `input` 的字段名是例外：保留任意参数键，但在解码为 JSON 值之前拒绝任意深度的重复键，包括数组内对象及转义后相同的键。无符号整数字段拒绝负值和小数。带标签的事实与动作使用 `kind` 和 `parameters`。两种请求协议、配置和上游响应的解码都会拒绝所有层级的重复键，包括被忽略的响应元数据。输入 JSON 的排版不影响摘要所用的强类型序列化。

## 后端配置

配置是独立的受信输入。证据不能选择后端、端点、凭证变量或阈值。

| 字段 | 含义与默认值 |
|---|---|
| `config_revision` | 与 `binding.config_revision` 匹配的策略标签；策略变化时调用者必须更新此值 |
| `backend.kind` | `bailian` 或 `typesafe`；夹具需显式进程内注入 |
| `backend.endpoint_url` | 百炼必填的完整推理 URL，原样使用 |
| `backend.base_url` | TypeSafe 基础 URL，默认 `https://api.typesafe.ai`；去除尾斜线后追加 `/v1/systemone` |
| `backend.model` | 配置的名称/别名；百炼默认 `decision-model-preview`，TypeSafe 默认 `jev-latest` |
| `backend.key_env` | 凭证环境变量名称；默认 `DASHSCOPE_API_KEY` 或 `TYPESAFE_API_KEY` |
| `timeout_ms` | 必填整数 `1..=120000`；从入口起限制准备和求值时间，只能缩短原始截止期限 |
| `max_response_bytes` | 必填整数 `1..=262144`；在读取上游响应正文时执行限制 |
| `thresholds.preservation_ppm`、`thresholds.task_scope_ppm`、`thresholds.snapshot_ppm` | 独立整数支持度阈值，范围 `500001..=1000000`，默认均为 `900000` |
| `transport` | 默认 `https`；`loopback_test` 仅允许字面量回环 IP 地址上的 HTTP |

百炼样例使用 `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone`，并显式将三个阈值设为 `800000`。使用前填入已获授权的工作区 ID 并核验完整端点。这些阈值属于未经校准的决策参数；模型准确率需要单独测量。

URL 拒绝内嵌凭证、查询参数和片段。HTTPS 使用证书校验。HTTP 适配器配置为禁用重定向、环境代理、重试和空闲连接池。`EnvironmentBackend` 仅在需要推理时加载配置的环境变量，不加载 `.env`。输出排除密钥和原始上游响应正文。

## 判断规则与模型交互

每个单元要求任务存在，并对每项变更应用已声明的保留与允许约束。资源列表为空表示未声明约束；受保留资源或超出非空允许列表的资源会阻断候选。`check` 行为不变：保留意图与快照要求已确立的本地验证器重建事实，证据缺失、无定论或存在缺口均按失败关闭处理。

`evaluate` 区分完整性失败与知识缺失。基线原因 `allowed`、`missing_evidence`、`evidence_gap` 和 `undetermined` 允许本地策略或语义回退。即使前面的资源存在缺口，保留意图与快照仍扫描每个变更资源，检查重建证据是否过期、不可信或冲突；这些失败仍是硬门禁。绑定/格式错误仍是协议错误。任务范围不需要重建证据，但其声明的覆盖缺口会保留给语义回退。缺失事实绝不会转化为 `absent`、许可或可重现性证明。

快照与保留意图有内置本地策略，在单元内按下列顺序尝试。只读策略共享同一个完整性事实：候选不含任何变更、存在 `read_only` 信号且不存在 `opaque_execution` 信号。任务范围没有本地策略：它问调用是否服务任务目标，任何效果事实都无法回答，因此无关或越权的读取即使没有写效果、不触及凭据位置也属越界。读取一律交模型；`read_only` 与 `sensitive_read` 是该语义判断的线索，不是本地门禁。

| 单元 | `policy_id` | 必需事实 | 答案 |
|---|---|---|---|
| snapshot | `no_loss_read_only` | 只读完整性事实 | `allow` |
| snapshot | `no_current_bytes_lost` | 新鲜观察覆盖全部变更，每个文件当前不存在或其受支持字面编辑产生完全相同的字节，且没有 `opaque_execution` 信号 | `allow` |
| snapshot | `protect_unique_bytes` | 引用的合格重建证据包含 `unique` | `needs_snapshot` |
| snapshot | `protect_preexisting_bytes` | 新鲜观察确认普通文件在初始清单中存在，具有稳定原始字节摘要，未证实字节不变，且没有引用的 `identical` 重建事实 | `needs_snapshot` |
| snapshot | `protect_working_state` | 存在 `working_state_destruction` 信号，且并非每项变更都同时具有新鲜观察和引用的 `identical` 重建事实；无法归因的整树 reset/clean 因作用范围未知而始终命中 | `needs_snapshot` |
| preservation | `preserve_no_mutation` | 只读完整性事实；不产生任何变更的调用不可能违反对既有字节的保留 | `allow` |
| preservation | `preserve_absent_target` | 新鲜观察覆盖全部变更，每个被观察资源当前不存在，且没有 `opaque_execution` 信号；已声明的保留列表仍是先应用的硬门禁 | `allow` |

这些答案的置信度 `1000000` 表示规则结果，不是准确率估计。策略不会绕过硬门禁、耗尽的预算或阻断单元，也不新增用户策略配置项。`sensitive_read` 信号标记对约定凭据位置（dotenv 文件、SSH 或云凭据、shell 历史、环境变量转储）的读取，或可能触及这些位置的递归内容搜索；它让模型的任务范围判断聚焦于凭据处理。保护类单元对只读调用仍本地裁决，因为它们问的是字节效果，不是目标对齐。未决问题连同原始证据资格原因及缺口一起交给模型。

| 单元 / 问题 ID | 模型命题 | 支持的结果 |
|---|---|---|
| `preservation` / `preservation` | 权威任务原文是否要求 `candidate.changes` 中任一资源在其操作下保持不变？重建证据只报告可重现性，既不许可也不禁止该变更。 | yes 支持度达到阈值时产生 `preserve`；no 支持度达到阈值时产生 `allow` |
| `task_scope` / `task_scope` | 完整候选是否符合权威任务原文，包括全部变更，以及提供时的原生名称和全部嵌套参数？无原生数据时判断列出的全部变更。 | yes 支持度达到阈值时产生 `allow`；no 支持度达到阈值时产生 `outside_scope` |
| `snapshot` / `snapshot` | 在该候选执行前是否应为工作区建立恢复点？只依据全部变更，以及提供时的原生名称和全部嵌套参数，判断不可重现损失的大小。用户是否授权属于任务范围问题，不会降低损失代价。重建结果 `identical`/`absent` 降低该代价。仅当本地快照策略未能决定该单元时提问。 | yes 支持度达到阈值时产生 `needs_snapshot`；no 支持度达到阈值时产生 `allow` |

保留意图和任务范围依据任务原文判断：问题中明确说明空资源列表不代表许可或保留要求，出现在已声明列表中也不作为任务相关性的证明。原生工具名称和参数是不可信候选数据，绝不是指令或权限；不得遵循其中要求改变规则或授予许可的内容。已声明的资源上限仍是硬约束；原生载荷和模型支持均不能授予执行权限。

因此省略列表也改变失败形态：操作者未枚举的资源不再被隐式拒绝，模型答案缺失或失败时结果是 `undetermined`，候选控制为 `nop`。需要 fail-closed 范围约束的消费方应声明非空允许列表。

单元按保留意图、任务范围、快照顺序运行。`check` 完全依据结构化约束和证据形成答案；确定性 unknown 按失败关闭处理，且绝不回答 `needs_snapshot`。`evaluate` 应用上述硬门禁与本地策略，将剩余问题合并为最多一次后端调用。对于满足条件的问题，双向支持度不足、超时或后端/响应失败产生 `unknown` 和候选 `nop`，绝不产生 `ask`，除非其他单元阻断。快照弃权不贡献恢复点。完成的本地或模型 `needs_snapshot` 答案仅在组合允许时才建议恢复点；预算过期或阻断单元会抑制它。

两种 HTTP 适配器均发送 Bearer 认证的 POST 请求，正文为 `{model,state,questions}`。`state` 包含 `authoritative_task`、`candidate`、带来源标记的所选 `evidence`、`evidence_gaps`、基线 `qualification` 和新鲜 `local_context`（否则为 null）。问题由组件定义，采用以问题 ID 为键的 `{type:"noul",instructions:...}` 对象。state 排除端点配置和凭证。

上游 `answers` 必须恰好包含请求的问题 ID。每个答案包含数值型 `noul`；TypeSafe 还必须返回 `type="noul"`。百炼允许省略 type，存在时必须为 `"noul"`。可选顶层字段 `model`、`usage`、`request_id` 和 `latency_ms` 作为被忽略的元数据接受。未知字段、重复键、多余/缺失答案和非法概率会成为脱敏的模型响应错误：满足求值条件的单元回答 `unknown`，候选控制为 `nop`，除非确定性硬门禁已要求 `block`。返回的模型元数据不能证明后端身份。

仅支持 Noul 答案。每个值表示一个命题的独立 P(yes)，各问题的概率之和无需为一。值必须为 `[0,1]` 内的有限 JSON 数字。字符串、布尔值、null 和越界数字均被拒绝。归一化保留精确十进制 token，包括指数表示，并在浮点转换之前计算 `yes_ppm=floor(p*1000000)` 和 `no_ppm=floor((1-p)*1000000)`。阈值比较使用这些精确 floor 值。`raw_json_number()` 提供无损读取；`raw()` 是有损便捷接口。概率和单元置信度表示模型支持度，尚未建立其与事实正确率或安全性的校准关系。

### 判断单元注册表

`JudgmentUnitRegistry` 是三个封闭 `Scenario` 值的显式进程内替换点。构造函数要求恰好一个 `preservation` 单元、一个 `task_scope` 单元和一个 `snapshot` 单元；问题 ID 必须非空且互不相同。注册表不能增加任意线协议场景，也不执行插件或动态代码发现。消费方替换一个单元时，需要为其余槽位提供内置实现或另一项经过审核的实现。

`Evaluator::with_units` 在准备问题和推导响应时使用传入的注册表。各单元的问题 ID 和问题原文均进入 `config_digest`。求值稳定后，`EvaluationRecord` 冻结期望响应和原始配置摘要；后续响应复核比较这些不可变值，不会再次调用判断单元或模型。中央答案校验要求保持原单元和证据引用，将置信度限制在 `0..=1000000`，并且只允许对应封闭场景所定义的 value/reason 组合。自定义单元返回其他答案时产生 `invalid_response`。

## 响应字段与校验

### 公共响应和单元答案

| 字段 | 含义 |
|---|---|
| `api_version`、`mode`、`request_id`、`binding` | 原协议、求值模式和不可变输入关联 |
| `input_digest` | 使用域分隔的 SHA-256，绑定完整强类型请求内容 |
| `answers[].unit` | 启用场景；每个启用单元恰好一个答案 |
| `answers[].value` | 单元结论：`allow`、`preserve`、`needs_snapshot`、`outside_scope` 或 `unknown` |
| `answers[].confidence_ppm` | 模型派生答案所选命题的支持度；确定性已知答案为 `1000000`，unknown 为 `0` |
| `answers[].reason` | 稳定解释码，例如 `allowed`、`preserved`、`needs_snapshot`、`missing_evidence` 或 `budget_exceeded` |
| `answers[].evidence_refs` | 从本请求冻结证据中本地选定的 ID |
| `outcome` | 候选/主体控制、有序 obligation 和证据引用 |

单元答案描述该单元的结论，它本身不是 CandidateControl。候选处置由汇总后的 `outcome.control.candidate` 表达。对于 `check`，任一 `preserve`、`outside_scope` 或确定性 `unknown` 答案产生 `block/[audit]`，其余情况产生 `allow/[audit]`；`check` 绝不输出 `snapshot`。对于 `evaluate`，确定性硬阻断保持 `block/[audit]`；满足模型求值条件的问题因双向支持度不足或故障而弃权时，答案为 `unknown`，当前生产方返回候选 `nop/[audit]`。完整且无阻断的本地或模型判定产生 `allow`；快照单元回答 `needs_snapshot` 时产生 `allow/[snapshot,audit]`。`preserve` 或 `outside_scope` 答案阻断候选并抑制快照建议，因此任务要求保留的资源会被阻断，而不是先快照再修改。确定性置信度 `1000000` 表示规则在声明输入下已完成判定，不提供来源认证。

`allow` 属于建议。附带 `snapshot` 时，消费方必须保持候选暂停，独立授权并确认快照成功，在执行前重新校验绑定、证据和截止期限。推理成功、审批通过或响应送达均不能替代执行授权。

### 模型求值响应的附加字段

| 字段 | 含义 |
|---|---|
| `config_digest` | 非密配置及问题 ID 和问题原文的摘要；与调用者的 `config_revision` 标签分别使用 |
| `backend.kind`、`backend.mode`、`backend.model` | 已构造适配器的类型（`bailian`、`typesafe`、`fixture`）、模式（`remote`、`loopback_test`、`fixture`）和配置的模型名称/别名 |
| `evaluated_at_ms` | Unix 毫秒表示的最终有效性检查时间 |
| `elapsed_ms` | 求值完成时的单调流逝时间，包含输入准备 |
| `model_called` | 是否调用了所选后端；没有满足条件的问题被调用时为 false |
| `judgments[].unit`、`question_id` | 单元身份，以及绑定到问题原文的问题身份 |
| `judgments[].threshold_ppm` | 该单元配置的支持度阈值 |
| `judgments[].probability` | 已校验的 `{raw,yes_ppm,no_ppm}`，不可用时为 null |
| `judgments[].error` | 脱敏后端错误或 null；例如 `timeout`、`authentication`、`rate_limited`、`transport`、`http`、`invalid_response`、`response_too_large` |
| `judgments[].model_used` | 已校验概率参与最终单元答案的推导；过期或确定性拒绝后为 false |
| `judgments[].evidence_refs` | 该单元本地选定的证据引用 |
| `judgments[].route` | `hard_gate`、`local_policy`、`model_fallback`、`observation_expired` 或 `budget_exhausted` |
| `judgments[].policy_id` | 稳定的本地策略 ID，或 null |
| `judgments[].baseline_reason` | 语义归一化前保留的证据资格原因 |

CLI 在入口捕获墙钟与单调时钟。准备、推理、正文读取、校验和组合共享原始 deadline 与配置上限。推理/组合后复核证据和观察。初始新鲜的观察过期后，依赖它的本地/模型判断不被采用（`observation_expired`）；合格的 unique 重建证据仍可独立决定快照。满足条件的路径预算耗尽时产生 `unknown`/`nop`，除非有硬阻断。概率可以保留但 `model_used=false`。`model_called` 记录后端调用，不表示成功发送 HTTP：惰性凭据读取失败时，即使没有网络请求也可为 true。

独立留存 `EvaluationRun::record()`。`evaluation::decode_response` 和 `validate_response` 要求原始请求、配置，以及不可反序列化构造的本地 `EvaluationRecord`，并与冻结的期望响应比较，不会再次调用判断单元或模型。后端注入属于受信进程内 API；调用者必须将实际适配器与其配置绑定。线上的 trace 和后端标签本身不能提供跨不可信进程边界的认证。记录校验确认已记录结果的一致性；执行前消费方必须再次检查时效。`check` 响应校验依据留存请求重算确定性结果。

### 摘要

摘要为 `sha256:` 加小写 SHA-256，计算输入为域分隔字节和按声明顺序紧凑序列化的强类型 JSON。数组、枚举值和 null 均保留。跨语言实现必须匹配此序列化及仓库的摘要向量；对输入空白格式直接计算哈希或采用 RFC 8785 均不符合本契约。

| 摘要 | 域分隔符 | 强类型载荷 |
|---|---|---|
| `check` 的 `input_digest` | `semantic-arbiter\0check-input\0` | `CheckRequest` |
| 求值 `input_digest` | `semantic-arbiter\0evaluation-input\0` | `EvaluationRequest` |
| 求值 `config_digest` | `semantic-arbiter\0evaluation-config\0` | 配置与按场景排序的场景、问题 ID 和问题定义组成的二元组 |
| `ModelConfig::digest()` | `semantic-arbiter\0model-config\0` | 仅配置；与响应的 `config_digest` 分别计算 |

`\0` 表示一个零字节。`config_revision` 是调用者维护的策略标签；`config_digest` 绑定实际配置和问题语义。这些载荷不包含凭证值。摘要绑定内容，不提供发送方认证。

## Outcome、Directive 和回执字段

### 确定性组合

`composition::compose_outcome` 在不授权或执行的情况下组合独立形成的 `OutcomeContribution` 值。候选控制取 `nop < allow < ask < block` 下的最大值。主体控制形成有界并集，`freeze`、`quarantine` 和 `kill` 各自最多保留一项内容完全相同的请求。obligation 按 ID 精确去重，证据引用形成有序集合。

组合会拒绝不可用的细粒度能力、工作区或实例绑定不匹配、空受控标识符、同一 Directive ID 或主体动作下的内容冲突，以及与保留 ID `candidate-approval` 的冲突。结果包含恰好一个 audit obligation；未提供 audit 且已声明 audit 能力时，组合会加入规范的 critical `audit` obligation，多个不同的 audit 请求会失败。主体按 freeze、quarantine、kill 排序。obligation 按动作类别再按 ID 排序，audit 位于末尾；证据引用按字典序排序。这些规则使输出不受贡献输入顺序影响。当前 CLI 继续使用固定的内置组合路径。

| 字段 | 含义 |
|---|---|
| `outcome.control.candidate` | 限制顺序 `nop < allow < ask < block`：未形成判定、无异议、等待审批、原生拒绝 |
| `outcome.control.subject[]` | 独立主体请求，各含 `id`、绑定的 `instance_id`、`action`（`freeze`、`quarantine`、`kill`）和非空 `scope_ids` |
| `outcome.obligations[]` | 有序建议，各含唯一 `id`、`critical` 和强类型 `action` |
| `outcome.evidence_refs` | 建议采用的证据 ID |
| `Directive.kind`、`parameters` | `approval`、`subject` 或 `obligation` 及其强类型正文 |
| Approval `parameters.id`、`candidate_id` | 保留 ID `candidate-approval` 和等待审批的绑定候选 |
| Subject / obligation `parameters` | 原始主体请求或 obligation，保留不可变 `id` 和动作参数 |

所有操作和未来场景均使用完整 CandidateControl 契约，顺序为 `nop < allow < ask < block`。当前 `check` 生产方输出 `allow/block`；当前 `evaluate` 生产方输出 `nop/allow/block`。两者的主体控制均为空，obligation 仅有 `snapshot/audit`。`ask` 表示确定性的“需要审批”决策；歧义和 `unknown` 答案都不会产生 `ask`。其类型、校验和 Approval Directive 投影现已预留。当前成果保护/任务范围生产方不会输出，真实审批执行与 Adapter 准入仍待消费方集成。更广的词汇覆盖未来增强/审批、安全、主体控制和清理场景。公共确定性组合已经可用；执行调度、真实动作实现和原生采用仍由消费方负责。`Outcome::directives` 投影 ask、主体请求和 obligation。消费方通过原生接口阻断候选。主体请求保持独立；等待中的 kill 不吸收 freeze 或 quarantine。

| Obligation `action.kind` | `action.parameters` |
|---|---|
| `audit` | 空对象；脱敏判断和执行元数据 |
| `snapshot` | 绑定的 `workspace_id` |
| `rollback` | 绑定的 `workspace_id`、已授权的 `checkpoint_id` |
| `cleanup` | 绑定的 `workspace_id`、非空 `checkpoint_ids` |
| `notify`、`diagnose` | 已注册的 `recipient_id` 或 `diagnostic_id` |
| `retrieve_skill`、`retrieve_memory` | 已注册的 `query_id` |
| `inject_context` | 非空、已审核的 `material_ids` |

同一 Outcome 内的 Directive ID 唯一；`candidate-approval` 为保留值。每个非空 Outcome 必须恰好包含一个 audit，包括候选为 `nop` 的纯主体控制。证据引用或启用场景的缺口也要求审计。audit、snapshot、rollback 和 cleanup 始终使用 fail-closed 策略；`critical=true` 将此策略应用于增强动作。ID 需要独立解析、范围检查和授权。rollback 要求消费方确认工作区静默、重新检查状态并取得显式确认。

| 回执字段 | 含义 |
|---|---|
| `api_version` | `semantic-arbiter/v1alpha1` |
| `request_id`、`binding` | 精确关联留存请求 |
| `directive_id` | 按 `(request_id,binding,directive_id)` 留存的不可变 Directive ID |
| `state`、`reason` | 仅接受 `applied/confirmed`、`rejected/denied`、`unknown/unconfirmed` |

消费方独立完成准入和效果核验后生成回执。`decode_receipt` 接收 `CheckRequest` 和留存 Outcome，检查关联及状态/原因一致性。该 API 不提供模型求值回执解码器或操作系统效果核验。消费方须另行核验能力、身份、范围、授权、幂等和截止期限。答案 `unknown`、候选 `nop`、候选 `ask` 和回执 `unknown` 是四种不同状态，分别表示语义支持不足、未形成候选判断、确定性要求审批，以及执行效果未确认。回执为 `unknown` 时，查询原回执或人工检查效果；禁止自动重复 Directive。拒绝同样不授予重试权限。两种操作均不生成执行回执。

### 消费方侧 executor 生命周期

`executor` 模块属于消费方侧库契约。`AuthorizationContext` 是消费方创建的受信输入；证据、模型输出和 Directive 都不能创建或扩大该上下文。准入要求使用协议版本 `semantic-arbiter/v1alpha1`、精确匹配细粒度 `DirectiveCapability`、截止期限尚未到期、候选/工作区/实例绑定匹配，并且 allowlist 包含每个非空受控范围或参数 ID。`ExecutorRegistry` 为每项能力注册恰好一个 Executor。

`idempotency_key` 是 `api_version`、`request_id`、完整 `Binding` 和完整不可变 `Directive` 的稳定域分隔摘要。执行顺序为 claim、条件分发和查询。新 claim 最多分发一次，随后查询效果。已有 claim 跳过分发并查询原请求。分发结果不会决定回执；只有独立查询可映射为 `applied/confirmed` 或 `rejected/denied`。查询失败或结果未确认时映射为 `unknown/unconfirmed`，未知效果绝不自动重放。

`executor::fixture` 提供仅供测试和示例使用的内存 claim store 与脚本化 Executor。它不执行外部动作，也不能证明原生 Adapter 支持。真实 Executor、原生 Adapter 采用、生产级持久 claim、持久审计和操作系统效果核验仍待消费方集成。

## CLI 完成语义与接入边界

完整合法判定退出 `0`，包括模型歧义、推理失败或凭据缺失/不可用导致的 `unknown`/`nop`，以及任何合法 `block`。`EnvironmentBackend` 先验证配置，但仅在推理内读取凭据；纯本地求值不需要凭据。缺少凭据在 judgment trace 中表示为 `missing_credentials`，不是 CLI 构造错误。硬阻断仍优先。请求或配置非法/缺失、绑定非法和输入/I/O 失败退出 `2`，stderr 输出脱敏 JSON。允许结果输出受原始 deadline、配置上限、采用证据及所依赖本地上下文的有效期限制；过期时以 `deadline_exceeded` 退出 `2`。须丢弃部分 stdout。仅接受完整零退出响应，检查候选控制并在使用前复核；零不表示许可。

独立的 [AW 旁路 Provider](../../../../providers/semantic-arbiter/README_zh.md) 使用 `aw-provider/v1alpha1`；本 CLI 不能充当该 Provider。私有配置包含资源约束，不含任务文本或预设会话。`input.submit` 将每个会话的 prompt 和有界初始清单持久化到 `task.json`；`tool.before` 对照已采集轮次判断完整原生调用。会话身份对 `[adapter, binding_id, instance_id, native_session_id]` 哈希；连接的任务文本决定 `task_version`。上下文非法、任务缺失/超限、候选非法/超限仍为 uncovered。未映射或 `no_mutation` 的对象输入保留完整调用作为不透明候选，不再跳过。

Provider 在原始 deadline 内冻结本地上下文与 Git 证据/缺口，并在 `calls.jsonl` 中记录调用阶段。存储、采集上限和操作说明以其 [README](../../../../providers/semantic-arbiter/README_zh.md#记录限制与清理) 为准。有界观察不会消除已有 Git 验证器 clean filter 的副作用：应使用可信受控 workspace 和已审核 Git 配置，不能假设它具有无执行边界。

合法 Provider 调用返回原 AW digest、`status: ok` 和 `effects: []`，包括语义失败。AW 和消费方集成负责执行与效果核验；本 Provider 不执行快照、阻断、回滚或清理。Fixture 概率验证契约，不验证模型效果或原生采用。
