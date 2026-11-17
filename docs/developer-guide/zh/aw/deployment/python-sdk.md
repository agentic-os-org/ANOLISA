# 应用持有的 Python SDK

[English](../../../en/aw/deployment/python-sdk.md)

`anolisa-aw-sdk` 提供 Python 3.11+ 的 Linux 异步客户端与可选 QwenPaw 插件。
应用保留原生启动、配置、权限和调度。AW／Provider 分别安装，Service 使用应用用户
运行。当前 wheel 为源码构建候选，registry 发布沿 AW 发行流程。

AW 应与语言包采用同一候选源码版本构建。未提供 `aw-sdk/v1` 的既有发行版会拒绝
连接；协作式控制还要求 `aw-sdk-control/v1`。安装语言包不会升级 AW。

SDK 也支持无框架依赖的 `task.emit` 语义回调，见 [通用应用接入](native-sdk.md#通用应用接入)。

## 安装与接入

在应用环境安装 wheel：

```bash
python3 -m pip install /absolute/artifacts/anolisa_aw_sdk-0.1.1-py3-none-any.whl
```

顶层 `aw_sdk` 导入不加载 Agent 框架。`Instance` 接受绝对 `endpoint`、配置中的
`target`、原生 `capabilities`、显式 `availability` 和 `install(routes)` 回调；
还可传入 `cwd`、`environment`、`config`、`timeout_ms`、`reconnect_ms`。
上下文默认采用应用目录和环境，目录应与原生 Agent 一致。wheel 不包含二进制或策略引擎。

QwenPaw 接入时，将安装后的 `share/anolisa-aw-sdk/examples/qwenpaw/` 示例复制到
新的原生插件目录，启用 `plugins.aw-sdk`，并将 `AW_CONFIG` 设置为部署配置的绝对路径。
沿用 `qwenpaw app`、原模型、权限和其他插件。初始化复用 AW 权威离线解析器，
不启动 AW 或 sec-core。

```python
from aw_sdk.qwenpaw import QwenPawPlugin

plugin = QwenPawPlugin.from_config("/absolute/aw.yaml")
```

可将配置解析移至部署阶段：使用 `qwenpaw-installed-native-hooks` Profile 生成
[Descriptor](native-sdk.md#部署-descriptor)，再传入其绝对路径：

```python
plugin = QwenPawPlugin.from_config(
    "/absolute/aw.yaml", descriptor_file="/absolute/aw-qwenpaw.json"
)
```

此路径校验配置的准确字节，不调用 `aw`；Descriptor 错误会在注册原生 middleware 前
使初始化失败。

QwenPaw 专用入口同时尝试安装 Adapter 的插件隔离路径加速，仅适用于已验证的
QwenPaw `2.1.1b3`。安装不启动服务，保留原生隔离与清理；其他版本或接点不可用时
跳过加速。通过插件导入时，从安装后的加载过程开始生效，已经加载的插件保留此前耗时。
业务有早期 Python 启动入口时，可在 QwenPaw 加载插件前调用，让后续全部插件受益：

```python
from aw_sdk.qwenpaw import prepare_runtime

accelerated = prepare_runtime()  # Before QwenPaw starts loading plugins.
```

返回值表示加速是否已安装。重复调用安全，补丁持续到进程退出。这项 Adapter 可选优化
不改变 SDK 能力准入及后续 PID 协作 attach；`aw run` 复用同一实现。关闭 AW 插件并移除
显式初始化调用后，重启 App 即恢复原生解析。

只有一个 `qwenpaw` 目标时自动选择，多个目标时传入 `target`。
`daemon.endpoint` 或 `daemon.state_dir` 采用明确的私有路径。
配置决定最早保护边界：启用 `input.submit` 时，每个 reply 在模型派发前等待 Ready 和 Scope；
仅配置 `tool.before`／`tool.after` 时，模型调用可与部署准备重叠，首次工具进入前等待
Ready 并固定 Scope。内部准入门不执行未配置的 before 策略；工具策略下的纯文本回复不建远端 Scope。

HTTP 就绪后，部署方准备 sec-core，再执行
`aw instance attach --config /absolute/aw.yaml --sdk-pid APPLICATION_PID`。
此协作式选择需要已安装插件。配置式 resident 路径采用 required 可用性，默认等待
5000 ms，支持工具前阻断与工具后替换／扣留；原生生成器、后台任务和最终结果交付
沿用 resident 保障。每个 App 使用一种 AW 接线。
初始未安装输入接点时，增加 `input.submit` 需要排空并重启，再重新 attach；已发起的模型
请求无法补做输入检查。普通策略更新继续沿用空闲更新门。

需要显式管理公开 middleware 时，`QwenPawPlugin(instance)` 保留既有 Instance API、
可选可用性和每个 reply 的 Task 归属，`ready` 对登记和 Scope 确认实际安装的路由。
上述配置工厂额外复用 resident 结果保障，按所需原生效果选择入口。

## 异步生命周期与可用性

`await instance.connect()` 执行一次连接尝试；`instance.start()` 启动单一后台重连流程。
`state`、`last_error` 暴露就绪状态和明确错误。`await instance.stop()` 停止重连；
`await instance.close()` 释放任务和登记，原生 App 与共享 Service 继续按各自归属管理。
直接集成可用 `await instance.task()` 创建任务，通过
`await task.invoke_native(hook, payload)` 调用原生路由，在 `finally` 或 `async with`
中关闭任务。结果为 `(code, stdout_bytes, stderr_bytes)`。

显式模式及策略约束与 [TypeScript SDK](native-sdk.md#可用性与任务归属)一致。
`native_fallback` 要求全部已启用事件设置 `required: false`。传输缺席／丢失经首次
有界检测后使当前任务固定回退，后续回调立即返回中性结果；恢复仅影响新任务。
生命周期 RPC 默认超时 1000 ms；回调 RPC 至少容纳事件预算加 1000 ms 传输余量，
Service 保留同一事件截止时间。
结果未知的回调和原生工具均不重放。

`asyncio.CancelledError` 保留取消语义。配置／协议错误、Provider 失败和明确拒绝均不转为
传输回退。before 拒绝阻止工具进入，post 处理错误沿原生任务传播。
当前公开 middleware 尚未提供 resident Coordinator 的严格流式结果扣留、托管 MCP／凭据
接点。其他 middleware 在 AW 之后改写参数的场景尚不具备最终输入保护保证。

`required` 模式传输失联后退休连接，已有任务保持失败；后台重连继续，仅新任务使用恢复后的登记。
Service 仍可达时，回调取消或失败先等待事件清理，再传播原异常。原生步骤之间提前结束时，调用
`await task.close_event(hook, original_payload)`，随后可在同一 Task 内执行其他事件。
原生生成器清理完成后再关闭 Task。

使用 required 可用性及调用账本的原生 Source，在激活后可调用
`await task.watch_scope(probe=..., on_failure=...)`。SDK 在模型和工具等待期间续租，
向 owner 锁存桥接失败，并在原生排空后关闭 Task 时停止、汇合监视任务。
配置式 QwenPaw 插件复用这项公共 Task 能力。

## 协作式控制

原生回调安装后调用 `instance.listen(automatic=False)`，发布初始未接入的应用；
`automatic=True` 同时启用自动接入与 CLI 控制。QwenPaw 插件默认等待显式 attach。正常启动 `qwenpaw app` 后，选择发现列表中的 ID
执行链接中的 attach 命令。`QwenPawPlugin(instance, automatic=True)` 会在启动阶段等待
首次连接，随后自动重连。
`application_id` 提供发现 ID。命令、请求期限、空闲脱离和重启规则沿用
[协作式控制](native-sdk.md#协作式-attach-与-detach)。
`await instance.stop()` 撤下监听，`await instance.close()` 释放应用持有的资源。

## 工具后的延迟接入

实验性 `AfterOnlyPlugin` 保留原生 `qwenpaw app` 启动，启动阶段不等待 AW 或 sec-core。
从已安装 wheel 的 `share/anolisa-aw-sdk/examples/qwenpaw-after/` 复制到新的原生插件目录，
启用 `plugins.aw-after`，将 `AW_SOCKET` 设置为 AW 配置选择的私有 socket。
该应用使用这一种 AW 接线，替代可选的 `aw-sdk` 插件；原模型、权限和其他 Hook 沿用业务配置。

启用插件后执行原生命令。HTTP 就绪后，通过 sec-core 既有部署入口启动／预热后端，
再从另一个终端使用应用的实际 PID 接入：

```bash
aw instance attach --config /absolute/aw.yaml --sdk-pid APPLICATION_PID \
  --timeout-ms 10000
```

配置 `daemon.startup: on_demand` 后，`--sdk-pid` 配合配置文件会按需启动或复用 AW，等待应用发布 SDK 接线，并按准确 PID／
启动身份选择一个实例。同进程存在多个 SDK 实例时改用 `--sdk-instance` 精确选择。
命令等待应用确认 Ready，不向任意进程注入，也不启动 Agent。传入 `--socket` 时仅连接
已有服务。CLI 不启动 sec-core，后端生命周期继续由部署方管理。

prompt 可在 attach 前进入模型。预装入口在首次工具前等待准备完成，创建 Scope 并确认
仅工具后策略快照，复用 resident 的调用账本、Coordinator 暂扣及审核后模型／事件交付。
只接受 `tool.after` 策略；内部 `tool.before` 观察路由用于关联，不执行工具前业务扫描。
`input.submit` 和已配置的工具前检查会被拒绝。无工具的文本回复无需等待 AW。
Scope 固定同一策略修订，直到原生工作完成清理。

插件固定使用 `required` 可用性。`attach_timeout_ms` 默认 5000，范围 100–60000；
过早到达的工具等待剩余准备，超时拒绝派发。扫描失败扣留未检查输出，HTTP 进程继续存活；
未知清理状态沿 resident 的失败围栏处理。不回退该任务，也不重放工具。
该插件的 `reconnect_ms` 默认 100，控制 SDK 发现轮询。可通过 `cwd` 指定实例工作目录；
每轮任务传入原生 workspace 和不含任务凭据的 Hook 环境。
直接 Python 接入也可使用 `instance.task(cwd=..., environment=...)`，上下文按任务复制。

停止时先排空活动任务、detach 选定实例，再按部署生命周期停止 AW／sec-core。
仅禁用 `plugins.aw-after` 并重启原生 App 即可移除本地接点，同时取消这层附加保护。
既有 `aw run` 行为保持。原生验收使用 QwenPaw 2.2.2.post1／AgentScope 2.0.10、
本地模型与替换 Provider 夹具；`after-only-sdk` 覆盖延迟接入、普通／替换结果、扫描失败、
脱离时纯文本回复及未接入时的有界拒绝。该验收不构成启动或业务时延承诺；原生与插件启动
差值、后端准备和首次工具剩余等待分别实测。

## 已验证范围

默认 gate 在仓库外构建、安装 wheel，检查顶层导入无框架依赖，并运行共享生命周期、传输与协作控制测试。真实 App 验收采用 QwenPaw 2.2.2.post1／AgentScope 2.0.10、控制模型、原生配置
和 observer 插件，核对服务缺席／在线、失联、恢复、AW 明确拒绝、公共原生事件与 shell
副作用恰好一次。其他版本按能力协商，验收结论对应实际测试版本。

原生 Skill、输入／结果改写、后台 offload 与托管 MCP 组合仍需单独验收后进入业务采用。
现有 required 入口及其更完整接点见 [QwenPaw 指南](../agents/qwenpaw.md)。
