# 应用自主管理的 Runtime SDK

[English](../../../en/aw/deployment/native-sdk.md)

应用自主管理启动并需要 AW 策略与生命周期管理时，可使用 `@anolisa/aw-sdk` 或异步
`anolisa-aw-sdk` Python 包。两种客户端均支持无框架依赖的语义回调；Qoder Hook 与
QwenPaw Middleware 保留为可选原生集成。要求 Linux，Node 包要求 Node.js 22+。
AW 与 Provider 分别安装。当前交付源码构建候选，registry 发布沿 AW 发行流程。

AW 应与语言包采用同一候选源码版本构建。未提供 `aw-sdk/v1` 的既有发行版会拒绝
连接；协作式控制还要求 `aw-sdk-control/v1`。安装语言包不会升级 AW。

## 通用应用接入

TypeScript 与异步 Python SDK 共用注册、Ready、Task/Scope、Provider 执行及审计合同。
应用配置使用 `adapter: sdk`，能力声明使用 `version: aw-hook-source/v1`、
`entrypoint: application`，按实际安装边界声明事件与效果。应用自行启动，AW Service
和 Provider 独立部署；该 profile 不提供 `aw run` 启动器。

配置名为 `app` 的目标并保留应用普通 `argv`，安装候选包后设置 `AW_SOCKET`，可运行
[TypeScript 示例](../../../../../src/aw/sdk/typescript/examples/application.mjs)或
[Python 示例](../../../../../src/aw/sdk/python/examples/application.py)。示例在工具执行前
调用 `task.emit`，采用阻断决定，执行后提交结果观察并释放 Task；具体决定来自配置策略。
`install(routes)` 确认所有路由已安装后才进入 Ready。应用保留原生权限检查，等任务
执行结束后再释放 Scope。

通用入口支持输入提交、工具调用前和工具调用后，字段与效果见
[通用语义入口](../../../../../src/aw/docs/design/zh/runtime/native-sdk.md#通用语义入口)。
具有这些语义的应用可直接复用，无需新增框架专属 Rust Adapter。原生 Hook 或可选
插桩来源可向同一入口供给事件；自动发现与覆盖选择属于后续交付。
Qoder 的丰富原生效果继续使用下述集成，Python/QwenPaw 见 [Python SDK](python-sdk.md)。

生命周期 RPC 默认超时 1000 ms；回调 RPC 至少容纳准入事件预算加 1000 ms 传输余量。
Service 在有序步骤间持续使用同一事件截止时间。

## 安装与连接

在应用目录安装候选 tarball：

```bash
npm install /absolute/artifacts/anolisa-aw-sdk-0.1.1.tgz
```

使用应用用户持有的私有目录中的绝对 socket 路径。同用户运行 AW，完整配置中设置
`agents.qoder.adapter: qoder-sdk`。选择 `native_fallback` 时，所有已启用事件均需
设置 `required: false`；Provider 的明确拒绝继续生效。独立执行
`aw service serve --config /absolute/aw.yaml`，使用该服务报告的 socket。
SDK 不启动服务或 Agent。

安装包中的 `examples/qoder.mjs` 给出完整连接、Query 与清理流程。沿用业务原有的
认证、daemon、模型、权限、配置来源和 Hook。将现有选项传给
`qoderOptions(task, options)`，创建原生 Query 后立即通过返回对象执行
`bind(query)`。每个 Query 使用独立 task，先关闭 Query，再关闭 task；应用退出时
关闭 Instance。同一 Query 只使用一种 AW 接线方式，避免与原 AW Query factory 重复。

创建 `Instance` 时显式传入 endpoint、target、capabilities 和 availability。
`cwd`、`environment` 默认采用应用值并构成任务上下文，原生 Query 必须使用同一目录。
其他工作目录使用另一个 Instance。`install(routes)` 仅在应用已安装对应事件路由时
返回 `true`；登记及每个任务的策略快照均在 Ready 确认前调用它。
示例将所有 Query 路由到 `qoderOptions`，遇到未知事件拒绝确认。

`await instance.connect()` 执行一次有界连接尝试。只有 native-fallback 下的传输不可用
或登记尚未完成时返回 `false`。`instance.start()` 启用单一后台重连流程，通过
`state`、`lastError` 查看就绪状态或明确错误。`stop()` 停止重连；`close()` 进一步
释放本应用的 Scope 和登记。这些调用保留共享 Service 与原生 daemon 的生命周期。

Ready 记录应用对路由安装的确认；后续每个 Query 是否实际安装 Hooks、是否采用返回效果，
仍由应用接线和原生验收证明。

## 可用性与任务归属

| 模式 | 服务缺席或连接丢失 | 明确错误或拒绝 |
| --- | --- | --- |
| `required` | 操作失败 | 保留错误或拒绝 |
| `native_fallback` | 有界检测后返回中性 AW 回调 | 保留错误或拒绝 |

生命周期 RPC 默认超时 1000 ms；回调等待时限覆盖准入事件预算与传输余量。
首次传输故障使当前连接退役。`native_fallback` 的活动任务固定为回退，后续回调立即返回；
`required` 的活动任务保持失败，恢复前拒绝新受保护任务。两种模式的后台重连均继续重试
传输不可用；明确的协议或配置错误停止重连。恢复时重新登记并确认 Ready，仅新任务使用恢复后的登记。不重放回调、工具或结果未知的
Provider 调用。已经采用的输入／结果改写继续保留。

可向 `instance.task({signal})` 或 `task.invoke` 传入 `AbortSignal`，用户取消继续传播。
协议不匹配、输入无效、配置拒绝、远端期限错误与 Provider 失败继续报错。
Qoder before Hook 失败拒绝工具，post Hook 的 Provider 失败停止 Query，并保存在
`integration.failure`。传输回退保持 Query 运行。消费结束后检查 `integration.failure`，
向业务暴露策略执行错误。

Service 仍可达时，回调 RPC 失败或取消会先清理该事件，再传播错误。原生代码在有序步骤之间提前结束时，
调用 `await task.closeEvent(hook, originalPayload)`。清理使用原 Scope 和独立的有界请求，
即使调用方的 signal 已取消也会执行。关闭成功后保留 Task，并拒绝该事件的迟到步骤；
正常完成的事件继续由 Service 自动退休。

## 协作式 attach 与 detach

应用调用 `instance.listen()` 发布已安装的 SDK 接线，可先于 AW 启动。配置的 Service
可用后，列表显示该应用；收到 attach 前保持未接入。使用
`instance.listen({automatic: true})` 同时启用自动登记与 CLI 控制。
后台流程在 `start()` 和 `listen()` 中选择一种；重新监听前等待 `await instance.stop()` 完成。两种 attach 方式复用同一登记和 Ready
路径；应用仍可在启动监听前调用 `connect()` 显式完成初始化。

使用应用的同一用户执行以下命令，将第一条命令返回的准确 `application_id` 填入变量：

```bash
AW_SOCKET=/absolute/private/aw.sock
aw instance list --socket "$AW_SOCKET"
AW_SDK_INSTANCE=application_id_from_list
aw instance attach --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
aw instance detach --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
```

应用确认 Ready 或释放空闲登记后，结果才显示 `completed`。存在活动任务时 detach 返回
`sdk_control_busy`，任务结束后再操作。选定实例脱离后，在 Service 恢复时仍保持未接入，
直到下一次 attach。既有回退任务继续原生运行，新任务使用恢复后的登记。detach 保留原生
应用与 Service；`await instance.stop()` 撤下发现记录并停止轮询，`close()` 还会释放本实例的任务与登记。

同一 PID 内的实例具有不同 ID。Service 重启后发现 ID 改变，操作前重新 list。
未运行 SDK 监听的应用无法 attach。私有 socket 仅接受 Service 同用户连接，应用确认还须
通过 PID／启动时间身份核验。CLI 不能改变应用选择的可用性模式或远程安装 Hook。

`--timeout-ms` 默认 5000，范围为 100–30000。超时或 CLI 中断代表确认结果未知，后续操作前
先通过 `instance list` 检查。Service 在期限后使保留的请求过期，迟到 Ready 无法激活该请求。
列表提供最近请求结果和已接入登记；普通 `aw service status` 也展示 SDK Ready 与 owner。

## 已验证范围

初版 Qoder 接线使用公开 `UserPromptSubmit`、`PreToolUse`、`PostToolUse` 和
`PostToolUseFailure` 回调，效果处理复用 Adapter。原 Hook 分组保留，AW 回调追加；Worker 可以并发调度分组。AW 检查收到的回调快照，
其他原生 Hook 后续改写参数的场景尚不具备最终输入保护保证。
SDK 1.0.56／Worker 1.1.78 验收覆盖真实 Bash、原生 Hook 共存、Query 中途 AW 退出、
新任务恢复接入、工具副作用恰好一次，以及 AW／原生拒绝时无工具副作用。包顶层不导入 Qoder，不读取私有 binding 文件。
其他框架版本按声明能力协商，原生验收结论对应实际测试版本。

原 resident `aw run` 路径保留更完整的托管 MCP、凭据与固定 Worker 接点。
当前公开 Hook SDK 尚未提供这些附加托管能力。原生 MCP 配置继续由应用选项传入。
Skill 检查沿用 Adapter resolver，并要求对应原生 Query 接口。
输入／结果改写与 Skill／MCP 组合需要单独完成原生验收后再进入业务采用。

归属与 wire 合同见 [SDK 协议](../../../../../src/aw/docs/design/zh/runtime/native-sdk.md)；
现有 required 入口见 [Qoder 指南](../agents/qoder-sdk.md)。
