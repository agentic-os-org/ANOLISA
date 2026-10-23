# 语义判断协议

[English](protocol.md)

## 范围与职责

`semantic-arbiter` 对冻结的任务、候选和证据输入求值，返回建议性判定。消费方负责输入认证、不可变身份/版本绑定、授权和执行验证。

| 入口 | 协议 / 模式 | 求值方式 |
|---|---|---|
| `check` | `semantic-arbiter/v1alpha1` / `deterministic_check` | 在记录的 `check_at_ms` 执行确定性约束和证据检查 |
| `evaluate` | `semantic-arbiter/v1alpha2` / `model_evaluation` | 在实时截止期限内复用同一确定性硬门禁，再向模型提交满足条件的问题 |
| `describe` | 能力声明 | 支持的操作、版本、模式、场景、后端、候选控制、obligation 类型和消息大小上限 |

两种操作均使用内联冻结证据。`check` 不执行外部 I/O，也不重放 Agent 或其动作；模型求值将选定输入发送给配置的后端。两者都不采集文件系统事实或执行 Directive。单元注册表和组合规则固定；调用者可选择 `preservation`、`task_scope` 或二者。当前 `check` 生产方输出候选 `allow/block`；当前 `evaluate` 生产方输出 `nop/allow/block`。所有操作及未来场景的完整 CandidateControl 接口始终为 `nop < allow < ask < block`。库已提供显式 EvidenceSource 注册和求值前有界采集；生产证据源、可配置组合、真实 Agent 接入和 Executor 尚待实现。

公共定义位于 [`contract.rs`](../../src/contract.rs)、[`evidence.rs`](../../src/evidence.rs)、[`config.rs`](../../src/config.rs)、[`evaluation.rs`](../../src/evaluation.rs)、[`outcome.rs`](../../src/outcome.rs) 和 [`protocol.rs`](../../src/protocol.rs)。使用公共解码和校验入口执行语义约束。

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
| `scenarios` | `preservation`、`task_scope` 的非空、不重复子集；Rust 变体为 `Scenario::Preservation` 和 `Scenario::TaskScope` |
| `task` | 权威任务投影；`null` 产生原因码为 `missing_task` 的未知答案 |
| `task.session_id`、`task.version` | 必须等于 `binding.session_id` 和 `binding.task_version` |
| `task.origin` | `operator` 或 `native_user`；消费方必须认证入口 |
| `task.text` | 非空白任务意图，最多 16384 个 UTF-8 字节；仅模型求值解释此文本 |
| `task.allowed_resource_ids` | 候选操作的显式资源上限 |
| `task.preserve_resource_ids` | 必须保持不变的资源；保留要求优先于允许 |
| `candidate.operation`、`candidate.resource_id` | 在绑定工作区内，对不透明资源身份执行 `replace` 或 `delete` |

绑定必须保持不可变。每条证据的绑定必须与请求绑定完全相同。任务文本、来源报告和模型内容不能扩大结构化资源上限或授予执行权限。

### 证据、缺口和预算

| 字段 | 含义与约束 |
|---|---|
| `evidence[].id` | 答案和 Outcome 引用的唯一证据身份 |
| `evidence[].source_id`、`source_revision` | 消费方提供的来源身份和不可变内容版本 |
| `evidence[].provenance` | `local_verifier`、`source_reported` 或 `model_echo`；仅 `local_verifier` 可用于重建检查 |
| `evidence[].binding` | 与请求的任务、候选、实例、工作区和策略精确关联 |
| `evidence[].captured_at_ms`、`valid_until_ms` | 采集时间和不包含端点的失效时间；采集必须早于失效 |
| `evidence[].fact.kind` | `rebuild` |
| `evidence[].fact.parameters.resource_id` | 验证器比较的资源 |
| `evidence[].fact.parameters.result` | `identical`：留存配方重现了原字节；`unique`：现有字节包含未重现的成果；`unknown`：比较无定论 |
| `gaps[].source_id`、`scenario` | 某来源和场景的显式覆盖缺失；重复组合非法，启用单元的缺口阻止允许 |
| `budget.opened_at_ms`、`deadline_ms` | 原始事件开始时间和不包含端点的截止时间；截止必须晚于开始，求值时间不能早于开始 |
| `budget.max_evidence_items`、`max_evidence_bytes` | 调用者对证据条数及紧凑序列化证据数组字节数的限制，不能超过协议硬上限 |
| `check_at_ms` | 仅 `check` 使用，表示夹具记录时钟中的求值时间；模型请求无此字段 |

来源标签属于声明。消费方必须独立认证任务入口、证据来源和验证器结果。合成夹具仅声明这些事实。`source_reported` 和 `model_echo` 不能证明本地重建结果。

`check` 使用记录时钟。模型请求的预算和证据时间戳使用 Unix 毫秒。证据仅在 `captured_at_ms <= evaluation_time < valid_until_ms` 时可用。在 `check` 中，事件预算过期、超出调用者证据限制或其他确定性 unknown 均 fail-closed 为候选 `block`。时间字段非法、硬上限超限和绑定不匹配属于协议错误。

### 求值前证据采集

`EvidenceSourceRegistry` 是可选的生产侧库接口，不属于任一线协议。消费方在最终确定 check 或 evaluation 请求前显式注册进程内来源并选择 source ID。采集接收不含 evidence 和 gaps 的冻结请求上下文。注册表注入已注册的来源身份、版本、provenance 和请求 binding；来源输出不能替换这些字段。结果包含确定排序的内联 `evidence`、`gaps`、脱敏的逐来源报告和一个最终 `collected_at_ms`。

每个协作式异步来源只在来源超时与原事件截止时间的较早者之前尝试一次。来源实现必须立即返回 future，且不得阻塞执行器线程；Tokio deadline 无法抢占进程内阻塞代码。不重试、不回退、无隐式来源、无插件发现，也不续期。来源不可用、超时，或返回未来、过期、超龄批次时，为已选择且来源支持的场景生成显式 gap。最终冻结时点会复检全部批次，后续来源消耗的时间不能让先前已经过期的证据进入结果。来源输出非法、证据 ID 重复，或超出条数/紧凑数组字节预算时，整次采集失败且不截断。

采集不认证来源，也不提升 provenance。内存示例保持 `source_reported`，其重建声明不能满足本地验证器要求。消费方把返回的证据和缺口填入请求；确定性请求还要将 `check_at_ms` 设为返回的 `collected_at_ms`。`check` 与 `evaluate` 自身不会调用注册表。

### 线格式上限

| 上限 | 值 |
|---|---|
| 契约消息 | 262144 个 UTF-8 字节 |
| 证据条目、缺口记录、每个 ID 列表、obligation | 分别最多 64 |
| 主体动作 | `freeze`、`quarantine`、`kill` 各最多一项 |
| 标识符 | 1～128 个 ASCII 字节：字母、数字、`.`、`_`、`-`、`:`、`@` |
| 单元置信度 | 整数 `0..=1000000` |

契约对象封闭；拒绝未知字段、重复字段、未知枚举值和尾随 JSON。无符号整数字段拒绝负值和小数。带标签的事实与动作使用 `kind` 和 `parameters`。模型请求、配置和上游响应的解码还会拒绝所有层级的重复键，包括被忽略的响应元数据。输入 JSON 的排版不影响摘要所用的强类型序列化。

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
| `thresholds.preservation_ppm`、`thresholds.task_scope_ppm` | 独立整数支持度阈值，范围 `500001..=1000000`，默认均为 `900000` |
| `transport` | 默认 `https`；`loopback_test` 仅允许字面量回环 IP 地址上的 HTTP |

百炼样例使用 `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone`，并显式将两个阈值设为 `800000`。使用前填入已获授权的工作区 ID 并核验完整端点。这些阈值属于未经校准的决策参数；模型准确率需要单独测量。

URL 拒绝内嵌凭证、查询参数和片段。HTTPS 使用证书校验。HTTP 适配器配置为禁用重定向、环境代理、重试和空闲连接池。它仅加载配置的环境变量，不加载 `.env`。输出排除密钥和原始上游响应正文。

## 判断规则与模型交互

两个单元都先要求任务存在，应用保留和允许资源约束，并检查声明的覆盖缺口。保留意图判断还要求匹配的本地验证器重建证据。证明过期、缺失、不可信、冲突或无定论时产生 unknown。不可信的补充证据不能替代合格证明。任务范围判断不要求重建证据。

| 单元 / 问题 ID | 模型命题 | 支持的结果 |
|---|---|---|
| `preservation` / `preservation-v1` | 任务意图是否要求资源在此次操作下保持不变？ | yes 支持度达到阈值时产生 `preserve`；no 支持度达到阈值时保留确定性的 `allow` 或 `needs_snapshot` 结果 |
| `task_scope` / `task-scope-v1` | 对此资源执行该操作是否符合任务意图？ | yes 支持度达到阈值时产生 `allow`；no 支持度达到阈值时产生 `outside_scope` |

单元按保留意图、任务范围的固定顺序运行，不受请求顺序影响。`check` 完全依据结构化约束和证据形成答案：`identical` 允许修改，`unique` 要求快照；确定性 unknown 均 fail-closed。模型求值复用该确定性硬门禁，并将满足条件的问题合并为最多一次后端调用。硬阻断保持 `block`。对于满足求值条件的问题，双向支持度均不足、超时或后端/模型响应错误会产生 `unknown` 单元答案和候选 `nop`；歧义绝不产生 `ask`。成果保护模型弃权时仅保留 `audit`，不会把确定性基线中的 `snapshot` 带入 Outcome。

两种 HTTP 适配器均发送 Bearer 认证的 POST 请求，正文为 `{model,state,questions}`。`state` 包含 `authoritative_task`、`candidate` 和保留来源标记的所选证据。问题由组件定义，采用以版本化问题 ID 为键的 `{type:"noul",instructions:...}` 对象。state 排除端点配置和凭证。

上游 `answers` 必须恰好包含请求的问题 ID。每个答案包含数值型 `noul`；TypeSafe 还必须返回 `type="noul"`。百炼允许省略 type，存在时必须为 `"noul"`。可选顶层字段 `model`、`usage`、`request_id` 和 `latency_ms` 作为被忽略的元数据接受。未知字段、重复键、多余/缺失答案和非法概率会成为脱敏的模型响应错误：满足求值条件的单元回答 `unknown`，候选控制为 `nop`，除非确定性硬门禁已要求 `block`。返回的模型元数据不能证明后端身份。

仅支持 Noul 答案。每个值表示一个命题的独立 P(yes)，各问题的概率之和无需为一。值必须为 `[0,1]` 内的有限 JSON 数字。字符串、布尔值、null 和越界数字均被拒绝。`noul-floor-ppm/v2` 保留精确十进制 token，包括指数表示，并在浮点转换之前计算 `yes_ppm=floor(p*1000000)` 和 `no_ppm=floor((1-p)*1000000)`。阈值比较使用这些精确 floor 值。`raw_json_number()` 提供无损读取；`raw()` 是有损便捷接口。概率和单元置信度表示模型支持度，尚未建立其与事实正确率或安全性的校准关系。

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

单元答案描述该单元的结论，它本身不是 CandidateControl。候选处置由汇总后的 `outcome.control.candidate` 表达。对于 `check`，任一 `preserve`、`outside_scope` 或确定性 `unknown` 答案产生 `block/[audit]`；没有阻断答案时，`needs_snapshot` 产生 `allow/[snapshot,audit]`，其余情况产生 `allow/[audit]`。对于 `evaluate`，确定性硬阻断保持 `block/[audit]`；满足模型求值条件的问题因双向支持度不足或故障而弃权时，答案为 `unknown`，当前生产方返回候选 `nop/[audit]`。尤其是成果保护弃权不会保留基线快照建议。完整且无阻断的模型判定可产生 `allow`；成果保护判断支持时也可产生 `allow/[snapshot,audit]`。阻断会抑制快照建议。确定性置信度 `1000000` 表示规则在声明输入下已完成判定，不提供来源认证。

`allow` 属于建议。附带 `snapshot` 时，消费方必须保持候选暂停，独立授权并确认快照成功，在执行前重新校验绑定、证据和截止期限。推理成功、审批通过或响应送达均不能替代执行授权。

### 模型求值响应的附加字段

| 字段 | 含义 |
|---|---|
| `config_digest` | 非密配置及问题和解释版本的摘要；与调用者的 `config_revision` 标签分别使用 |
| `backend.kind`、`backend.mode`、`backend.model` | 已构造适配器的类型（`bailian`、`typesafe`、`fixture`）、模式（`remote`、`loopback_test`、`fixture`）和配置的模型名称/别名 |
| `normalization_revision` | 概率转换版本，`noul-floor-ppm/v2` |
| `evaluated_at_ms` | Unix 毫秒表示的最终有效性检查时间 |
| `elapsed_ms` | 求值完成时的单调流逝时间，包含输入准备 |
| `model_called` | 是否调用了所选后端；没有满足条件的问题被调用时为 false |
| `judgments[].unit`、`unit_revision`、`question_id` | 单元身份、解释版本（`semantic-units/v1`）和版本化问题身份 |
| `judgments[].threshold_ppm` | 该单元配置的支持度阈值 |
| `judgments[].probability` | 已校验的 `{raw,yes_ppm,no_ppm}`，不可用时为 null |
| `judgments[].error` | 脱敏后端错误或 null；例如 `timeout`、`authentication`、`rate_limited`、`transport`、`http`、`invalid_response`、`response_too_large` |
| `judgments[].model_used` | 已校验概率参与最终单元答案的推导；过期或确定性拒绝后为 false |
| `judgments[].evidence_refs` | 该单元本地选定的证据引用 |

CLI 在入口捕获墙钟和单调时钟。准备、推理、正文读取、校验和组合共用一个截止期限，受原始事件和配置超时限制。推理和组合后会复核证据。满足模型求值条件的路径耗尽预算时产生 `unknown` 答案和候选 `nop`，除非已有确定性硬阻断要求 `block`。证据或预算过期后，概率仍可保留在 trace 中，同时 `model_used=false`。

独立留存 `EvaluationRun::record()`。`evaluation::decode_response` 和 `validate_response` 要求原始请求、配置，以及不可反序列化构造的本地 `EvaluationRecord`，据此推导预期结果，无需再次调用模型。后端注入属于受信进程内 API；调用者必须将实际适配器与其配置绑定。线上的 trace 和后端标签本身不能提供跨不可信进程边界的认证。记录校验确认已记录结果的一致性；执行前消费方必须再次检查时效。`check` 响应校验依据留存请求重算确定性结果。

### 摘要

摘要为 `sha256:` 加小写 SHA-256，计算输入为域分隔字节和按声明顺序紧凑序列化的强类型 JSON。数组、枚举值和 null 均保留。跨语言实现必须匹配此序列化及仓库的摘要向量；对输入空白格式直接计算哈希或采用 RFC 8785 均不符合本契约。

| 摘要 | 域分隔符 | 强类型载荷 |
|---|---|---|
| `check` 的 `input_digest` | `semantic-arbiter/v1alpha1\0check-input\0` | `CheckRequest` |
| 求值 `input_digest` | `semantic-arbiter/v1alpha2\0evaluation-input\0` | `EvaluationRequest` |
| 求值 `config_digest` | `semantic-arbiter/v1alpha2\0evaluation-config\0` | 包含配置、单元版本、归一化版本及按保留意图/任务范围排序的问题定义的数组 |
| `ModelConfig::digest()` | `semantic-arbiter/v1alpha2\0model-config\0` | 仅配置；与响应的 `config_digest` 分别计算 |

`\0` 表示一个零字节。`config_revision` 是调用者维护的策略标签；`config_digest` 绑定实际配置和版本化语义。这些载荷不包含凭证值。摘要绑定内容，不提供发送方认证。

## Outcome、Directive 和回执字段

| 字段 | 含义 |
|---|---|
| `outcome.control.candidate` | 限制顺序 `nop < allow < ask < block`：未形成判定、无异议、等待审批、原生拒绝 |
| `outcome.control.subject[]` | 独立主体请求，各含 `id`、绑定的 `instance_id`、`action`（`freeze`、`quarantine`、`kill`）和非空 `scope_ids` |
| `outcome.obligations[]` | 有序建议，各含唯一 `id`、`critical` 和强类型 `action` |
| `outcome.evidence_refs` | 建议采用的证据 ID |
| `Directive.kind`、`parameters` | `approval`、`subject` 或 `obligation` 及其强类型正文 |
| Approval `parameters.id`、`candidate_id` | 保留 ID `candidate-approval` 和等待审批的绑定候选 |
| Subject / obligation `parameters` | 原始主体请求或 obligation，保留不可变 `id` 和动作参数 |

所有操作和未来场景均使用完整 CandidateControl 契约，顺序为 `nop < allow < ask < block`。当前 `check` 生产方输出 `allow/block`；当前 `evaluate` 生产方输出 `nop/allow/block`。两者的主体控制均为空，obligation 仅有 `snapshot/audit`。`ask` 表示确定性的“需要审批”决策；歧义和 `unknown` 答案都不会产生 `ask`。其类型、校验和 Approval Directive 投影现已预留。当前成果保护/任务范围生产方不会输出，运行时 `ApprovalExecutor` 与 Adapter 准入仍待实现。更广的词汇覆盖未来增强/审批、安全、主体控制和清理场景；通用合并、执行调度和动作实现尚待完成。`Outcome::directives` 投影 ask、主体请求和 obligation。消费方通过原生接口阻断候选。主体请求保持独立；等待中的 kill 不吸收 freeze 或 quarantine。

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
| `api_version` | 现有 `check` 回执解码器使用 `semantic-arbiter/v1alpha1` |
| `request_id`、`binding` | 精确关联留存请求 |
| `directive_id` | 按 `(request_id,binding,directive_id)` 留存的不可变 Directive ID |
| `state`、`reason` | 仅接受 `applied/confirmed`、`rejected/denied`、`unknown/unconfirmed` |

消费方独立完成准入和效果核验后生成回执。`decode_receipt` 接收 `CheckRequest` 和留存 Outcome，检查关联及状态/原因一致性。该 API 不提供模型求值回执解码器或操作系统效果核验。消费方须另行核验能力、身份、范围、授权、幂等和截止期限。答案 `unknown`、候选 `nop`、候选 `ask` 和回执 `unknown` 是四种不同状态，分别表示语义支持不足、未形成候选判断、确定性要求审批，以及执行效果未确认。回执为 `unknown` 时，查询原回执或人工检查效果；禁止自动重复 Directive。拒绝同样不授予重试权限。两种操作均不生成执行回执。

## CLI 完成语义与接入边界

完整合法的判定退出码为 `0`，包括模型歧义或推理失败导致的 `unknown` 答案加候选 `nop`，以及任何合法 `block`。零退出码表示完成，不表示许可。请求/配置非法或缺失、凭证不可用或缺失、绑定非法及 I/O 失败均退出 `2`，并在 stderr 输出脱敏错误 JSON。允许结果使用非阻塞写入，受事件截止时间、配置上限和采用证据的失效时间约束。输出期间过期时，以 `deadline_exceeded` 退出 `2`；丢弃全部部分 stdout。仅接受零退出码对应的完整响应，检查候选处置，并在使用前重新校验。

AW Adapter 尚待实现，应位于仓库的 `providers/` 目录。本 CLI 不能配置为 AW Provider。接入必须认证任务/证据投递、留存不可变关联、将阻断映射到原生拒绝，并在必要保护确认成功且复核通过前保持候选暂停。不支持的保护必须保持阻断。
