# Provider 协议与能力准入

[English](provider-protocol.md)

`aw-provider` 定义实验性的 `aw-provider/v1alpha1` 接口，供外部策略程序使用，
并离线检查消息和请求的能力。它将统一配置衔接到 Provider 合同，本身不启动进程、
安装 Agent Hook 或建立运行时绑定。同一套 Provider 接口面向 QwenPaw、Qoder CLI、
OpenClaw 和 Hermes；本次交付使用合成夹具，不新增原生接入的验收结论。

## 交付边界

| 层次 | 当前职责 |
| --- | --- |
| `aw-config` | 解析一份配置，检查结构和静态引用 |
| `aw-provider` | 校验协议消息、关联响应，并依据调用方提供的 Provider 和 Adapter 证据检查配置步骤 |
| `aw-contracts` / `aw-core` | 通过可信 Host 和 Journal 校验、执行已有能力模型 |
| `aw-host` | 执行 Provider 命令，限制时间和输出，处理取消，返回可关联的调用报告 |
| 后续 Adapter 与服务 | 认证原生能力，注册回调，转换效果，管理绑定并审计原生采用情况 |

配置字段保持不变。`spec.providers` 中的命名对象选择 `aw-provider/v1alpha1`、
固定的 `transport.argv` 和私有 `config`。脚本、二进制程序或后台服务的 CLI
客户端均可实现该接口。AW 不推断或自动安装程序的依赖。

[配置示例](../../crates/aw-config/examples/aw.yaml) 展示全部 16 个事件名和后续能力。
该示例通过静态校验，不代表其中启用的事件、guard 或替换效果都能通过本阶段范围更窄
的能力准入。

## 传输约定

执行约定为每个方法启动一个进程：调用方按配置的 argv 启动程序，将一个 UTF-8
JSON 对象写入 stdin 后关闭 stdin。Provider 仅在 stdout 返回一个 JSON 对象，
诊断写入 stderr；不隐式执行 shell 展开。`aw-provider` 定义并校验消息，
[本地 Provider Host](provider-host_zh.md) 通过 `aw-exec` 执行交互。

策略阻断是成功结果，进程退出码必须为零。非零退出码表示执行失败，不论 stdout
内容是什么。JSON 非法、消息超限、身份不匹配和效果不合法都属于失败，不是策略判断。
Host 在应用配置的失败策略时必须保留这一区别；本离线库不执行原生效果。

请求和响应 Schema 随 crate 提供：

- [请求 Schema](../../crates/aw-provider/schemas/request-v1alpha1.schema.json)
- [响应 Schema](../../crates/aw-provider/schemas/response-v1alpha1.schema.json)

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
| 精确工具选择器或高于 `native_hook` 的保证 | 拒绝 |
| 缺少 Provider、操作、能力或匹配的配置校验证据 | 拒绝，并给出明确原因 |

工具执行前的 `on_error` 支持 `report`；Adapter 支持阻断时，也可以使用 `block`。
工具执行后的失败动作只支持 `report`，其他失败动作拒绝准入。

`required: false` 不允许静默丢弃已启用但不受支持的步骤。通配选择器覆盖全部工具，
Provider 可以在自身操作中检查工具名和参数。词表中存在 `permission.request`，
不表示任何具体框架或入口都具备交互式审批界面。

`aw-host` 在本地执行时落实超时、事件预算和输出限制。离线准入本身无法落实耗时
限制、进程回收、原生调度或失败处理。因此，准入通过只是建立可运行原生绑定的
前置条件之一。

## 与 Core 和安全执行的关系

当前 Core 模型接受四项能力：`security.command.inspect/v2`、
`security.code.inspect/v2`、`security.content.inspect/v2` 和
`context.projection.prepare/v2`。其 pre-tool 计划要求强制命令检查和
`required_final_guard`；执行分发校验还将计划的 OS 要求与最新原生证据匹配。
这些合同保持不变。

普通原生 Hook Provider 返回 `block`，不满足上述命令检查或 final-guard 合同。
它的 `describe` 响应不是已准入的 `provider-descriptor-v1`，调用响应也不是
Host 生成的 `provider-receipt-v1`。回执的 `denied` 表示能力执行不可用或被拒绝，
不表示成功得出的安全判断：成功的安全检查即使拒绝命令，也应产生 `verdict: "deny"`
输出及 `produced` 回执。

把这些通用操作接入 Core，仍需要显式评审并版本化原生 Hook 能力模型，以及已认证的
Provider 身份。`aw-host` 执行传输并校验候选效果，不转换 Core 模型，也不为 Core 和
Journal 生成证据。既有安全模型继续保持更强的最终分发要求。框架是否
采用效果仍需要原生回读；Provider 响应或已经写入 Journal 的调用都不能单独证明采用。

## 独立 stdio 示例

[policy.rs](../../crates/aw-provider/examples/policy.rs) 以独立程序实现三个方法。
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

## 与 sec-core 联合交付

sec-core 可以依据 Schema 实现三个协议方法和私有策略配置，并使用本地 Host 进行
协议集成。内置规则、自定义策略计算和特定工具安全判断的含义由 sec-core 负责。
CLI 包装程序可以连接独立管理的 sec-core 服务，但必须将扫描结果转换为已声明的
Provider 效果，并区分扫描失败与成功的策略阻断。

AW 提供协议校验、准入和本地有界 Host，后续增加服务生命周期、可信 Adapter
能力、原生 Hook 转换及审计接线。最终 `security.violation` 编排和 sec-core 的
协作属于后续执行保障设计；本阶段不宣称提供最后一道不可绕过的安全检查。

四框架夹具校验统一消息结构及能力拒绝行为，不证明所有原生入口都加载 Hook、
框架一定采用返回的 block，或执行无法绕过回调。这些结论需要独立的运行时验收。

## PostTool 文本替换

结构化 `tool.after` 操作在可信 Adapter 能力支持时可以请求 `replace_result`，并且
必须是事件最后一个启用步骤；之前可保留观察、布尔 Hook 和原生 Hook。第二个替换
步骤或替换之后还有启用步骤时，准入会拒绝。布尔 Hook 不能请求该效果。Provider 与 Adapter 证据仍
分别检查。只有已准入的替换步骤获得可选投影，既有观察步骤保持原事件形态。Adapter 可提供 `event.tool.result_projection`，包含 `text`、`status`、
`content_origin` 及可选 Shell `command`。原 `result` 和原生载荷保留，并参与请求绑定。

`replace_result` 效果要求字符串 `text`；`block` 可携带非空、有界的 `text` 原因，其他效果拒绝该字段。响应最多包含一个
替换，且要求输入中存在投影。缺少投影时只允许不替换的结果。Adapter 替换选定文本
槽并保留原生 envelope。这是候选效果，不证明模型已消费；AW Journal 不记录文本。
Schema 增量沿用实验性的 v1alpha1 协议。CLI、服务及 Provider 须来自同一 Preview
构建；旧实现会拒绝不支持的投影或效果，不会静默忽略。

### PreTool 重试与 PostTool 归属

Tokenless Provider 在 `tool.before` 上准入 Protocol v2 `pre_tool`，复用已有的
`block` 效果。对于具有对象 `command` 的受支持 Shell 工具，声明
`replace_arguments: false, block_and_suggest: true`，由 Core 选择改写。
AW 将完整的建议参数作为模型可见阻断原因返回，不输出 `updatedInput`、原生
`modify`、审批指令，也不执行 Shell。Passthrough 必须保持原参数；直接替换动作、
修改非 command 字段或非法响应均作为 Provider 故障。

模型必须发起新调用才能采用建议。原调用和重试均经过既有 sec-core before 步骤及
原生权限链；Tokenless 的中立结果不能覆盖安全拒绝或扫描故障时的 fail-closed。
联合模板先保留扫描，再追加 PreTool；完成观察仍位于最终 PostTool 步骤之前。
AW 不改变外部 Hook 的顺序；外部改写 Hook 仍遵循宿主自己的权限合同。

被阻断调用的 ID 不能标识重试。Tokenless 接入层从实际执行参数中识别独立的归属
RTK 包装（Agent、Session、data 目录及绝对路径的 `rtk`），将
`output_optimization: rtk` 传入 PostTool，并让此类重试通过 PreTool。
识别时解码带引号或转义的字面量，并通过已存在祖先的规范路径比较状态目录，不创建目录。
仅跳过[文档列明的执行前缀形式](../../../../docs/user-guide/zh/user-entrypoint/aw-tokenless.md#pretool-与权限行为)，
其他命令的参数中出现归属包装并不足以确认 RTK 归属。
裸 `rtk`、外部归属、未知或带额外选项的包装，以及复合 Shell 命令不推断 RTK 归属。这只是优化提示，不授予权限
或恢复访问；不新增 AW 持久优化/恢复存储，也不复制压缩策略。
RTK 定位和改写仍由公共 Tokenless CLI 负责；RTK 是 PreTool 的外部依赖，当前
AW 包未包含它。CLI 故障按 `on_error: report` 显式报告；sec-core 保持
`on_error: block`。

### 生命周期扩展边界

当前桥接实现通过重试的 `pre_tool` 和文本替换的 `post_tool`，未实现透明参数
替换或 `before_model` Schema 替换。恢复、Stash、可达的恢复入口及恢复结果旁路
由 Tokenless 接入层负责；当前请求继续声明 recovery `none`。启用 PreTool 不等于
获得 Tokenless 全部能力。

恢复需要真实的 Agent 可调用入口及 Tokenless 所需的原生授权与作用域，并将 Retrieve
结果旁路，防止再次压缩。Schema 替换需要模型请求点位及独立恢复能力；当前支持的
OpenClaw、Hermes 原生接入缺少该点位，应声明能力子集，不能声称四框架完全一致。
失败环境诊断还需要独立的附加结果合同：当前桥接有意保持失败结果原样。上游 Tool Ready
仍为硬禁用，不应计入当前已激活能力的完整性承诺。
