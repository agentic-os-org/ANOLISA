# sec-core Provider 接入参考

[English](../../../en/aw/providers/sec-core.md)

本页用于组件接入与维护。安装和日常使用请阅读[用户指南](../../../../user-guide/zh/aw/security.md)。

`aw-provider-sec-core` 让 AW 策略调用已有的 `agent-sec-cli scan-code` 命令。
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
| `mode` | 必填，`observe` 或 `block`，只影响 `scan_code` |
| `unmapped_tool` | 可选，`error`（默认）或 `observe`，控制 `scan_code` 对已选中但没有映射的工具的处理 |
| `tools` | 必填，包含 1–128 个规范化工具名称的对象，按名称精确匹配 |
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
