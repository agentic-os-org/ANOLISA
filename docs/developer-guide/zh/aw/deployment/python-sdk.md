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
新的原生插件目录，在应用配置中启用 `plugins.aw-sdk`。`AW_SOCKET` 指向独立 Service
的私有 socket，AW 配置声明名为 `qwenpaw`、Adapter 为 `qwenpaw` 的目标。
沿用原模型、插件、权限及配置来源。原生插件加载先安装 middleware，startup Hook
发布协作式接入能力后返回，显式 attach 完成 AW 注册；shutdown 只释放本插件实例。
每个 App 使用一种 AW 接线，避免重复策略回调。

示例通过 `QwenPawPlugin.ready` 实现 `install`，确认 middleware 已登记并校验返回的路由。
自定义集成同样应在安装回调后才返回 `True`。客户端会对每个任务的不可变策略快照
重复确认。每个 Agent reply 持有独立任务，Scope 保留到相关原生工具生成器也完成；
不同 Agent 实例的任务独立。

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

## 协作式控制

原生回调安装后调用 `instance.listen(automatic=False)`，发布初始未接入的应用；
`automatic=True` 同时启用自动接入与 CLI 控制。QwenPaw 插件默认等待显式 attach。正常启动 `qwenpaw app` 后，选择发现列表中的 ID
执行链接中的 attach 命令。`QwenPawPlugin(instance, automatic=True)` 会在启动阶段等待
首次连接，随后自动重连。
`application_id` 提供发现 ID。命令、请求期限、空闲脱离和重启规则沿用
[协作式控制](native-sdk.md#协作式-attach-与-detach)。
`await instance.stop()` 撤下监听，`await instance.close()` 释放应用持有的资源。

## 已验证范围

默认 gate 在仓库外构建、安装 wheel，检查顶层导入无框架依赖，并运行共享生命周期、传输与协作控制测试。真实 App 验收采用 QwenPaw 2.2.2.post1／AgentScope 2.0.10、控制模型、原生配置
和 observer 插件，核对服务缺席／在线、失联、恢复、AW 明确拒绝、公共原生事件与 shell
副作用恰好一次。其他版本按能力协商，验收结论对应实际测试版本。

原生 Skill、输入／结果改写、后台 offload 与托管 MCP 组合仍需单独验收后进入业务采用。
现有 required 入口及其更完整接点见 [QwenPaw 指南](../agents/qwenpaw.md)。
