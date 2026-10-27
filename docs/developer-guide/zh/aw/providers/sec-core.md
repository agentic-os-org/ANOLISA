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

## 包内后台与显式健康诊断

Preview sec-core Provider 包包含同一次源码构建的 `agent-sec-cli`、
`agent-sec-daemon` 和 `agent-sec-daemon-health`。拆分包和 all-in-one 均通过
组件回执校验它们的哈希和执行权限。官方 systemd/配置模板作为参考资产放在
`share/aw/providers/sec-core/`；安装 AW 包不会注册服务或修改 `/etc`。

已有 root `agent-sec-core.service` 时复用其 socket。新后台按照
[sec-core 安装指南](../../../../user-guide/zh/agent-sec-core/index.md)安装，服务生命周期
由正式系统包负责。目前组件清单声明 Linux x86_64；本地 AW ARM64 包不代表
sec-core 已有正式 ARM64 发行。源码 `make install-core-v2` 可通过
`V2_BIN_BUILD_DIR` 读取本包二进制，但会覆盖目标系统二进制和 unit，不能直接用于
已有受管理安装。root 服务的执行路径不得指向用户可写的 AW 前缀。

安装器或诊断消费者可显式查询包内 helper：

```bash
AW_SEC_PROVIDER="$AW_PREVIEW_PREFIX/libexec/aw/providers/sec-core"
"$AW_SEC_PROVIDER/aw-provider-sec-core" --health \
  --helper "$AW_SEC_PROVIDER/agent-sec-daemon-health" \
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

AW 卸载只移除本包回执拥有的 helper/模板，保留独立管理的系统服务、`/etc`
配置及后台数据。后台升级/移除交给拥有它的系统包；AW 暂无自动服务安装、
升级或卸载衔接。

## 配置 Provider

复制[完整示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/agent-sec-core/examples/aw.yaml)为
`aw.sec-core.yaml`，替换可执行文件与 socket 路径。示例使用 `mode: observe` 和
`on_error: report`，本地评估不会请求阻断。before 步骤通过 `native:qoder:shell`
和 `native:qoder:python` 选择合成的 Qoder `shell`、`python` 事件；
其他工具跳过该步骤，不调用此 Provider。
同时配置步骤和事件选择条件时，两者都须匹配。
必需的安全检查使用 `mode: block`，并在 before 步骤中准入 `[observe, block]`、
设置 `on_error: block`。缺少映射、无效提取输入和后端失败均通过 Host 的错误策略
阻断选中的调用；扫描器发现的风险保留独立的 `code_risk` 结果。

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
./target/debug/examples/host aw.sec-core.yaml tool.before shell '{"command":"echo safe"}'
./target/debug/examples/host aw.sec-core.yaml tool.before shell '{"command":"rm -rf /"}'
./target/debug/examples/host aw.sec-core.yaml tool.after shell '{"command":"echo safe"}'
```

这些命令提交合成工具事件，不执行待扫描代码。使用内置 regex 规则时，预期候选效果
分别为 `code_pass`、`block/code_risk` 和 `tool_observed`。Host 输出的
`adoption: not_tested` 表明阻断候选效果尚不能证明 Agent 阻止了工具执行。
原生工具前后接线和持久化 AW 审计仍需 daemon 与适配器完成。

禁用时移除 Provider 及引用它的事件步骤即可，无需回退 sec-core 代码或打包改动。
已有 Agent 原生 sec-core Hook 继续使用相同的公开 CLI。
