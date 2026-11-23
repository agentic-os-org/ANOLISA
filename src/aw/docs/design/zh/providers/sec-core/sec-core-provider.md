# sec-core CLI Provider 边界

[English](../../../en/providers/sec-core/sec-core-provider.md)

`aw-provider-sec-core` crate 是一个具体的 AW Provider 二进制，不链接 sec-core
Rust crate，也不向 sec-core 引入 AW 依赖。默认（`--transport cli`）经公开
`agent-sec-cli` 进程接口调用；可选（`--transport daemon`）经 `aw_exec::uds`
通过公开 V2 换行分隔 JSON 协议直连本地 sec-core daemon Unix Socket。

```text
AW Host → aw-provider-sec-core ─┬─(--transport cli, 默认)─→ agent-sec-cli → sec-core daemon
        ← AW 效果/错误         └─(--transport daemon)─────→ sec-core V2 daemon Socket (UDS)
```

AW 协议处理 describe、私有配置校验、请求与摘要绑定以及效果准入。桥接负责精确工具名、
JSON Pointer 选取及显式 observe/block 阈值；sec-core 负责代码扫描、规则和 verdict
含义。Core 与 Host 无需理解扫描器字段。`observe_tool` 仅确认工具后观察，不扫描或
变换结果。

桥接读取有类型的 `ok` 和 `verdict` 字段，允许 CLI 增加其他输出字段。CLI 成功退出
只是判定有效的必要条件：`ok=true/pass` 表示通过，`ok=true/warn|deny` 表示风险。
非法或未成功的响应归为执行故障。stdout/stderr 与扫描详情均有大小上限，不会作为
AW 响应数据返回。Host 独立处理 `on_error`。

调用使用字面 argv、绝对 CLI 路径与 daemon 端点、选定语言、`--mode regex` 及剩余
`--timeout-ms`。不插入 shell、本地扫描器、服务启动、重试或旧 CLI 回退路径。
`--transport cli`（默认）面向公开 Rust V2 CLI；`--transport daemon` 经 `aw_exec::uds`
直接通过本地 sec-core V2 daemon Socket 执行 `scan_code`、`scan_prompt`、`scan_user_input`、
`scan_tool_input`、`scan_tool_output` 和 `check_final_tool_output`，发送不含客户端
`requestId` 的严格 `DaemonRequest` 帧，校验独立的 `32 KiB` 上下文与 `4 MiB` 业务载荷预算（总帧上限 `4 MiB + 32 KiB`），
携带 `TraceCarrierV1` / `CompatibilityV1`（`version: 1`）并校验 `SO_PEERCRED` UID `0`。
`check_skill` 仍依赖 CLI，未传 `--cli` 时从 `describe` 中省略；切回 `--transport cli` 即可回退。未来若需扩展字段，应表达通用扫描能力，不向 CLI
引入 AW 请求 ID 或事件类型。

`aw-exec` 管理内层 CLI 的期限、输出上限和进程组清理，CLI 继承宿主为 Provider
选定的环境。执行预算从请求解析前开始计算，上限 60 秒，在 EOF 到达后检查；
它不能中断任意阻塞的 `Read`/`Write` 实现。所有方法均要求调用方限制整个进程生命周期
及 stdio。`aw-host` 通过 `aw-exec` 提供外层期限，直接调用方必须关闭 stdin 并自行
终止停滞进程。Provider 不承诺独立调用的总耗时上限。清理使用执行器已有的一秒
额外预算。Linux 父进程死亡信号保证 Host 强制终止 Provider 时，直属 CLI
也会终止。这不构成所有后代的 OS 强制执行保证：逃逸进程组、权限切换以及 owner
被杀后无法核实清理仍是边界。已验收的 Rust scan CLI 不另起扫描进程，而是调用
已有 daemon；已派发的 daemon 工作由 sec-core 约束，终止客户端不会回滚这些工作。

私有配置与可运行的 Host 步骤见[用户指南](../../../../../../../docs/user-guide/zh/aw/security.md)。
示例提供完整 AWConfiguration，无需修改统一配置 schema。部署管理、原生 Hook
调度、效果采用与持久化 AW 审计仍由 daemon 和适配器负责。使用早期实验命令
`agent-sec-cli aw-provider` 的组合分支需要将 Provider argv 改为独立桥接。

## 提交扫描

`scan_prompt` 与 `scan_user_input` 消费非工具事件 `input.submit`。Adapter 负责新的轮次身份与原生处置
接点，Host 为全部步骤保留同一截止时间和单次声明。`--transport cli`（默认）下，
`scan_prompt` 将完整文本通过 stdin 交给公共 V2 `scan-prompt`，显式选择 fast 模式和用户输入来源；
`--transport daemon` 下，直接经 `aw_exec::uds` 发送单次有界的 `action.prompt_scan` V2 RPC
（`mode: "fast"`、`source: "user_input"`）。两种传输均同时校验完整性与判定；
Prompt 的 `ok=false` 配合 warn/deny 表示风险；扫描不完整或错误输出按
调用失败处理。用户 PII 复用共用 PII 传输（`--transport cli` 调用 `pii::call`，
`--transport daemon` 调用 `pii::call_daemon`），传入 `Source::UserInput`、相同 turn
上下文和剩余预算，风险按 `pii.input_mode` 处理，故障保留独立 Provider 错误路径。
AW 效果及元数据审计不包含后端发现详情和用户原文。配置、版本范围
及可执行原生验收入口见[Provider 参考](../../../../../../../docs/developer-guide/zh/aw/providers/sec-core.md#用户提交检查)。

## PII 边界

共用 `pii` 模块负责传输选择（`--transport cli` 下的 stdin-only CLI 请求 `pii::call`，
以及 `--transport daemon` 下直连 UDS 的严格 `action.pii_scan` V2 RPC `pii::call_daemon`）、
来源、剩余预算和严格结果解析，供工具阶段、`check_final_tool_output` 与用户输入消费者共用。
校验包括来源、字节覆盖、截断和已报告的执行/自定义规则错误。`tool_pii` 应用私有输入/输出模式
（以及 `security.violation` 上 `check_final_tool_output` 的阻断模式）。结构化值先按 JSON
扫描以保留字段和值的上下文，再逐个扫描解码后的字符串值和对象键，保留空白边界与自定义规则锚点；
普通字符串直接扫描。参数、完整原生结果和保留结构使用同一遍历逻辑，每次后端调用仅获本次调用的剩余预算。
输出命中后在私有副本中清空 Adapter 授权的唯一文本值，再用同一预算扫描保留结构。保留结构通过后才能返回固定安全提示替换；其他结构通过配置的输出故障动作处置。

Adapter 负责结果投影与采用。Qoder SDK 普通常驻 Post 返回 `updatedToolOutput`；失败工具回调保留原生错误语义，无法采用该指令。QwenPaw 在 Coordinator/公开交付前缓冲选中替换步骤的流。AW 仍只允许每个事件最后一步进行一次结果替换，PII 替换策略须占据该步骤。

## 验收边界

从仓库根目录执行。

`cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml`（以及 `--features testing --test daemon_transport --test daemon_e2e` 对直接 daemon 协议与真实 `agent-sec-daemon` 端到端链路）覆盖公开 CLI fixture、请求绑定、离线
操作、配置拒绝、精确 argv、判定与故障映射、预算和输出上限，以及 Host 取消时
内层 CLI 的终止，以及 stdin 保持打开、Provider 等待 EOF 时的外层期限清理。
常规 AW gate 同时运行这些测试与既有 Provider/Host/Core 测试。
真实 CLI/daemon 验收复用 Host 示例，使用隔离 daemon 路径与内置规则，独立于 fixture
测试和 Agent 原生验收。返回 block 候选效果不代表已经阻止工具执行。
