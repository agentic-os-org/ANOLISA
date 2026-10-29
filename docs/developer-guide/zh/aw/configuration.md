# AW 配置参考

[English](../../en/aw/configuration.md)

本文说明当前配置校验器接受的字段。起步模板、可用能力及 Agent
使用流程见[用户指南](../../../user-guide/zh/aw/index.md)。

[随包 Schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/schemas/configuration-v1alpha1.schema.json)
定义公共字段结构，Rust 校验器另行检查引用及字段之间的关系。本地 Host 负责
Provider 发现与准入，所选 Adapter 在绑定前执行原生准备检查。Qoder 探测并记录
CLI 版本，要求 >=1.1.64 而不固定一个版本；OpenClaw、QwenPaw、Hermes、Pi、Claude SDK
和 AgentScope SDK 仍保留各自的版本或修订限制。AgentScope SDK 必须为 2.0.6。四框架可运行示例见[组件接入教程](integration.md)。

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
| `agents.<id>.adapter` | `qwenpaw`、`qoder`、`qoder-sdk`、`openclaw`、`hermes`、`pi`、`claude-sdk` 或 `agentscope` |
| `agents.<id>.shutdown_grace_ms` | 外部信号退出宽限，1..120000 ms，默认 2000；见[生命周期](deployment/lifecycle.md) |
| `daemon.limits` | 常驻服务的并发上限，字段及默认值见[容量配置](deployment/lifecycle.md#服务并发预算) |
| `agents.<id>.argv` | 非空程序/参数数组；首项不可为空，不隐式调用 shell 或插值 |
| `agents.<id>.qoder.sequential` | `adapter: qoder` 专属的可选布尔值；将生成的原生组标为顺序执行，false 不覆盖其他匹配组 |
| `providers.<id>.protocol` | 缺省为仅执行 invoke 的 `aw-hook/v1alpha1`；显式 `aw-provider/v1alpha1` 保留结构化消息，`native-hook/v1alpha1` 保留原生回调字节 |
| `providers.<id>.transport` | `{type: stdio, location: agent, argv: [...]}`，在已绑定的 Agent 上下文中启动单次调用进程 |
| `providers.<id>.timeout_ms` | 单次调用上限正整数；运行时还须受事件剩余预算约束 |
| `providers.<id>.max_output_bytes` | stdout 上限正整数，由命令执行器落实 |
| `providers.<id>.config` | 结构化 Provider 必填的私有 JSON 对象；原生 Hook 必填空对象；布尔 Hook 省略或填空对象 |
| `events.<name>.enabled` | 已声明事件必填布尔值；省略事件等同关闭 |
| `events.<name>.required` | 默认 `false`；关闭事件不能标为必需 |
| `events.<name>.budget_ms` | 可选正整数，覆盖默认事件预算；嵌套 guard 同时共用父事件剩余预算 |
| `events.<name>.steps` | 必填有序数组；空数组不调用 Provider |
| `events.tool.before.match.tools`、`events.tool.after.match.tools` | 可选非空选择器数组；省略表示全部原生工具，`['*']` 不与精确选择器混用 |
| `events.tool.before.guard` | 可选引用已声明的 `security.violation`，before 启用时被引用事件也须启用 |
| `steps[].id`、`steps[].enabled` | ID 在事件内唯一；enabled 默认 `true` |
| `steps[].provider` | 已声明的 Provider ID，协议必须与步骤形态匹配 |
| `steps[].match.tools` | 工具前后步骤可填的工具选择器数组；与事件选择器取交集；省略表示全部工具 |
| `steps[].operation` | 布尔 Hook 缺省为 `invoke`（唯一支持值）；结构化 Provider 必填操作名，与发现结果校验 |
| `steps[].native` | 空对象，选择原生回调执行；与 `operation`、`effects` 互斥 |
| `steps[].effects` | 必填非空、不重复列表；布尔命中时请求全部效果，结构化 Provider 用作准入上限；原生步骤不填 |
| `steps[].reason_code` | 仅布尔 Hook 可填的命中原因；1–128 个 ASCII 字母、数字、`.`、`_` 或 `-` |
| `steps[].on_error` | `report`、`block` 或 `withhold_result`，受事件时机约束 |

Agent/Provider ID、步骤 ID 和操作名与 `metadata.name` 使用相同语法。数值限制
为 1 至 4,294,967,295 的整数。Agent、Provider、每事件步骤均不超过 128 项，
每条命令不超过 128 个参数。程序之后的空参数保留；参数、地址/目录字符串拒绝 NUL。

公共对象拒绝未知字段，仅 Provider `config` 接受私有字段。关闭的步骤同样检查
Provider 引用与步骤 ID 重复，避免启用时才暴露引用拼写错误。默认值是合同语义，
解析器不会将它们填入原始文档。

## Qoder SDK 执行资产

在 `qoder-sdk` 目标的 `agents.<id>.qoder_sdk` 下设置这些字段。
应用无需执行扩展时，可全部省略。

| 字段 | 接入方式与限制 |
| --- | --- |
| `sites` | `global`、`cn` 的非空去重列表，第一个区域为默认值；省略时为 `[global]`。在应用目录安装对应 SDK 包。 |
| `command_wrappers` | 最多 16 个原生 Bash 消费者授权。每项为 `{path, sha256}`，使用普通文件的绝对路径和 64 位小写十六进制 SHA-256。 |
| `runtime_preloads` | 最多 16 个受信任 Worker import 授权，文件格式与上项相同。 |
| `declarative_plugins` | 默认 `false`；为使用受管 Daemon 的普通常驻 Query 接入本地角色、Agent、命令和 Skill 资产快照。 |

每个授权文件最多 16 MiB。将 wrapper 和 preload 固定在镜像中。
AW 在 SDK 探测前验证文件，并在每个 Query 重新检查；不匹配时拒绝启动。
这些限制统计安装资产，Query 和工具调用的累计次数没有此上限。修改资产或区域后重启 worker。

Bridge 的 `startDaemon`、`createQuery().options` 和 `createSdkMcpServer` 接受
`awSite: "global"` 或 `"cn"`。Query 省略区域时继承受管 Daemon 的选择；显式选择不同区域会被拒绝。

### Worker import

`startDaemon` 的 `executableArgs` 接受成对的 `--import` 与已授权 preload 路径。
若提供 `pathToQoderCLIExecutable`，它必须指向对应 SDK 包的官方 Worker。
其他 executable 或 transport 替换会被拒绝。固定 Worker-thread transport 忽略
`executableArgs`，因此直接 Query 拒绝该选项；需要 import 时使用受管 Daemon。

### 声明式插件快照

启用 `declarative_plugins: true` 后，通过原生
`options.plugins: [{type: "local", path: "/absolute/plugin"}]` 传入本地插件根目录。
使用元数据 manifest 与声明式 Markdown；Agent 的 `tools` 工具选择受支持。
AW 拒绝可执行插件 Hook、MCP/settings 重定向、shell 插值、`permissionMode`、
`allowed-tools`，以及 Skill/命令 frontmatter 中的 `context` 和 `agent`。
未知或有歧义的 frontmatter、符号链接也会被拒绝。可执行行为改用受管 SDK Hook 或 MCP 工厂。

| 快照资源 | 每个 Query 的上限 |
| --- | --- |
| 所有插件根目录下的文件与目录 | 共 10,000 个条目 |
| 内容总大小 | 128 MiB |
| 插件根目录以下的目录深度 | 64 层 |

AW 强制设置 `skills.disableShellExecution: true`，并在释放首个提示前确认原生设置。
直接 Query 无法确认有效设置，因此拒绝此配置。Reloadable 启动在 SDK 探测前拒绝插件快照和命令 wrapper。
此配置限定于 SDK 1.0.56 / Worker 1.1.78，升级 SDK 后须重新验证。

原生工作与回调确认排空后，AW 删除快照。清理未确认时保留快照和 scope 所有权，排查原因后重启 worker。
原生命令消费者还需要显式审批与完成确认，见 [Qoder SDK 工厂](agents/qoder-sdk.md#原生-bash-命令消费者)。

## 缺省布尔 Hook

Provider 省略 `protocol` 时选择 `aw-hook/v1alpha1`，步骤省略 `operation` 或填
`invoke`。Host 在本地校验准入，不调用 `describe`/`validate_config`。命令只接收
标准化事件，以退出码 0 返回一个 JSON 布尔值。true 时 AW 投影步骤的全部 effects
和可选 reason code，false 时不请求效果，关联字段由 AW 内部处理。非零退出、超时、
stdin 未完整交付、非法或超限输出仍为错误，由 `on_error` 处理。stdout 同时受
32 字节和配置上限约束。已有显式结构化和原生协议行为不变。
见[布尔脚本示例](providers/hooks.md)。

## 原生命令与运行支持

原生步骤使用 `native: {}`，引用 `native-hook/v1alpha1` Provider。它们不执行
`describe` 或 `validate_config`，收到原生回调的完整输入，将字节输出和退出状态
交回 Adapter，不声明结构化 AW 效果。CLI 方言的非零退出仍是原生结果；Pi 将
非零/信号退出和非法原生指令视为 callback 故障，与超时、输出超限一样按 `on_error`
处理。为一个框架编写的原生命令不会自动跨框架适配。当前 Qoder
回调要求 AW 步骤之间输入一致，顺序重写链与审批流程不在已接受的原生组合范围内。
输入变化会被拒绝，并按受影响步骤的 `on_error` 处理。

对于 Claude SDK `PreToolUse`，原生命令 exit-0 的
`hookSpecificOutput.permissionDecision: deny` 与 exit 2 使用同一 veto 路径，
包括可选原因和提前事件关闭。这不开放原生批准或输入改写。

当前运行时准入工具前 `observe`/`block`、工具后 `observe` 或唯一且最后的结构化
`replace_result`，以及这两个点位的原生步骤。布尔/原生步骤不能请求结果替换。Qoder CLI 分别映射到 `PreToolUse` 和成功后的 `PostToolUse`，尚未
接通失败工具。运行时仍拒绝其他已识别事件、guard 和更强执行保证。
Qoder、Qoder SDK、OpenClaw、QwenPaw、Hermes、Pi、Claude SDK 与显式 AgentScope 2.0.6 工厂均有启动 Adapter；
受支持版本、入口及 after 事件覆盖见[原生 Agent 接入参考](native-agents.md)。

## 事件、效果与工具选择

配置识别以下 16 个名称。当前公共 Provider 只派发 `tool.before` 和 `tool.after`，
其余 14 个名称为预留词汇。实现状态见[16 Hook 概览](../../../user-guide/zh/aw/capabilities.md#16-个通用-hook)。
各 Adapter 的效果支持见
[能力矩阵](../../../user-guide/zh/aw/capabilities.md#hook-支持矩阵)。

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

Schema 词汇允许 `tool.before` 声明 `observe`、`block`、`replace_input`；`tool.after` 允许
`observe`、`replace_result`；其余事件的 Schema 仅接受观察效果。运行时只准入已实现的两个工具事件，拒绝 `replace_input`。`ask` 保留在 before 步骤中，
启用步骤请求它时拒绝配置。显式关闭的 before 步骤可以保留 `ask` 供后续编辑，
不因此获得审批能力。宿主原生审批不受影响。

`on_error: block` 仅用于执行前的 `tool.before` 或 guard；`withhold_result`
仅用于工具后；`report` 记录失败后继续。禁止原结果交付需要已验证的模型消费
边界，只修改历史记录不足以满足要求。必需的安全隐藏不能使用 `report`。
服务在准入时拒绝尚不能落实的效果与失败动作。普通 Qoder SDK 与 QwenPaw resident 启动
为结构化 Provider 和布尔 Hook 的 After 步骤声明 `withhold_result`；reloadable
启动和原生命令步骤拒绝该动作。

工具事件及其步骤均可配置 `match.tools`。工具必须同时匹配事件和步骤的列表，
才会执行该步骤；单个列表内任一条目匹配即可。任一层省略选择器或使用 `["*"]`
时，该层选择全部工具。列表须非空且无重复，`*` 必须单独使用。
结构化 Provider、布尔 Hook 和原生命令均遵循这一规则。

选择器区分大小写，支持 `*`、`bash`、`file_read`、`file_write` 或
`native:<adapter>:<exact-name>`。标准名称匹配规范化的 `event.tool.name`；
原生选择器匹配 `event.agent.adapter` 中已注册的 Adapter ID 和保留的
`event.tool.native_name`。Adapter ID 与配置的目标名称分开使用，例如
`native:qoder:Bash` 精确选择 Qoder 的 `Bash`。不支持正则或 glob 模式，
唯一通配符为 `*`。Provider 私有工具映射独立配置。

启动前，AW 根据所选 Adapter 投影事件与步骤选择器，排除交集确定无法匹配的步骤。仅剩余步骤参与 Provider 准备和效果准入，配置结构与引用仍全量校验。
`aw validate` 列出各 target 被投影掉的步骤；`aw config validate --json` 使用 `result.projected_out_steps`，包含 target、event 和 step_id。
OpenClaw after 将 `bash`、`apply-patch`、`cron` 分别规范化为 `exec`、`apply_patch`、`automations`；native_name 保留原回调名称。

共享 Host 在回调归一化、绑定检查和步骤单次占用之后执行选择。
未匹配步骤以 `kind: skipped` 完成，没有效果，也不启动调用进程；仍校验规范化
事件、检查取消及共享截止时间，并保留步骤顺序、关联和完成状态。
即使后续工具未匹配，启用的结构化 Provider 仍须在准备阶段完成 `describe`
和 `validate_config`。

跳过或中立结果不授予权限。原生拒绝和待确认的权限请求保持优先，
不受支持的参数改写仍被拒绝。Tokenless PreTool 建议须由新调用重新经过选中的
安全步骤和原生权限检查。PostTool 替换仍仅作用于末尾获准的文本槽，
保留原生 envelope 与失败状态。

`required: false` 不能授权丢弃启用的控制效果或失败处置。运行时准入
将启用步骤与 Provider 声明、实现及宿主能力对照，拒绝不支持的必需控制。
可选观察来源缺失必须明确记录。配置解析本身不执行这项运行时准入。

## 解析与兼容性

解析器接受一份 UTF-8 YAML 或 JSON，输入及展开后 JSON 均不超过 4 MiB，嵌套
深度不超过 32。重复键、非字符串键、自定义 YAML tag、merge key、非有限数值
及多文档均拒绝；普通 alias 在限额内展开。JSON 输入沿用同一套 YAML 标量解析
规则：`1e3` 这类指数写法会以浮点 `1000.0` 到达，Schema 的 integer 类型接受
整数值。JSON 入门文档与 YAML 入门文档一一对应：
[aw.minimal.json](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-config/examples/aw.minimal.json)。
超出有符号和无符号 128 位范围的十进制整数 scalar 会被拒绝，不会舍入成浮点数，
标记为 `!!float` 的 scalar 也适用。浮点值使用指数或小数写法；需要精确保留的大型
标识符使用带引号的字符串。这不扩大现有整数值的支持范围。

诊断包含字段路径或源码位置与约束，不回显字段值。

本 alpha 配置与已有能力 wire 记录、Schema ID/摘要分开演进，不应将 Provider
配置送入仅支持整数的 wire canonicalizer。校验器不安装或修改任何原生文件；
版本回退使用与目标二进制匹配的配置备份。
