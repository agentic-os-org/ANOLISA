# 通过 AW run 生命周期接入 AgentSight

[English](../../../en/integrations/agentsight/agentsight-integration.md)

## 实现与后续规划

已实现的只读来源连接器、自动观测和显式上下文冻结分别见[连接器](agentsight-readonly.md)与[证据合同](../../runtime/evidence-context.md)。
本文保留各层归属及后续设计。完整能力准入、`required` 模式、公共增量审计、AgentSight 审计消费与语义评估尚待实现。
配置和当前接入方式以[开发参考](../../../../../../../docs/developer-guide/zh/aw/agentsight.md)为准。

## 组件归属

| 组件 | 职责 |
| --- | --- |
| `aw run` 与原生 Adapter | 准备原生接入、绑定 AW 实例、持有前台 Agent 和临时注册，报告真实退出并释放实例 |
| `aw-service` | 保留配置、核实原生身份、管理有界实例观测任务、投影证据并记录本地元数据 |
| AW Host 与 Core | 保留已准入动作、原始截止时间、原生效果合同和关键本地审计失败语义 |
| AgentSight 连接器 | 有界 GET、明确的传输信任边界、兼容检查和显式错误，不接管探针或数据库 |
| AgentSight runtime | 持有采集器、查询服务和存储，按证据报告能力 |
| 部署 supervisor | 管理可选合包 worker 的权限、启动 profile 和退出 |

现有同用户私有 socket 是 AW 的信任边界。核实回调字段不能隔离同 Unix 用户的
恶意调用。

部署方向采用本机优先、显式配置来源。AW 持有 Agent 生命周期和本地审计，独立
管理的 AgentSight 服务提供可选观测。字面量 loopback HTTP 继续支持，无需凭据，
并记录 `loopback_unauthenticated`；它不验证监听者身份，返回数据不能独立作为
授权或执行事实的证明。远程连接（含跨容器 IP）使用 HTTPS 与私有凭据，loopback
始终指 AW 所处的网络命名空间。HTTPS 记录 `bearer_supplied`，不声称代理背后的
来源一定实施了 token 校验。两种传输使用相同的[API 合同检查](agentsight-readonly.md)。
后续若强制执行依赖经过认证的本地对端，需要单独准入具有明确对端认证的传输。

## 拟议的公共审计与交付

拟议的公共 `aw.audit/v1` 与私有 Journal、服务 wire 分开。信封包含稳定事件
ID 与 kind、经过认证的 producer scope/generation、AW/原生引用、因果引用、来源
chain/sequence/digest、时间、适用版本、证据/覆盖引用及类型专属结果。未知事实使用
null 或显式缺口。

拟议导出先覆盖现有事实，包括绑定准备、启动尝试与结果、原生回调、Provider 调用、原生报告的结果、
上下文冻结、观测/覆盖变化及事件/运行终结。调用完成不证明原生采用，退出报告也不
证明观测完整。导出器须保留这些区别；语义事件依赖下文的评估能力。

关键本地确认保持持久性。导出和 AgentSight 消费异步进行，使用稳定重放 ID 和有界
分链游标。消费者重访未完成链，报告历史缺失、损坏和滞后。相同 ID/内容是重复，
相同 ID/不同内容是完整性冲突。远端恢复不能重放 AW 动作。AgentSight 只写自己的
派生存储，不读取或改写 AW Journal 私有文件。

## 拟议的能力准入与合包

`required` 模式需在启动前核实每项来源能力，分别检查查询兼容、轨迹采集与 eBPF 覆盖；必需能力未支持或未就绪时拒绝启动。可选来源故障继续采用[连接器](agentsight-readonly.md)的有界降级行为。

合包按能力选择产物和 profile，允许只安装 AW，并区分已安装、已启用与已就绪。部署方管理特权探针及 worker 退出。Hook 不安装包、修改文件 capability 或启动 root worker；结束一次 AW 运行时保留共享服务。

## 拟议的语义评估合同

拟议能力为 `semantic.evaluate/v1`，使用已实现的[证据上下文](../../runtime/evidence-context.md)作为输入基础。模型评估和策略路由需单独实现。

| 方向 | 字段与校验 |
| --- | --- |
| 请求身份 | `schema_version`、`request_id`、`aw_event_ref`、已核实 `binding`、`stage`、`input_digest` |
| 主体/上下文 | `subject_ref`、`candidate_revision`、`context_revision`、获授权的内容投影、`evidence`、`cutoff`、剩余预算 |
| 问题集 | 版本化稳定 ASCII ID、`boolean`/`choice`/`score`、问题、声明的选项或整数等级、必答性与材料要求 |
| 响应绑定 | 回传请求、输入、候选及上下文绑定；Core 核对当前尝试的全部绑定 |
| 响应状态 | `ok`、`abstain` 或 `error`；实际 `model_version` 字段必填，未知时为 null |
| 答案 | 仅包含声明的问题 ID，不重复，答案类型匹配、证据引用有效、数值有界；`ok` 时必答项必须存在 |

公开概率与置信度使用百万分比整数。`probability_ppm`、`confidence_ppm` 范围为
`0..1000000`。Choice/Score 分布之和为 `1000000`，选项/等级 ID 必须匹配问题
定义。后端适配器校验有限的原始输出，再按版本化的确定性量化规则转换，阈值校准
包含该规则。加权分数由代码依据声明的整数等级/分布计算，不接收模型另行声称的
分数事实。Boolean 命题概率与分类置信度含义不同。

`ok` 必须包含 `answers`。Boolean 提供命题概率；Choice 提供声明过的 `selected`
选项、选项分布和置信度；Score 提供声明的等级分布和置信度。可省略可选答案。
`abstain`/`error` 不包含 `answers`，返回非敏感 `reason_code` 和受影响的问题 ID；
部分答案不能授权策略的其余部分。选中项与证据引用必须存在于请求中，版本化
Schema 拒绝未知公共字段。

证据不足返回 `abstain`；后端故障或响应无效返回 `error`。任一必答项不可用时，
该策略评估不可用。超时、过期/错配绑定、强制模型版本未知、置信度低于配置下限也
进入 `on_unusable`。既有 Provider 执行故障仍与有效策略拒绝分开。

Provider v1alpha1 无法承载这些答案。语义评估需要显式版本化的类型化结果协议，以及
服务/配置准入。不能把答案藏进 `observe` 原因文本、原地改变旧响应形状，或声称
Provider 握手已经产生 Core 回执。新能力 Schema 进入 AW 合同 Registry；原生 Hook
与 Core 适配分别按其真实保证准入。

## 后续语义路由与审计

拟议流程依次执行确定性检查、选定问题、获取/固定上下文、有界评估、答案校验、
匹配配置路由、准入动作计划、调用已注册动作，再返回原生入口支持的效果。全部
阶段共享原事件截止时间。

路由引用已注册动作与受控输入，模型生成的命令、路径或新 Provider 身份不能成为
可执行路由。后续路由不能修改运行中的不可变 Core 计划；评估与动作计划需要在
同一父事件下使用各自单次执行的阶段身份。重新评估不能清除 claim、延长截止时间
或重复既有动作；资源/候选变化后，派发前重新校验。

`no_additional_action` 不表示授权。必需评估不可用时拒绝相关分发；可选建议评估
记录缺口。保护动作必须先成功，依赖它的执行才能继续，并且只有完成相应原生/OS
保证验收后才能启用。Schema 名称或本文设计都不暗示原生审批、final guard 或
Stop/继续控制已经存在。

语义配置独立采用 `off`/`shadow`/`enforce` 模式。`off` 关闭整个扩展，不关闭
已有 AW 检查；`shadow` 只做获授权的评估，记录拟议路由，不调用路由引用的动作
Provider；`enforce` 仍要求逐策略能力与原生验收。不隐式打包模型权重、远程服务或
外发凭据。

Journal 元数据保留判断值、模型/问题集/阈值版本、上下文摘要、证据引用、选定
路由、动作结果与采用状态，不默认保存完整提示、工具内容或私有诊断。复现评估
需要另行授权保存不可变脱敏快照，只有哈希不能重建输入。

## 后续扩展的验收与回滚

能力准入测试覆盖采集器缺失、来源不兼容、权限不足和必需能力不可用。审计导出测试覆盖重放、ID 冲突、历史缺失和消费滞后。语义测试覆盖绑定不符、非法回答、过期版本、超时和必需评估不可用，同时保留现有 Hook 与观测回归。

每项扩展需有独立停用入口。回退服务协议前排空所属运行，使用匹配的 client/daemon 重启，保留 Journal 历史和外部 AgentSight 数据。合包、失联 owner 回收、完整覆盖策略与公共增量导出仍属后续工作。
