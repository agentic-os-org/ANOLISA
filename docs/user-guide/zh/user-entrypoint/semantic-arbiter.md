# semantic-arbiter

[English](../../en/user-entrypoint/semantic-arbiter.md)

使用可配置决策模型判断保留意图和任务范围，或在记录时钟下确定性检查冻结参考输入。`semantic-arbiter` 返回类型化建议，不启动 Agent、不重放 Agent 动作，也不修改工作区。消费方负责授权和执行。

## 目标设计与可用范围

完整目标设计覆盖六类业务场景：成果保护、任务范围、上下文补给、状态诊断、安全响应和恢复资源治理。`semantic-arbiter` 永不执行动作，目标架构也遵守这一边界。全部 Executor 永久属于消费方集成层。

| 状态 | 本指南中的含义 |
|---|---|
| **核心已实现** | 固定的成果保护/任务范围判断、冻结内联证据、确定性 `check`，以及通过 Bailian/TypeSafe 进行 Noul 模型求值 |
| **仅契约类型/校验** | 已有完整 `nop < allow < ask < block` CandidateControl 词汇、更完整的 Outcome 类型、Directive 投影和回执校验；当前场景不会产生 `ask` 或扩展动作 |
| **消费方集成待实现** | 任务/历史采集与投递、准入、原生阻断、全部 Executor、持久审计和效果核验仍需接入；实时 EvidenceSource 采集与通用组合也待实现 |
| **外部能力须核验** | AgentSight、AW、ws-ckpt 和原生 Adapter 接口须对照目标版本源码与部署行为核验；历史能力清单不能证明支持情况 |

| 业务场景 | 目的 | 目标输出 | 可用范围 |
|---|---|---|---|
| 成果保护 | 保留必需版本；修改前保护独有成果 | `allow` 配合 `audit`，独有成果另加 `snapshot`；要求保留或证明不可用时 `block` | 核心已实现：`preservation`；执行待接入 |
| 任务范围 | 检查任务意图与允许资源；目标扩展到命令结构和网络目的地 | `allow/block` 配合 `audit`；未来确定性审批规则在接入后可产生 `ask` | 核心已实现：`task_scope`，当前仅资源 `replace/delete` |
| 上下文补给 | 查找缺失的 skill、记忆或其他已审核材料 | `retrieve_skill/retrieve_memory`、`inject_context`、`audit`；候选处置取决于交付路径 | 场景和集成待实现 |
| 状态诊断 | 判断故障、资源压力和任务进展；请求协助 | `diagnose`、`notify`、`audit`；后续候选可能需要 `ask` | 场景和集成待实现 |
| 安全响应 | 判断事件可信度并控制受影响主体 | 执行前 `block`；`freeze/quarantine/kill` 配合 `audit`，可附加 `notify` | 场景和集成待实现 |
| 善后治理 | 保留审计与恢复义务，回收合格检查点 | `nop` 配合 `audit/cleanup`；恢复需求可建议 `rollback` | 场景和集成待实现 |

仅 `preservation` 和 `task_scope` 是可调用的场景名称。其他行描述规划行为，不能作为请求值使用。

### 从证据到判定

目标流程为 **EvidenceSource → JudgmentUnit/Scenario → 经校验的答案 → Outcome → 消费方准入 → Executor → Receipt**。

| 阶段 | 职责与边界 |
|---|---|
| 消费方与 EvidenceSource | 认证任务/候选来源，绑定身份和版本，冻结有界证据。目标证据源声明 provenance、新鲜度、大小和采集时限；当前由调用者提供内联证据 |
| JudgmentUnit 与 Scenario | 单元选择必需证据、版本化问题及阈值；场景按业务目的组织单元。当前仅有两个固定单元与固定组合 |
| 经校验的答案与 Outcome | 校验答案格式、证据引用、覆盖范围和有效期，再确定性映射为限制与 obligation。模型支持度只能收紧确定性约束 |
| 消费方准入与 Executor | 检查声明能力、配置许可、原生 Adapter 支持、身份、资源范围及剩余期限；必需保护未确认前保持候选暂停 |
| Receipt 与审计 | 留存不可变 Directive，核验效果并逐项记录结果。发出请求或收到成功的传输响应均不能证明生效 |

消费方集成必须从经过认证的用户/操作者入口采集任务原文和保留要求，将当前任务及版本独立于有界会话历史留存，并标注历史遗漏或裁剪。同时冻结候选、工作区、任务和配置修订。采集、推理、保护和最终复核共享原事件截止期限，辅助采集不得续期。任务、候选或策略修订变化后须重新判定。

AgentSight 拟提供辅助历史、观测、趋势和取证报告。报告缺失不能表示许可；消费方投影 `source_reported` 后也不能将其提升为用户授权或 `local_verifier`。本地重建需要使用留存的生成器与旧输入重建并比对字节，由消费方提供并认证结果。核心不运行生成器，也不核验磁盘内容。工作区身份与资源范围须独立核验，cwd 本身不能证明路径包含关系或操作权限。

## 源码构建

尚无安装器和 RPM 分发。使用 Linux 检出目录，准备 Rust/Cargo、rustfmt、clippy 和 Make。从仓库根目录执行：

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
```

`describe` 列出 `deterministic_check` 和 `model_evaluation`、`bailian` 和 `typesafe` 后端、`preservation` 和 `task_scope` 场景，以及消息大小上限。当前 `check` 输出候选 `allow/block`；当前 `evaluate` 输出 `nop/allow/block`。两者都只产生建议类 `snapshot/audit` obligation。无需启动 daemon。

## 命令帮助

无参数运行，或使用 `help`、`-h`、`--help`，会输出人可读的命令说明、场景、示例和退出码。使用 `help <command>` 或 `<command> --help` / `-h` 查看子命令帮助。帮助不需要请求、配置或凭证，也不调用模型。

```bash
./target/debug/semantic-arbiter --help
./target/debug/semantic-arbiter help describe
./target/debug/semantic-arbiter check --help
./target/debug/semantic-arbiter evaluate --help
```

帮助以文本输出到 stdout，退出码为 `0`。`describe`、`check` 和 `evaluate` 保持 JSON 输出；非法参数仍在 stderr 输出脱敏 JSON 错误并以 `2` 退出。

## 使用决策模型求值

`evaluate` 将任务文本、候选和保留来源标记的所选证据发送给配置的服务。运行前须取得端点、凭证使用和出站数据授权。先使用提供的合成样本。远端可用性和语义准确率需要单独验证。

### 配置后端

在仓库根目录使用 `src/semantic-arbiter/examples/evaluation/bailian.json` 或 `src/semantic-arbiter/examples/evaluation/typesafe.json`。百炼样例提供工作区端点模板 `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/systemone`。将 `{WorkspaceId}` 设为已获授权的工作区 ID，并核验完整 URL；适配器原样使用此地址。TypeSafe 默认为 `https://api.typesafe.ai`，在配置的网关前缀之后追加 `/v1/systemone`。

| 字段 | 含义 |
|---|---|
| `config_revision` | 与请求 `binding.config_revision` 和全部证据绑定匹配的策略标签；样例使用 `policy-v1` |
| `backend.kind` | `bailian` 或 `typesafe`；配置不能选择夹具后端 |
| `backend.endpoint_url` / `backend.base_url` | 完整百炼推理 URL / TypeSafe 基础 URL |
| `backend.model` | 分别默认为 `decision-model-preview` / `jev-latest` |
| `backend.key_env` | 环境变量名称；默认 `DASHSCOPE_API_KEY` / `TYPESAFE_API_KEY` |
| `timeout_ms` | 必填，`1..=120000`；准备和求值从入口起共用此上限，并受原始请求截止期限约束 |
| `max_response_bytes` | 必填，`1..=262144`；流式读取上游正文时执行限制 |
| `thresholds.preservation_ppm`、`thresholds.task_scope_ppm` | 独立支持度阈值，`500001..=1000000`；默认均为 `900000` |
| `transport` | 默认 `https`；`loopback_test` 仅允许字面量回环 IP 上的 HTTP，并在响应中标注测试模式 |

百炼样例显式将两个阈值设为 `800000`；TypeSafe 样例使用 `900000`。这些值尚未建立准确率校准。响应中的 `config_digest` 绑定实际配置和版本化问题语义；`config_revision` 由调用者维护，作为策略标签。

HTTPS 校验证书。URL 拒绝内嵌凭证、查询参数和片段。适配器禁用重定向、环境代理和重试，不加载 `.env` 文件。不要将凭证值写入 JSON 或 CLI 参数；`key_env` 仅包含变量名。

### 运行合成请求

核验端点配置后，仅运行已获授权的后端。以下 Bash 命令提示输入密钥，不回显，也不将密钥值写入 shell 历史。Python 3 仅供合成请求生成器使用；Rust 公共库和 CLI 无需 Python SDK。

百炼：

```bash
read -rsp 'API key: ' DASHSCOPE_API_KEY; printf '\n'
export DASHSCOPE_API_KEY
python3 examples/evaluation/request.py preservation-allow | ./target/debug/semantic-arbiter evaluate --config examples/evaluation/bailian.json -
status=${PIPESTATUS[1]}
unset DASHSCOPE_API_KEY
printf 'semantic-arbiter exit: %s\n' "$status"
```

TypeSafe：

```bash
read -rsp 'API key: ' TYPESAFE_API_KEY; printf '\n'
export TYPESAFE_API_KEY
python3 examples/evaluation/request.py preservation-allow | ./target/debug/semantic-arbiter evaluate --config examples/evaluation/typesafe.json -
status=${PIPESTATUS[1]}
unset TYPESAFE_API_KEY
printf 'semantic-arbiter exit: %s\n' "$status"
```

生成器接受 `preservation-allow`、`task-scope-allow`、`snapshot`、`preserve` 和 `missing-evidence`。它生成新的 30 秒 Unix 毫秒预算和合成证据时间戳。禁止用此方式刷新真实证据时间戳。求值已冻结的请求文件时执行：

```bash
./target/debug/semantic-arbiter evaluate --config examples/evaluation/bailian.json request.json
```

此命令保留请求的截止期限并使用内联证据，不采集新证据。任务入口和来源标签需要独立认证；在 JSON 中填写 `local_verifier` 不能核验重建结果。

### 解读结果

| 场景 | 问题与判定 |
|---|---|
| `preservation` | 任务意图是否要求资源保持不变？yes 支持度达到阈值时产生 `preserve`；no 支持度达到阈值时保留本地重建结果：相同字节为 `allow`，独有成果为 `needs_snapshot` |
| `task_scope` | 候选操作是否符合任务意图？yes 支持度达到阈值时产生 `allow`；no 支持度达到阈值时产生 `outside_scope` |

`evaluate` 复用确定性硬门禁。结构化保留要求、允许资源上限、任务/证明缺失和覆盖缺口保持约束力，硬阻断保持 `block`。最多发起一次合并后端调用；没有满足条件的单元时不调用。注册表和组合规则固定。

检查以下响应字段：

| 字段 | 检查内容 |
|---|---|
| `answers` | 各单元的结论、原因、置信度和证据引用 |
| `outcome.control.candidate` | 汇总候选处置：当前模型求值可产生 `nop`、`allow` 或 `block` |
| `outcome.obligations` | 建议中必需的 `audit`；仅当完整成果保护判断支持时才附加 `snapshot` |
| `judgments` | 问题/单元版本、阈值、原始 P(yes)、归一化 yes/no 支持度、`model_used` 和脱敏错误 |
| `backend`、`model_called` | 已构造适配器身份及是否调用 |
| `input_digest`、`config_digest`、`binding` | 与留存请求和配置的关联 |
| `evaluated_at_ms`、`elapsed_ms`、`normalization_revision` | 最终检查时间、准备/求值耗时和概率转换版本 |

仅支持 Noul 模型答案。每个概率表示一个命题的 P(yes)。yes/no 支持度依据精确十进制 token 保守归一化为百万分比整数；置信度尚未建立与事实正确率的校准关系。上游 `model`、`usage`、`request_id` 和 `latency_ms` 作为被忽略的元数据接受。未知字段、重复键、问题不匹配和非法概率会成为模型响应错误，产生 `unknown` 答案和候选 `nop`，除非已有确定性硬阻断。

单元答案和候选处置含义不同。确定性硬阻断保持候选 `block`。对于满足模型求值条件的问题，双向支持度均不足、超时或后端/模型响应错误产生 `unknown` 答案和候选 `nop`，绝不产生 `ask`；不回退到夹具。成果保护模型弃权仅保留 `[audit]`，不携带确定性基线中的 snapshot。推理和组合后复核证据有效期。

附带 `[snapshot,audit]` 的 `allow` 要求消费方保持候选暂停，独立授权并确认快照成功，在执行前重新校验。普通 `allow` 和候选 `nop` 同样不授予执行权限。响应不包含执行回执、密钥或原始上游正文。公共库响应校验需要原始请求、配置和独立留存的 `EvaluationRecord`；导出的 trace 不能自证真实性。

## 检查冻结参考样本

样例包含合成约束和声明式证据。`check` 使用记录的 `check_at_ms` 确定性求值冻结输入，不调用模型、不解释任务文本，也不重放 Agent 或动作：

```bash
./target/debug/semantic-arbiter check examples/preservation-allow.json
./target/debug/semantic-arbiter check examples/preserve.json
./target/debug/semantic-arbiter check examples/snapshot.json
./target/debug/semantic-arbiter check examples/missing-evidence.json
./target/debug/semantic-arbiter check - < examples/task-scope-allow.json
```

候选/obligation 预期组合依次为 `allow/[audit]`、`block/[audit]`、`allow/[snapshot,audit]`、`block/[audit]` 和 `allow/[audit]`。`examples/index.json` 记录预期结果。确定性 unknown 会 fail-closed。`check` 使用 `semantic-arbiter/v1alpha1` 与 `deterministic_check` 模式；模型求值仍使用 `semantic-arbiter/v1alpha2`、`model_evaluation` 模式和实时时间戳。模型求值前，应通过合成生成器转换 check 样例。

## Outcome 目标设计参考

Outcome 将候选限制、持续的主体请求和有序 obligation 分开，用 `evidence_refs` 标识采用的证据。完整 CandidateControl 接口适用于所有操作和未来场景；下列更广词汇按说明属于**仅契约类型/校验**。当前 `check` 仅输出候选 `allow/block`；当前 `evaluate` 仅输出 `nop/allow/block`。两者的 `control.subject[]` 均为空，建议仅有 `snapshot/audit`。全部执行仍待消费方接入。

### 候选控制

| 值，按严格程度递增 | 含义 |
|---|---|
| `nop` | 未形成判定，缺口须审计。可用于不可用的可选增强，绝不能作为缺少保护证据时的回退 |
| `allow` | 判定完成且无异议，不授予执行权限 |
| `ask` | 确定性判断本候选需要人工审批；等待和超时期间均保持暂停。审批后须重新检查上下文；审批既不证明授权，也不证明执行 |
| `block` | 通过消费方原生阻断接口阻止本候选；不能撤回已执行的操作 |

目标组合取最严结果，`nop < allow < ask < block`；后续增强不能取消阻断。候选控制仅对本候选有效。`ask` 绝不从歧义推导：其类型、校验和 Approval Directive 投影现已预留，但当前场景不会产生，运行时 `ApprovalExecutor`/Adapter 准入仍待 A4/A10。单元答案 `unknown` 表示语义支持不足。确定性 `check` 将此类保护不确定性映射为 `block`；`evaluate` 中满足模型求值条件的弃权产生候选 `nop`，除非已有确定性硬阻断要求 `block`。执行回执 `unknown` 又是另一状态，表示效果尚未确认。

### 主体控制

| `action` | 期望效果 | 生命周期与授权 |
|---|---|---|
| `freeze` | 暂停执行并保留状态 | 持续到另行授权恢复；不会腾退 cwd |
| `quarantine` | 限制网络、敏感路径或凭证访问 | 独立于暂停；解除限制需要新授权 |
| `kill` | 终止主体 | 不可撤销；需要稳定目标身份、范围检查和人工确认 |

每项 `SubjectDirective` 包含 `id`、绑定的 `instance_id`、`action` 和非空、由消费方解析的 `scope_ids`。仅有 PID 不足以识别目标。目标组合对跨判定的独立保护取并集，不隐式移除已有保护。请求描述期望动作，不表示已确认的进程状态；每项都需要独立授权和回执。

### Obligations

每项 obligation 包含 Outcome 内唯一的 `id`、`critical`，以及采用封闭 `kind/parameters` 的 `action`。消费方解析受控 ID 并检查访问范围；参数不含任意命令、脚本或生成的可执行文本。

| `action.kind` | 受控 `parameters` | 目的 / 类别 |
|---|---|---|
| `audit` | `{}` | 记录脱敏判定与执行元数据 / 保护 |
| `snapshot` | `workspace_id` | 执行前保存恢复点 / 保护 |
| `rollback` | `workspace_id`、`checkpoint_id` | 建议恢复到已授权检查点 / 保护 |
| `cleanup` | `workspace_id`、非空 `checkpoint_ids` | 回收已授权且不再需要的检查点 / 保护 |
| `notify` | `recipient_id` | 通知已授权接收方 / 增强 |
| `diagnose` | `diagnostic_id` | 执行已注册、有界的诊断 / 增强 |
| `retrieve_skill` | `query_id` | 在授权范围内检索 skill / 增强 |
| `retrieve_memory` | `query_id` | 在授权范围内检索记忆 / 增强 |
| `inject_context` | 非空、已审核的 `material_ids` | 通过已准入的原生通道补充材料 / 增强 |

目标组合对 obligation 取并集，明确执行顺序并分别留存回执。保护类始终 fail-closed：必需工作未确认时暂停依赖它的执行，`critical=false` 也不能降级。增强类通常 fail-open，但失败必须可见；`critical=true` 将其升级为保护语义。每个非空 Outcome 必须恰有一项 `audit`，包括候选为 `nop` 的纯主体控制；证据引用和已启用场景的缺口也必须审计。

### Directive 与回执

`Outcome::directives` 投影 `ask`、主体请求和 obligation，不执行、调度或合并。`allow/nop` 无动作，`block` 使用消费方原生拒绝。按 **`request_id + binding + directive_id`** 关联留存的不可变 Directive 与回执，不能仅按动作名称匹配。

| 回执 `state/reason` | 含义 |
|---|---|
| `applied/confirmed` | 目标动作已独立核验生效 |
| `rejected/denied` | 已确认目标动作未生效；不隐含重试许可 |
| `unknown/unconfirmed` | 无法判断效果；进一步行动前须查询原回执或人工核验 |

unknown 回执涉及执行效果，与 unknown 判定答案、候选 `nop` 或候选 `ask` 含义不同。绝不自动重复效果未知的 Directive，也不放行依赖它的候选。当前 `decode_receipt` 使用 `CheckRequest` 和留存的 Outcome 校验 check 协议关联及 state/reason 一致性，既不提供模型求值回执解码器，也不核验 OS 效果。两种操作均不产生执行回执。

## 消费方集成设计

**下列全部 Executor 均待接入，并始终位于 semantic-arbiter 之外。** 准入必须取配置许可、声明输出、Executor 支持和已核验原生 Adapter 能力的交集，再检查身份、范围和截止期限。不支持必需保护时保持候选阻断。消费方负责调度、逐项 Directive 幂等、配额和持久审计。

| 职责分组 | Executor | 必需行为 |
|---|---|---|
| 恢复资源 | `WsCkptExecutor` | 通过已核验检查点接口处理 `snapshot/rollback/cleanup`，分别满足其前置条件 |
| 责任记录 | `AuditExecutor` | 记录判定、覆盖缺口及每项 obligation/主体结果；暴露写入失败；不授予权限 |
| 人工沟通 | `NotificationExecutor` | 解析已授权接收方，限流去重；未知投递仍为未确认 |
| 人工决策 | `ApprovalExecutor` | 将问题/答复关联到暂停的候选；执行超时约束，恢复前复核 |
| 诊断证据 | `DiagnosticExecutor` | 仅运行已注册诊断，限制执行和输出；保留来源标签，抑制同事件/目标的重复告警 |
| Skill 证据 | `SkillRetrieverExecutor` | 检索已授权 skill；绑定查询、版本、范围和缓存有效期；注入前审核结果 |
| 记忆证据 | `MemoryRetrieverExecutor` | 检索已授权记忆，采用相同的版本、范围、缓存和审核控制 |
| 上下文交付 | `ContextInjectorExecutor` | 检查已审核材料、任务关联、原生采用效果和字节上限 |
| 主体保护 | `SubjectControlExecutor` | 解析稳定目标实例，对每项动作/范围授权并独立核验效果 |

规划生命周期点位包括：`tool.before` 用于保护，`permission.request` 和 `compact.before` 用于审批，`tool.after`/`runtime.observed`/`turn.stop` 用于诊断，`security.violation` 用于响应，`turn.stop`/`session.end` 用于恢复和清理。这些安排不证明当前 `invoke` 支持；诊断若需要后续审批，必须关联到后续候选的执行前事件。

启用任何集成前，须维护已核验的 Adapter 矩阵，覆盖准入事件、原生阻断/审批/注入通道、目标身份、工作区范围、有效期限、大小限制和回执/效果核验。schema 或 `describe` 枚举本身不能证明原生能力。须核验目标源码和实际采用效果；对于仓库外第三方组件，依赖其行为前先取得目标版本源码。

### 三种上下文补给路径

| 交付路径 | 候选行为 | 集成边界 |
|---|---|---|
| 原生透明补充 | 本场景贡献 `allow/nop`，其他限制仍生效 | 规划在 `session.start`、`input.submit`、`compact.before`、`model.before_request` 或受支持的工具执行前点位检索与注入。原生准入、注入和采用仍待接入 |
| 阻断后建议 | `block` 阻止当前候选，可说明替代方案 | 任何建议重试都必须作为**新候选**重走全部原始授权和执行前检查，不能算作透明补给 |
| 工具执行后结果重写 | 工具已经运行 | 只能修改显式准入的结果槽位；既不能撤回执行，也不提供通用 prompt 注入；实际 Adapter 支持须核验 |

结果重写须核验投影可用性、文本槽位和顺序约束、来源标签、原始结果保留及原生采用效果。压缩与补给若共用末位替换槽，须定义一个组合步骤或只启用其一，不能假设独立替换步骤能够组合。检索文本始终属于证据，不能授予权限或覆盖任务约束。

### 动作安全前置

- **Snapshot：** `allow+[snapshot,audit]` 始终暂停候选，直到消费方授权快照并确认其匹配效果，再重新检查绑定、候选/资源状态、证据有效期和原始截止期限。同时完成必需审计。等待审批期间创建的快照，在答复后仍须复核。
- **Rollback：** 协调工作区静默并腾退所有工作区 cwd，再预览、取得人工确认并重新校验后执行。Freeze 和会话/轮次结束均不能满足静默或 cwd 腾退条件。无法协调时只提供恢复建议/预览，由操作者清除占用后重新确认。
- **Cleanup：** 仅删除已授权且不再承担恢复义务的检查点。Pinned 状态处理和回执保留是独立生命周期，不能假设删除检查点会释放二者。回收能力核验前，限制新增快照并报告容量压力。
- **主体动作：** freeze 与 quarantine 保持独立。先尝试已授权 freeze，再执行 quarantine，但 freeze 失败/unknown 不能阻止独立授权且满足自身前置条件的 quarantine。等待、拒批或失败的 kill 不吸收任何一项，也不移除已有保护。只有确认同一稳定目标及完整控制范围已终止，才可能使尚未执行的 freeze/quarantine 不再必要；仅有人工批准或请求发送不够。

## 输入、错误与验证

两个判定命令都从文件或 stdin 读取一个 UTF-8 JSON 请求，上限为 262144 字节。字段和枚举封闭，绑定必须匹配。`check` 要求 `semantic-arbiter/v1alpha1`、`deterministic_check` 模式和记录的 `check_at_ms`。`evaluate` 保留 `semantic-arbiter/v1alpha2` 与 `model_evaluation` 模式；其配置必须来自具名文件，并拒绝请求、配置和上游响应中所有层级的重复键。输入准备受时间上限约束，即使输入管道保持开启也不能延长。`--help` 显示用法；字段、摘要、Directive 和回执定义见 [协议参考](../../../../src/semantic-arbiter/docs/design/protocol_zh.md)。

证据数量硬上限为 64 项；`budget.max_evidence_items` 和 `budget.max_evidence_bytes` 可施加更紧的限制。超过调用者预算时，不能静默裁剪证据后放行。在请求时钟域内，证据必须满足 `captured_at_ms <= evaluation_time < valid_until_ms`，推理后仍须满足。

| 结果 | 退出码 | 输出 |
|---|---|---|
| 完整合法判定，包括模型 `unknown` 加候选 `nop` 或合法 `block` | 0 | stdout 输出响应 JSON；须检查 Outcome 和 judgment 错误 |
| 请求或配置非法/缺失、绑定非法、凭证不可用/缺失、输入超时或 I/O 失败 | 2 | stderr 输出脱敏错误 JSON；输入失败不产生 stdout |
| 允许结果输出期间超过截止期限或采用的证据过期 | 2 | stderr 输出 `deadline_exceeded`；丢弃全部 stdout，包括部分响应 |

推理期间远端拒绝认证时，会记录 judgment 错误并产生 `unknown` 答案和候选 `nop`，除非已有确定性硬阻断要求 `block`；该情况退出 `0`。后端构造时缺少凭证则退出 `2`。零退出码表示完整判定，不表示许可。允许结果使用非阻塞写入，受原始截止期限、配置上限和采用证据的有效期约束。仅接受零退出码对应的完整响应，检查候选控制，并在后续使用前重新校验。

错误仅暴露稳定错误码，不暴露路径、密钥、来源文本或原始上游正文。修正输入/配置错误后再调用。推理失败时，留存原记录，并在再次求值前重新评估候选、证据和截止期限。执行回执为 unknown 时，查询原回执或人工核验效果；禁止自动重复 Directive。回执关联本身不能证明授权或执行。

本地开发检查命令：

```bash
make check
```

此命令执行格式检查、clippy、测试和严格 rustdoc。本地检查不能证明远端可用性或模型准确率。AW Adapter 尚待实现，应位于仓库的 `providers/` 目录；本程序不能配置为 AW Provider。
