# sec-core Provider 接入参考

[English](../../../en/aw/providers/sec-core.md)

本页用于组件接入与维护。安装和日常使用请阅读[用户指南](../../../../user-guide/zh/aw/security.md)。

`aw-provider-sec-core` 让 AW 策略调用公共 `agent-sec-cli scan-code` 与 `scan-pii` 命令。
配置指定哪些工具输入包含代码，以及发现风险后返回观察还是阻断候选效果。
sec-core 提供扫描器与规则，AW 负责配置、协议校验和有界调用。

安装和后台准备见[用户指南](../../../../user-guide/zh/aw/security.md)。下文说明输入映射、故障处理和独立桥接验证。

## 前置条件与构建

按 [sec-core 指南](../../../../user-guide/zh/agent-sec-core/index.md) 安装 sec-core。
本桥接要求 Rust V2 CLI 支持 `--socket`、`--timeout-ms` 和
`scan-code --code --language --mode`，且已有可访问的 sec-core daemon 正在运行。
请核对部署版本的帮助；旧 Python CLI 未通过本桥接验收。
AW 不向 sec-core 添加命令或依赖。

从源码构建 AW Provider 与本地 Host 示例：

```bash
cd src/aw
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
cargo build --locked -p aw-host --example host
```

Provider 二进制位于 `../../providers/agent-sec-core/target/debug/aw-provider-sec-core`，启动参数为：

```text
aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH
```

`--cli` 指定公开的 `agent-sec-cli` 可执行文件，`--socket` 指定其已有 daemon。
两个路径均必填且必须为绝对路径，`--help` 输出用法。进程从 stdin 读取一份以 EOF
结束的 AW Provider JSON 请求，并向 stdout 写入一份响应。通常由 Host 启动。

## 外部后台与显式健康诊断

Provider 包包含桥接程序、schema、示例和配置资源，不包含 `agent-sec-cli`、
`agent-sec-daemon` 或 `agent-sec-daemon-health`。按
[sec-core 指南](../../../../user-guide/zh/agent-sec-core/index.md)安装并维护这些
程序，服务生命周期由后端负责。复用实际 socket；root 服务不能执行用户可写安装
中的程序。

安装器或诊断消费者可显式查询后端 helper：

```bash
AW_SEC_PROVIDER="$AW_EXTENSION_PREFIX/libexec/aw/providers/sec-core"
"$AW_SEC_PROVIDER/aw-provider-sec-core" --health \
  --helper "$(command -v agent-sec-daemon-health)" \
  --socket /run/agent-sec-core/daemon.sock
```

这是可选组件诊断接口，不增加 Agent 启动步骤；日常仍使用 `aw run`。
它只调用已有 `daemon.health` 探测，进程期限为一秒，每路输出限制为 4 KiB，
不会启动 daemon、扫描代码或执行目标准入。helper 校验后台的 root peer 凭据，
AW 移除 sec-core 测试绕过环境变量。helper 非零退出、无效/不健康响应、非 root
后台、组件缺失和超时均保持 unavailable；不返回 helper 私有输出。

独立 `aw-sec-core-health/v1alpha1` JSON schema 位于
`providers/agent-sec-core/schemas/health-v1alpha1.schema.json`，
`tests/fixtures/` 包含可达和不可用 fixture。`helper_installed` 只说明文件存在；
`socket_configured` 只说明传入了绝对 socket 路径。认证健康成功后才返回
`backend: authenticated_reachable`。`admission` 和 `check_result` 保持
`unknown`：后台健康和近期执行的 `status=ok` 都不能证明目标准入或扫描无风险。
现有 `aw inspect`、Provider view 协议及其 unknown 字段不变。诊断结果不会替代
每次调用的 `on_error` 配置；后台失联时 `on_error: block` 继续阻断。

AW 卸载只移除其管理的桥接程序/资源，保留独立管理的系统服务、`/etc`
配置及后台数据。后台升级/移除交给拥有它的系统包；AW 暂无自动服务安装、
升级或卸载衔接。

## 配置 Provider

复制[完整示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/agent-sec-core/examples/aw.yaml)为
`aw.sec-core.yaml`，替换可执行文件与 socket 路径。示例用 `native:qoder:Bash`
选择 Qoder CLI 的 `Bash`，将 `Bash` 映射到 `/command`，并使用 `mode: block`、
`[observe, block]` 和 `on_error: block`。执行前扫描和执行后观察使用相同的明确范围。
其他原生工具、MCP 调用和新增工具名称保留原生行为，这些步骤记录为 `skipped`。
跳过检查不授予安全许可，原生权限拒绝继续有效。

步骤和事件选择器取交集。已选中调用的映射缺失、输入提取错误、超时、Provider
崩溃和后端故障通过 `on_error: block` 阻断执行。事件身份缺失在选择前作为输入错误处理。
扫描发现风险保留独立的 `code_risk` 结果。需要明确的只观察策略时，使用
`mode: observe` 和 `on_error: report`。

现有模板使用以下精确名称，所有映射均提取 `/command`：

| 模板 | 原生选择器 | Provider 映射键 |
| --- | --- | --- |
| 发行模板 / Qoder CLI | `native:qoder:Bash` | `Bash` |
| 发行模板 / OpenClaw | `native:openclaw:exec` | `exec` |
| SDK 安全 / Qoder SDK | `native:qoder-sdk:Bash` | `bash` |
| SDK 安全 / QwenPaw | `native:qwenpaw:execute_shell_command` | `execute_shell_command` |

[SDK 安全示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
还包含应用 Hook，各自保留选择器和错误动作。原生参数变化须经过 Adapter 现有的重新授权路径，
或在派发前拒绝；之前的扫描结果不能授权变化后的参数。结构化 MCP 授权使用独立策略合同，
shell/Python 输入映射不授予远端 MCP 权限。

Host 选择器支持 `*`、标准名称 `bash`、`file_read`、`file_write`，以及
`native:<adapter>:<native_name>`。原生选择器匹配事件的 Adapter 和保留的
`event.tool.native_name`。Provider `config.tools` 独立使用规范化的
`event.tool.name` 作为键，配置时须核对 Adapter 的这两个事件字段。
例如，Qoder 原生 `Bash` 调用可通过 `native:qoder:Bash` 选择，
对应的 Provider 映射使用规范化工具名称。

Provider 的私有 `config` 接受以下字段：

| 字段 | 含义 |
| --- | --- |
| `version` | 必填，固定为 `1` |
| `mode` | 必填，`observe` 或 `block`，影响 `scan_code` 和 `check_skill` |
| `unmapped_tool` | 可选，`error`（默认）或 `observe`，控制 `scan_code` 对已选中但没有映射的工具的处理 |
| `prompt` | 可选 `{mode: observe\|block, scan_mode: fast}`，用于 `scan_prompt` |
| `pii` | 可选 `{input_mode: observe\|block, output_mode: observe\|replace_result}`；输入模式用于用户及工具输入，输出模式用于工具结果 |
| `skill` | 可选布尔值，默认 `false`；启用 `check_skill` |
| `tools` | 最多 128 个精确的规范化代码工具名；配置 `skill`、`prompt` 或 `pii` 时可省略 |
| `tools.<name>.language` | `bash` 或 `python` |
| `tools.<name>.input_pointer` | 指向 `event.tool.input` 内部的 JSON Pointer，所选值必须为字符串 |

未知字段、不支持的语言和非法指针会被拒绝。空指针选择根字符串，`/command`
选择字段，`/items/0` 选择数组元素。工具名区分大小写，使用 `event.tool.name`；
`native_name` 不参与映射选择。已选中但未映射的工具默认返回 Provider 错误
`tool_unmapped`，没有候选效果。省略 `unmapped_tool` 的已有配置也采用此严格默认值。
需要为这些工具保留只观察、不扫描的行为时，显式配置 `unmapped_tool: observe`，
返回结果为 `observe/tool_unmapped`。

`mode: observe` 记录已映射工具的扫描结果，发现风险时返回观察效果。
缺少映射、提取失败和后端错误仍返回错误。已映射工具缺少代码或代码不是字符串时，
返回 `invalid_tool_input`；配置 `unmapped_tool: observe` 时也执行这一校验。
仅观察的错误处理策略须显式使用 `on_error: report`。

| 操作 | 事件 | 结果 |
| --- | --- | --- |
| `scan_code` | `tool.before` | `ok: true, verdict: pass` → `observe/code_pass`；`warn` 或 `deny` 按 `mode` 返回 `observe/code_risk` 或 `block/code_risk` |
| `check_skill` | `tool.before` | 有效签名内容返回 `observe/skill_pass`；其他已完成的信任判定按 `mode` 观察或阻断 |
| `observe_tool` | `tool.after` | `observe/tool_observed`，不调用扫描器 |

`describe`、私有配置校验和工具后观察不访问 sec-core。工具前扫描使用字面参数和
`--mode regex`，不会把代码作为 shell 命令执行。CLI 退出 0 表示扫描完成，不代表
判定为 `pass`。非零或信号退出、非法 JSON、无效判定、超时和输出超限均返回
Provider 错误，不产生候选效果。

调用方必须限制整个 Provider 进程的期限，包括 stdin 接收和 stdout 输出。
`aw-host` 通过 `aw-exec` 提供这一保证。直接调用时也必须在请求发送完毕后关闭 stdin，
并在 I/O 停滞时终止进程；Provider 自身不保证独立调用的总耗时上限。
EOF 到达后，桥接从 invoke 预算（最多 60 秒）中扣除读取和解析已消耗的时间，
仅将剩余预算交给扫描。CLI stdout 上限为 1 MiB，stderr 上限为 64 KiB，
并向 CLI 传递 Host 为 Provider
选定的环境。响应仅携带原因码与绑定的输入摘要，不包含源码、发现详情或 CLI stderr。
代码经 argv 传递，受操作系统的参数可见性与大小限制；当前 CLI 合同不提供 stdin
或文件回退路径。

## 用户提交检查

[SDK 安全示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
为普通 resident Qoder SDK 1.0.56 / Worker 1.1.78 和 QwenPaw 2.2.2.post1 /
AgentScope 2.0.10 增加 `input.submit`。沿用原生应用的配置、权限、模型与初始化，
在配置中加入以下 Provider 设置与提交步骤：

```yaml
# spec.providers.security.config
prompt: {mode: block, scan_mode: fast}
pii: {input_mode: block, output_mode: observe}
# spec.events
input.submit:
  enabled: true
  required: true
  steps:
    - id: scan-prompt
      provider: security
      operation: scan_prompt
      effects: [observe, block]
      on_error: block
    - id: scan-user-pii
      provider: security
      operation: scan_user_input
      effects: [observe, block]
      on_error: block
```

`scan_prompt` 读取 `event.input.text`，调用公共 V2 CLI 的
`scan-prompt --mode fast --source user_input --format json`，文本通过 stdin 传入。
CLI 使用配置的 daemon socket 和剩余超时。fast 模式运行 L1 规则；私有配置校验
拒绝 `standard`、`strict`、ML 模型选项和 `multi_turn`。全空白文本无需调用扫描器。
其他响应须表明规则引擎完成了整个去除首尾空白后的输入扫描：截断、缺少检测层、
降级、空输出、非法输出或 CLI 失败均返回 Provider 错误。Prompt Scanner 的
`ok=false` 配合 `warn`/`deny` 表示已完成的风险判定。`prompt.mode` 决定风险的
观察或阻断；`on_error` 单独决定故障处置。响应只携带 `prompt_pass` 或
`prompt_risk`，不含原文与发现详情。

`scan_user_input` 将完整 `event.input.text` 交给共用 PII 扫描模块，调用
`scan-pii --stdin --format json --source user_input`，保留当前 turn 的 trace context
和剩余预算。模块检查来源、字节数和扫描完成状态。`pii.input_mode` 控制风险阻断，
故障按 `on_error` 处理；响应仅携带 `pii_pass` 或 `pii_risk`。`output_mode` 属于共用
PII 配置，此操作保留用户原文。

Qoder 等待公开 `UserPromptSubmit` 快照的检查结果，拒绝时返回原生 block 决定，
已有原生 Hook 拒绝仍然有效。QwenPaw 在原生 reply 准入与记忆写入前等待检查。
新用户文本扫描一次；确认、中断及外部工具执行结果事件按原生流程恢复，不重扫
旧输入。QwenPaw 提交被拒绝后，清理 scope 即可继续接收下一条新输入。两端生成
新的轮次 UUID，保留会话身份，同一事件全部步骤共享预算，提交事件不带工具或调用身份。

当前覆盖文本提交。QwenPaw 的非文本用户块按映射错误交给 `on_error` 处理。
受限 reloadable 入口在准入时拒绝该事件，使用普通 resident 模式。PII/Prompt 每项
检查选择原生或 AW 管理，避免重复扫描；AW 保留用户 Hook。

## 工具 PII 策略

[双框架示例](../../../../../providers/agent-sec-core/examples/aw.tool-pii.yaml)在同一 Provider 上组合工具输入 PII、代码扫描和输出 PII。`scan_tool_input` 以 `tool_input` 来源扫描完整工具参数；`pii.input_mode: block` 时命中返回 `block/pii_risk`。`scan_tool_output` 以 `tool_output` 来源扫描工具实际完整结果。通过返回 `observe/pii_pass`；观察模式命中返回 `observe/pii_risk`。

配置增加 `pii.input_mode`（`observe` / `block`）和 `pii.output_mode`（`observe` / `replace_result`），两者在配置 `pii` 时必填。此时 `tools` 可省略或为空；代码扫描仍依赖其工具映射。

结构化参数和结果先按 JSON 扫描以保留字段和值的上下文，再逐个扫描解码后的字符串值与对象键，保留换行、Tab 和自定义规则锚点的语义。普通字符串直接扫描。所有扫描及保留结构检查共用本次调用的剩余预算；预算耗尽时检查失败。

`pii.output_mode: replace_result` 命中后需要 Adapter 授权的文本投影。Provider 在私有副本中清空唯一对应文本，再用同一预算扫描保留结构。保留结构通过后返回固定安全提示 `Sensitive data detected by AW; content withheld.`，保留原生错误状态和元数据。缺失或歧义文本槽、保留字段仍含敏感信息时返回 `unsupported_pii_result`。后端脱敏文本不参与替换。安全的失败工具结果保留原错误语义；Qoder 的 `PostToolUseFailure` 无法替换错误文本，含 PII 时停止交付。

必需检查使用输入 `on_error: block`、输出 `on_error: withhold_result`。CLI/后端不可用、非零退出、空或损坏响应、来源不符、截断或不完整扫描、字节数不符、自定义规则无效或失败均返回检查错误，与风险命中分开。文本仅经 stdin 传入，Provider 响应不含原文或 findings。共用 `pii::call` 同时提供 `Source::UserInput`，供提交阶段消费者复用。

Qoder SDK 普通常驻 Query 返回原生 `updatedToolOutput` 替换指令；受限 `--reloadable` 模式在准入时拒绝替换。QwenPaw 普通 App 回复对选中替换或停止交付的工具流进行缓冲，Coordinator 的公开流同样等待最终采用。生产配置按实际工具选择范围。多文本块、媒体、托管 MCP 和后台结果交付需要分别完成原生验收；Post 回调本身不能证明这些模式采用了替换。

各工具阶段明确选择 AW 或原生 PII。AW 接管时显式移除原生 PII 的 PreToolUse/PostToolUse 注册，按用户文本保护需求保留 UserPromptSubmit。AW 保留原生设置，不自动删除 Hook；同时保留两套工具检查会造成重复扫描。

## 加载前校验 Skill

[SDK 策略示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
为 Qoder SDK 和 QwenPaw 使用同一 Provider。设置 `config.skill: true`，
在 `tool.before` 增加 `operation: check_skill`、`effects: [observe, block]`、
`on_error: block`，选择 `native:qoder-sdk:Skill` 和 `native:qwenpaw:Skill`。
共用的 `mode: block` 只允许 `pass`。`tools` 继续映射代码输入，Skill 检查无需
输入指针；只检查 Skill 时可在启用 `skill: true` 后省略 `tools`。

Adapter 在 `event.native.aw_skill` 提供请求的 `name`、规范化 `directory` 和
`source`，解析失败时提供 `error_code`。Provider 使用这些字段调用
`agent-sec-cli skill-ledger check DIRECTORY`，期限受剩余预算限制。
daemon 管理员须授权实际规范化目录并保证文件可访问，AW 不扩展目录授权或签名。
内容评审、扫描和签名通过 [Skill Ledger 指南](../../../../user-guide/zh/agent-sec-core/skill-ledger.md)单独完成。

| Ledger 状态 | 原因码 | `mode: block` |
| --- | --- | --- |
| `pass` | `skill_pass` | 继续原生加载 |
| `none` | `skill_unsigned` | 阻断 |
| `drifted` | `skill_drifted` | 阻断 |
| `warn`、`deny` | `skill_warn`、`skill_deny` | 阻断 |
| `tampered` | `skill_tampered` | 阻断 |
| `error`、非法或空输出、传输故障、超时 | Provider 错误 | 按 `on_error` 处置 |

Ledger 退出 1 时可能携带已完成的负向信任判定，Provider 按该状态处理。
非零退出却报告 `pass`、未知状态、信号退出或其他退出码均记为错误。
AW 审计分别记录风险判定与检查失败。`mode: observe` 只记录信任状态，
`ask` 尚未支持。同一项检查选择 AW 或原生 Skill Ledger Hook，避免重复执行；
原生权限拒绝继续有效。

Skill 校验使用普通常驻 `aw run`，`--reloadable` 接入会拒绝该配置。普通常驻
策略的空闲更新沿用既有任务快照边界。仅选中 Skill 校验时执行解析与对象固定，
其他策略保留原生 Skill 行为。

Qoder SDK 1.0.56 / Worker 1.1.78 从 Query 的 `settingSources` 所启用的目录中
校验唯一的本地对象，包括显式启用的 `.agents/skills`。frontmatter `name` 用于显示，
嵌套目录使用末级目录名。多个目录出现同名对象时，包括仅大小写不同的名称，均返回
`skill_resolution_error`。原生解析器可能跳过其中某个对象；控制接口没有提供最终
选中的路径，因此这些冲突也会被拒绝。可使用唯一名称或限制启用来源来消除冲突。
AW 在检查前刷新原生缓存，并在刷新和 Provider 执行期间核对源文件变化。刷新响应
是 UI 列表，`user-invocable: false` 等模型专用 Skill 可正常参与检查，后续调用继续
遵循原生准入与权限规则。已启用的 user/project 目录中存在本地 Workflow 脚本时
返回解析错误，覆盖文件名与元数据名称不同的脚本及目录入口 `workflow.js`/`index.js`。
JavaScript 元数据可引入竞争 Skill，公共接口无法证明最终选中的签名根目录；
配置必需 Skill 检查时须移除这些资产或排除其来源。原生 Workflow 关闭时也采用
这一保守边界，因为控制接口没有暴露其实际开关状态。
原生 `Read` 可填充动态 Skill 缓存，`reload_skills` 不会清除它，控制接口也不提供
选中的路径。Query 尝试过 `Read` 后，启用项目来源的 Skill 检查返回
`skill_resolution_error`，Read 失败、被拒绝或后续空闲更新启用检查时也适用。
必需检查应新建 Query，或从创建时使用 `settingSources: ["user"]`；
再次读取来源与更新策略均不会清除此限制。
旧命令资源、动态祖先目录冲突、附加目录或无法解析本地
目录的来源返回解析错误；带插件命名空间的名称和内置 Skill 尚未支持。
后续原生改参沿 Adapter 已有的公开 Hook 输入快照合同处理。

QwenPaw 2.2.2.post1 / AgentScope 2.0.10 使用实际 SkillViewer 与已激活的 Toolkit
分组，按原生同名优先级选取对象。AW 核对内存中的 markdown 与 `SKILL.md` 一致，
校验规范化目录，并为本次调用固定该对象。校验期间修改源文件会阻断加载。
不可访问或仅存在于内存的来源返回解析错误；并发调用各自保留快照。
AW 对已提供的文件沿用 AgentScope 的 UTF-8-SIG 解码，支持 BOM。原版 QwenPaw
2.2.2.post1 App 的运行时缓存会在 AW 执行前跳过 BOM 资产，这条原生加载路径仍不支持。
通过读文件加载、自动注入上下文和 Skill 内部命令的执行需要各自配置策略。

## Trace 关联

bridge 通过 JSON `--trace-context` 参数向 sec-core 传递原生 ID，`agent_name` 使用 Adapter 名称。从 `event.native` 读取 `trace_id`、`session_id`、`run_id`、`call_id` 和 `tool_call_id`，接受 snake_case 与 camelCase 别名。OpenClaw 依次读取 `native.event`、`native.context`；Hermes 依次读取 `native`、`native.extra`。
Qoder、Qoder SDK 和 Claude SDK 优先使用原生 `tool_use_id` 标识工具调用；Pi、QwenPaw 和 AgentScope 使用 `native.tool_call.id`。缺失的原生 ID 保持缺失，AW 生成的 ID 独立保留。
字段须为字符串，去除首尾空白后忽略空值；每项最多 256 个 Unicode 字符，包含超长时追加的 `...[truncated]`。字段优先级见 [trace_context.rs](../../../../../providers/agent-sec-core/src/trace_context.rs)。

## 本地验证候选效果

在 `src/aw` 中编辑好 `aw.sec-core.yaml`，将 `mode` 改为 `block` 后执行：

```bash
./target/debug/examples/host aw.sec-core.yaml tool.before Bash '{"command":"echo safe"}'
./target/debug/examples/host aw.sec-core.yaml tool.before Bash '{"command":"rm -rf /"}'
./target/debug/examples/host aw.sec-core.yaml tool.after Bash '{"command":"echo safe"}'
```

这些命令提交合成工具事件，不执行待扫描代码。使用内置 regex 规则时，预期候选效果
分别为 `code_pass`、`block/code_risk` 和 `tool_observed`。Host 输出的
`adoption: not_tested` 表明阻断候选效果尚不能证明 Agent 阻止了工具执行。
原生工具前后接线和持久化 AW 审计仍需 daemon 与适配器完成。

禁用时移除 Provider 及引用它的事件步骤即可，无需回退 sec-core 代码或打包改动。
已有 Agent 原生 sec-core Hook 继续使用相同的公开 CLI。
