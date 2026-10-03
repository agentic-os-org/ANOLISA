# AW 配置参考

[English](../../en/aw/configuration.md)

本文说明当前配置校验器接受的字段。起步模板、可用能力及 Agent
使用流程见[用户指南](../../../user-guide/zh/user-entrypoint/aw.md)。

[随包 Schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/schemas/configuration-v1alpha1.schema.json)
定义公共字段结构，Rust 校验器另行检查引用及字段之间的关系。本地 Host 负责
Provider 发现与准入，Qoder Adapter 在绑定前核实受支持的原生版本。

## 配置字段

唯一接受的外层为 `apiVersion: aw/v1alpha1`、`kind: AWConfiguration`、
`metadata: {name: ...}` 和 `spec: {...}`。不接受或自动迁移早期草案的平铺
`api_version`/`name`。`status`、已安装绑定、revision 和能力状态不属于用户输入。
下表除明确标注外，所有字段均在 `spec` 内。

| 字段 | 合同 |
| --- | --- |
| `metadata.name`（在 `spec` 外） | 配置身份；1 到 128 个 ASCII 字母、数字、`.`、`_` 或 `-` |
| `daemon.startup` | `on_demand` 启动或复用匹配服务，`external` 要求服务已存在；校验时均不启动进程 |
| `daemon.endpoint`、`daemon.state_dir` | 必填非空字符串；`auto` 选择按配置区分的私有位置，显式路径必须为绝对路径并指向同一 `aw.sock` |
| `execution.guarantee` | 仅 `native_hook`，不提供 OS、final 或 protected 保证 |
| `execution.default_event_budget_ms` | 必填正整数共享事件预算，各步骤不会获得重新开始的预算 |
| `audit.enabled`、`audit.payload` | 本版本固定为 `true`、`metadata_only`；服务持久保存执行元数据 |
| `agents.<id>.adapter` | `qwenpaw`、`qoder`、`openclaw` 或 `hermes`；识别名称不等于运行效果已认证 |
| `agents.<id>.argv` | 非空程序/参数数组；首项不可为空，不隐式调用 shell 或插值 |
| `agents.<id>.qoder.sequential` | `adapter: qoder` 专属的可选布尔值；将生成的原生组标为顺序执行，false 不覆盖其他匹配组 |
| `providers.<id>.protocol` | 结构化消息用 `aw-provider/v1alpha1`，原生回调字节用 `native-hook/v1alpha1` |
| `providers.<id>.transport` | `{type: stdio, location: agent, argv: [...]}`，在已绑定的 Agent 上下文中启动单次调用进程 |
| `providers.<id>.timeout_ms` | 单次调用上限正整数；运行时还须受事件剩余预算约束 |
| `providers.<id>.max_output_bytes` | stdout 上限正整数，由命令执行器落实 |
| `providers.<id>.config` | 结构化 Provider 的必填私有 JSON 对象，准备时校验；原生 Hook 要求空对象 |
| `events.<name>.enabled` | 已声明事件必填布尔值；省略事件等同关闭 |
| `events.<name>.required` | 默认 `false`；关闭事件不能标为必需 |
| `events.<name>.budget_ms` | 可选正整数，覆盖默认事件预算；嵌套 guard 同时共用父事件剩余预算 |
| `events.<name>.steps` | 必填有序数组；空数组不调用 Provider |
| `events.tool.before.match.tools`、`events.tool.after.match.tools` | 可选非空选择器数组；省略表示全部原生工具，`['*']` 不与精确选择器混用 |
| `events.tool.before.guard` | 可选引用已声明的 `security.violation`，before 启用时被引用事件也须启用 |
| `steps[].id`、`steps[].enabled` | ID 在事件内唯一；enabled 默认 `true` |
| `steps[].provider` | 已声明的 Provider ID，协议必须与步骤形态匹配 |
| `steps[].operation` | 结构化步骤必填的非空操作名，与 Provider 发现结果校验 |
| `steps[].native` | 空对象，选择原生回调执行；与 `operation`、`effects` 互斥 |
| `steps[].effects` | 结构化步骤必填的非空、不重复列表；声明请求上限，不授予权限；原生步骤不填 |
| `steps[].on_error` | `report`、`block` 或 `withhold_result`，受事件时机约束 |

Agent/Provider ID、步骤 ID 和操作名与 `metadata.name` 使用相同语法。数值限制
为 1 至 4,294,967,295 的整数。Agent、Provider、每事件步骤均不超过 128 项，
每条命令不超过 128 个参数。程序之后的空参数保留；参数、地址/目录字符串拒绝 NUL。

公共对象拒绝未知字段，仅 Provider `config` 接受私有字段。关闭的步骤同样检查
Provider 引用与步骤 ID 重复，避免启用时才暴露引用拼写错误。默认值是合同语义，
解析器不会将它们填入原始文档。

## 原生命令与运行支持

原生步骤使用 `native: {}`，引用 `native-hook/v1alpha1` Provider。它们不执行
`describe` 或 `validate_config`，收到原生回调的完整输入，将字节输出和退出状态
交回 Adapter，不声明结构化 AW 效果。非零退出仍是原生结果；`on_error` 处理超时、
输出超限等执行故障。为一个框架编写的原生命令不会自动跨框架适配。当前 Qoder
回调要求 AW 步骤之间输入一致，顺序重写链与审批流程不在已接受的原生组合范围内。
输入变化会被拒绝，并按受影响步骤的 `on_error` 处理。

当前运行时准入工具前 `observe`/`block`、工具后 `observe`，以及这两个点位的原生
步骤。Qoder CLI 1.1.64 分别映射到 `PreToolUse` 和成功后的 `PostToolUse`，尚未
接通失败工具。其他已识别事件、精确选择器、guard、替换效果和更强执行保证均不
代表已有运行支持。首个启动 Adapter 是 Qoder，另外三个 ID 仍属于配置词汇。

## 事件、效果与工具选择

配置识别以下 16 个名称。这里定义事件词汇，运行支持以上述较小范围为准。

| 事件 | 含义 |
| --- | --- |
| `session.start` | 会话创建、加载或恢复 |
| `input.submit` | 输入到达原生提交点 |
| `tool.before` | 原生执行前的工具意图 |
| `tool.after` | 原生工具完成，包含宿主报告的失败 |
| `permission.request` | 宿主请求权限判断 |
| `compact.before` | 上下文压缩前 |
| `compact.after` | 原生压缩结果 |
| `subagent.start` | 原生子 Agent 启动 |
| `subagent.stop` | 原生子 Agent 到达停止点 |
| `turn.stop` | 任务停止检查，不证明任务成功 |
| `session.end` | 原生会话结束 |
| `model.before_request` | 模型请求到达已验证的发送边界 |
| `runtime.observed` | 可信来源登记运行实例 |
| `runtime.exited` | 可信来源观测根运行实例退出 |
| `security.violation` | AW 工具前主动末尾检查的暂名 |
| `coverage.changed` | 已观测接入覆盖发生变化 |

预留的 `security.violation` 设计通过启用的 `tool.before` 的 guard 执行；当前
运行时拒绝 guard。该设计检查最终候选，
允许 `observe`/`block`，不修改参数。它不是第二个原生 Hook，也不保证排在全部
第三方 Hook 之后。检查后参数再次改变时，实际执行边界必须重新检查。

`tool.before` 允许 `observe`、`block`、`replace_input`；`tool.after` 允许
`observe`、`replace_result`；其余事件本版本仅观察。`ask` 保留在 before 步骤中，
启用步骤请求它时拒绝配置。显式关闭的 before 步骤可以保留 `ask` 供后续编辑，
不因此获得审批能力。宿主原生审批不受影响。

`on_error: block` 仅用于执行前的 `tool.before` 或 guard；`withhold_result`
仅用于工具后；`report` 记录失败后继续。禁止原结果交付需要已验证的模型消费
边界，只修改历史记录不足以满足要求。必需的安全隐藏不能使用 `report`。
服务在准入时拒绝尚不能落实的效果与失败动作。

选择器为 `*`、`bash`、`file_read`、`file_write` 或四个适配器 ID 对应的
`native:<adapter>:<精确名称>`。原生选择器依赖宿主，不是跨框架语义。除单独
`*` 外不提供正则或 glob 匹配。全部工具路由包括原生自定义工具并保留输入，
不表示每个 Provider 都能理解每种工具。

`required: false` 不能授权丢弃启用的控制效果或失败处置。运行时准入
将启用步骤与 Provider 声明、实现及宿主能力对照，拒绝不支持的必需控制。
可选观察来源缺失必须明确记录。配置解析本身不执行这项运行时准入。

## 解析与兼容性

解析器接受一份 UTF-8 YAML 或 JSON，输入及展开后 JSON 均不超过 4 MiB，嵌套
深度不超过 32。重复键、非字符串键、自定义 YAML tag、merge key、非有限数值
及多文档均拒绝；普通 alias 在限额内展开。诊断包含字段路径或源码位置与约束，
不回显字段值。

本 alpha 配置与已有能力 wire 记录、Schema ID/摘要分开演进，不应将 Provider
配置送入仅支持整数的 wire canonicalizer。校验器不安装或修改任何原生文件；
回退时移除新增库依赖，并恢复调用方自行编辑的配置草案。
