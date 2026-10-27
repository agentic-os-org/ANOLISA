# Provider 协议与能力准入

[English](../../../en/aw/providers/protocol.md)

[设计索引](../../../../../src/aw/docs/design/zh/README.md) · [架构总览](../../../../../src/aw/docs/design/zh/architecture.md)

`aw-provider` 定义实验性的 `aw-provider/v1alpha1` 接口，供外部策略程序使用，
并离线检查消息和请求的能力。它将统一配置衔接到 Provider 合同，本身不启动进程、
安装 Agent Hook 或建立运行时绑定。原生接入由 `aw-adapter` 实现、`aw-cli` 编排，支持范围见[能力矩阵](../../../../user-guide/zh/aw/capabilities.md#hook-支持矩阵)。


## 传输约定

执行约定为每个方法启动一个进程：调用方按配置的 argv 启动程序，将一个 UTF-8
JSON 对象写入 stdin 后关闭 stdin。Provider 仅在 stdout 返回一个 JSON 对象，
诊断写入 stderr；不隐式执行 shell 展开。`aw-provider` 定义并校验消息，
[本地 Provider Host](../../../../../src/aw/docs/design/zh/runtime/provider-host.md) 通过 `aw-exec` 执行交互。

策略阻断是成功结果，进程退出码必须为零。非零退出码表示执行失败，不论 stdout
内容是什么。JSON 非法、消息超限、身份不匹配和效果不合法都属于失败，不是策略判断。
Host 在应用配置的失败策略时必须保留这一区别；本离线库不执行原生效果。

请求和响应 Schema 随 crate 提供：

- [请求 Schema](../../../../../src/aw/crates/aw-provider/schemas/request-v1alpha1.schema.json)
- [响应 Schema](../../../../../src/aw/crates/aw-provider/schemas/response-v1alpha1.schema.json)

`Protocol::new` 无需网络即可编译 Schema。`Protocol::parse_request` 校验请求，
`Request::as_value` 提供已检查的值。`Protocol::check_response` 将响应与请求
匹配，返回 `Reply::Description`、`Reply::Configuration` 或 `Reply::Invocation`。
消息最多 1 MiB、32 层嵌套，Host 可以设置更小的输出上限。解析拒绝重复键和额外
JSON 文档。公共信封拒绝未知字段；Provider 私有配置和原生
载荷保持为 JSON 数据，不作为可执行内容或原生设置解释。

## 三个方法

每条请求包含 `api_version`、`method` 和请求 ID。Provider 必须返回相同版本和
请求 ID。调用方分配 ID，并负责其生命周期及与所选 Provider 实例的关联。

### `describe`

```json
{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"describe-1"}
```

```json
{"api_version":"aw-provider/v1alpha1","request_id":"describe-1","status":"ok","operations":[{"name":"check","events":["tool.before"],"effects":["observe","block"]},{"name":"record","events":["tool.after"],"effects":["observe"]}]}
```

描述最多声明 64 个操作及其支持的事件和效果，操作名不得为空或重复。声明来自 Provider，不能
扩大 Adapter 的可信能力或证明运行时身份。词表识别配置中的全部 16 个事件，本阶段
准入仅允许启用 `tool.before` 和 `tool.after` 的执行。

### `validate_config`

```json
{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"validate-1","config":{"rule_sets":["command-safety"]}}
```

```json
{"api_version":"aw-provider/v1alpha1","request_id":"validate-1","status":"ok"}
```

`config` 是必填 JSON 对象。Provider 定义其 Schema，必须拒绝不支持或非法的设置。AW 检查响应，
并保留它与本次校验的精确配置之间的关联。成功响应不代表 Agent 配置通过校验、
规则已经安装或 Provider 可执行文件已经认证。

### `invoke`

调用包含配置的操作、私有配置、配置版本、剩余预算、允许的效果、统一事件和不透明
输入摘要。通过检查的成功响应返回相同摘要及候选效果列表。空列表不增加约束；
`observe` 请求观测，`block` 请求阻止尚未执行的工具调用。两者都是候选效果，
库本身不写入审计记录。这些响应均不授予原生权限。
`Outcome::requests_block()` 只报告候选效果中是否包含阻断请求。

| 调用字段 | 含义 |
| --- | --- |
| `operation`、`config` | 所选 Provider 操作及已校验的私有设置 |
| `config_revision` | 调用方精确配置文档的 SHA-256，以 64 位小写十六进制表示 |
| `budget_ms` | Host 提供并执行的正数剩余预算 |
| `allowed_effects` | 为本次操作和事件准入的效果 |
| `event.agent` | Adapter、配置的 `binding_id` 和可空的运行时 `instance_id` |
| `event.session_id`、`event.tool.call_id` | 原生标识，缺失时为 null |
| `event.tool` | `name`、`native_name`、`call_id`、`input`、`result`；工具执行前的 `result` 必须为 null |
| `event.native` | 原始原生 JSON 对象，保留数据但不据此扩大权限 |
| `input_digest` | 为完整调用生成的不透明绑定 |

事件保留配置中的 Adapter 和绑定、可获得的原生会话及调用标识、任意工具名、JSON
参数、工具结果和原始载荷。缺失的标识为 null。JSON 对象键可以包含 Unicode。
负整数字面量必须落在 `i64` 范围内，非负整数字面量必须落在 `u64` 范围内；
超出范围的整数字面量被拒绝。小数和指数形式按有限 IEEE-754 `f64` 值处理。
协议不支持任意精度数字，也不保证原始数字拼写的无损往返；以字符串表示的大标识
仍保持字符串。

私有值和工具结果在上述数字范围内保留 JSON 类型和 Unicode 数据。字符串结果
不会被悄悄解析成另一个对象，null 结果也不证明执行成功。协议不推断未知工具属于
shell，不认证其参数来源。

`Protocol::bind_invocation` 接收不含 `input_digest` 的本地调用，对紧凑 JSON
序列化计算摘要，再加入 `sha256:` 前缀的绑定。`parse_request` 校验此绑定；Provider
在响应中原样返回，无需重新实现序列化。该摘要域与 `aw-contracts` 更严格的
canonical wire 编码不同。摘要一致不证明发送方已认证或效果已采用。
`config_revision` 标识调用方提供的配置；本库仅检查其格式，不核实配置文件的存在或内容。

错误响应使用 `status: "error"` 和 `error_code`，不得包含成功字段。非法响应或
Provider 错误产生协议错误，协议检查器不会将其转成成功的 `block` 结果。
最多返回 64 个效果。`reason_code` 和 `error_code` 由 1–128 个 ASCII 字母、数字、
下划线、连字符或句点组成。库的错误显示不回显不可信代码或载荷，其诊断用途由调用方控制。

## 离线准入

`admission::preflight(config, target, capabilities)` 根据受支持的模型和可信 Adapter
能力检查已启用的配置需求，不执行发现或启动进程。Host 在启动任何 Provider 前运行
预检。返回的步骤仍是候选项；预检不证明 Provider 支持这些需求。

`admission::admit` 接收已通过静态检查的 `aw_config::Configuration`、目标 ID、
调用方信任的 `AdapterCapabilities`，以及按配置 Provider ID 绑定的证据。每项
Provider 证据包括已检查的描述和私有配置校验结果。Adapter 证据包含 Adapter、
版本、入口及支持的事件与效果；调用方须从与实际运行时匹配的可信来源取得这些事实。

准入逐项确认启用且选中的步骤具有受支持的操作、事件和效果，配置校验证据与该
Provider 的精确私有配置一致。成功返回已检查的步骤；不返回 Core `PreparedPlan`，
不安装绑定、不启动 Agent，也不报告 `Ready`。
当前传输模型要求 `stdio` 和 `location: agent`。禁用事件和步骤不要求发现证据。
准入保留单个事件内的步骤顺序，不调度不同事件，也不选择串行或并行执行。

| 请求 | 准入结果 |
| --- | --- |
| `tool.before`：`observe`、`block` | 所选操作和 Adapter 均支持全部请求效果时通过 |
| `tool.after`：`observe` | 双方均支持时通过 |
| `tool.after`：`replace_result` | 单个结构化步骤与可信文本替换能力 |
| `ask`、输入替换、执行后阻断 | 拒绝 |
| 启用两个工具事件以外的事件 | 拒绝，`required: false` 也不例外 |
| `security.violation` 等 guard | 拒绝，本模型尚未实现最后一道检查的顺序保证 |
| 事件与步骤的 `match.tools` | 接受；Host 仅调用同时匹配两层选择器的工具 |
| 高于 `native_hook` 的保证 | 拒绝 |
| 缺少 Provider、操作、能力或匹配的配置校验证据 | 拒绝，并给出明确原因 |

工具执行前的 `on_error` 支持 `report`；Adapter 支持阻断时，也可以使用 `block`。
工具执行后的失败支持 `report`；已准备的 Adapter 显式声明该事件失败动作时，
还支持 `withhold_result`。目前普通 Qoder SDK 和 QwenPaw resident 入口声明该能力。其他动作拒绝。

`required: false` 不允许静默丢弃已启用但不受支持的步骤。所有启用的结构化
Provider 仍须在运行时选择工具之前提供发现证据和精确的配置校验证据。

事件与步骤选择器取交集；任一层省略或使用 `["*"]` 时，该层覆盖全部工具。
标准选择器 `bash`、`file_read`、`file_write` 匹配规范化的 `event.tool.name`；
精确的 `native:<adapter>:<name>` 匹配 `event.agent.adapter` 和保留的
`event.tool.native_name`。名称区分大小写，不提供 glob 或正则模式，见
[配置参考](../configuration.md#事件效果与工具选择)。回调归一化、绑定检查和单次
执行占用之后，未匹配步骤返回 Host 生成的跳过结果，不调用 Provider，也不产生
效果。事件校验、共享期限、取消、顺序和完成审计仍生效。
Provider 只接收选中的调用，负责私有的输入提取和后端策略。

原生拒绝和待确认权限请求优先于中立或跳过结果；不受支持的参数改写仍被拒绝。
Provider 结果不授予原生权限。`permission.request` 保留为词汇，AW 尚未实现
对应的交互审批。

`aw-host` 在本地执行时落实超时、事件预算和输出限制。离线准入本身无法落实耗时
限制、进程回收、原生调度或失败处理。因此，准入通过只是建立可运行原生绑定的
前置条件之一。


## 独立 stdio 示例

[policy.rs](../../../../../src/aw/crates/aw-provider/examples/policy.rs) 以独立程序实现三个方法。
从仓库根目录运行：

```bash
cd src/aw
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"demo-describe"}' | \
  cargo run --locked -q -p aw-provider --example policy
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"demo-config","config":{"blocked_tools":["example_tool"]}}' | \
  cargo run --locked -q -p aw-provider --example policy
```

每条命令启动一个独立 Provider 进程。`printf` 添加换行并关闭管道，示例读取 stdin
直到 EOF，再解析唯一的 JSON 请求。换行属于 JSON 空白，不表示第二条消息。
第一条命令输出 `check` 的 JSON 描述；第二条在 stdout 输出以下 JSON 确认，
字段顺序不影响含义：

```json
{"api_version":"aw-provider/v1alpha1","request_id":"demo-config","status":"ok"}
```

示例只接受一项私有设置 `blocked_tools`，其值为工具名数组。`invoke` 在工具执行前
匹配 `event.tool.native_name`，命中且本次调用允许 `block` 时请求阻断；其他调用，
包括工具执行后事件，返回空效果列表。这是示例策略，不是 sec-core 实现或内置安全
规则集。运行示例不会启动 Agent，也不使 AW 库开始执行命令。


## PostTool 文本替换

结构化 `tool.after` 操作在可信 Adapter 能力支持时可以请求 `replace_result`，并且
必须是事件最后一个启用步骤；之前可保留观察、布尔 Hook 和原生 Hook。第二个替换
步骤或替换之后还有启用步骤时，准入会拒绝。布尔 Hook 不能请求该效果。Provider 与 Adapter 证据仍
分别检查。工具选择器保留这一准入顺序；未匹配的替换步骤返回跳过，保留原始结果。
只有已准入的替换步骤获得可选投影，既有观察步骤保持原事件形态。Adapter 可提供 `event.tool.result_projection`，包含 `text`、`status`、
`content_origin` 及可选 Shell `command`。原 `result` 和原生载荷保留，并参与请求绑定。

`replace_result` 效果要求字符串 `text`；`block` 可携带非空、有界的 `text` 原因，其他效果拒绝该字段。响应最多包含一个
替换，且要求输入中存在投影。缺少投影时只允许不替换的结果。Adapter 替换选定文本
槽并保留原生 envelope 和失败状态。这是候选效果，不证明模型已消费；AW Journal 不记录文本。
Schema 增量沿用实验性的 v1alpha1 协议。CLI、服务及 Provider 须来自同一 Preview
构建；旧实现会拒绝不支持的投影或效果，不会静默忽略。

### Tokenless 组合

PreTool 通过 `block` 返回完整重试建议，新调用重新经过安全与原生权限检查；PostTool 只替换获准文本槽。
RTK 归属、结果分类和遥测设置见[Tokenless 参考](tokenless.md)。
当前接入的恢复能力为 `none`，Schema/BeforeModel 与 Retrieve 接点尚未开放。
Core 计划 API 的独立合同见[Core 执行](../../../../../src/aw/docs/design/zh/core-execution.md)。
