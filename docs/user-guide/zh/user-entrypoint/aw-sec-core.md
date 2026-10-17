# 通过 AW 使用 sec-core 扫描

[English](../../en/user-entrypoint/aw-sec-core.md)

`aw-provider-sec-core` 让 AW 策略调用已有的 `agent-sec-cli scan-code` 命令。
配置指定哪些工具输入包含代码，以及发现风险后返回观察还是阻断候选效果。
sec-core 提供扫描器与规则，AW 负责配置、协议校验和有界调用。

当前交付为 Linux 源码，可用于本地 Provider 集成。完整 Preview 集成见下方指南。Provider 不启动任何 daemon，不安装 Agent Hook，也不持久化 AW 审计。
工具后仅返回观察记录，不扫描或替换结果。

完整的 AW 服务、Qoder 和 sec-core 预编译集成见 [Preview 包指南](aw-preview.md)。
以下仍可单独从源码使用 Provider 桥接。

通用的组件接入路径见 [AW 教程](../../../developer-guide/zh/aw/integration.md)。sec-core
CLI 和 daemon 仍是独立管理的后端，AW 审计存储由 AW 服务负责，见
[容器持久化说明](aw.md#容器与持久化数据)。Preview 生成器映射 Qoder `Bash` 和
OpenClaw `exec` 输入。使用 QwenPaw 或 Hermes 前，须在 `config.tools` 中补充并
验证其实际工具名称和输入指针；未映射工具只观察，不扫描。

## 前置条件与构建

按 [sec-core 指南](../agent-security/agent-sec-core/QUICKSTART.md) 安装 sec-core。
本桥接要求 Rust V2 CLI 支持 `--socket`、`--timeout-ms` 和
`scan-code --code --language --mode`，且已有可访问的 sec-core daemon 正在运行。
请核对部署版本的帮助；旧 Python CLI 未通过本桥接验收。
AW 不向 sec-core 添加命令或依赖。

从源码构建 AW Provider 与本地 Host 示例：

```bash
cd src/aw
cargo build --locked -p aw-provider-sec-core
cargo build --locked -p aw-host --example host
```

Provider 二进制位于 `target/debug/aw-provider-sec-core`，启动参数为：

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
[sec-core 安装指南](../agent-security/agent-sec-core/QUICKSTART.md)安装，服务生命周期
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
`src/aw/crates/aw-provider-sec-core/schemas/health-v1alpha1.schema.json`，
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

复制[完整示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider-sec-core/examples/aw.yaml)为
`aw.sec-core.yaml`，替换可执行文件与 socket 路径。示例使用 `mode: observe` 和
`on_error: report`，本地评估不会请求阻断。需要阻断时，将 `mode` 改为 `block`，
并在 before 步骤中准入 `[observe, block]`。`on_error: block` 单独控制执行故障时
是否请求阻断；执行故障始终不会被解释为扫描器发现风险。

Provider 的私有 `config` 接受以下字段：

| 字段 | 含义 |
| --- | --- |
| `version` | 必填，固定为 `1` |
| `mode` | 必填，`observe` 或 `block`，只影响 `scan_code` |
| `tools` | 必填，包含 1–128 个规范化工具名称的对象，按名称精确匹配 |
| `tools.<name>.language` | `bash` 或 `python` |
| `tools.<name>.input_pointer` | 指向 `event.tool.input` 内部的 JSON Pointer，所选值必须为字符串 |

未知字段、不支持的语言和非法指针会被拒绝。空指针选择根字符串，`/command`
选择字段，`/items/0` 选择数组元素。工具名区分大小写。未映射的工具返回
`observe/tool_unmapped`，表示没有扫描；已映射的工具缺少代码或代码不是字符串时，
返回 `invalid_tool_input` 错误。

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
