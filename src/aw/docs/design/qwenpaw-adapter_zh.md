# QwenPaw App 适配器

[English](qwenpaw-adapter.md)

AW 通过原生 AgentScope 中间件，将 QwenPaw App 接入共享的 AW 服务。支持的运行时为
QwenPaw 2.2.2b4 和 AgentScope 2.0.8。启动器负责启动新的 App 进程，不接管已经运行的服务器。

## 原生边界

插件为每个通过能力准入的 AW 步骤注册一个 `on_acting` 中间件。原生注册表按优先级排序，
同优先级保留注册顺序。适配器采用优先级 100，将 after 包装放在 before 包装外层。
因此 before 按配置顺序执行，after 按配置逆序执行。AW 阻断产生的拒绝结果也会经过外层
after。既有中间件保留原生位置和行为。

独立工具调用继续由 AgentScope 决定串行或并发，AW 不增加全局工具锁。流式中间项
`ToolChunk` 原样通过，after 只接收终结的 `ToolResponse`，保留原生 success、error、
denied 和 interrupted 状态。

每次回调携带原生会话标识和工具调用标识。命令输入中的 `tool_call.input` 保持原生 JSON
字符串形式，归一化 Provider 事件则将其解析为 JSON。`tool_response` 保留原生数据。
服务用这些标识关联步骤，使同一事件共享一个期限，并拒绝重复或内容不一致的调用。

QwenPaw 没有定义命令 Hook 返回协议。在此适配器中，退出 0 表示观察并继续，工具前退出 2
返回原生拒绝结果。其他非零退出、信号、回调启动失败和超时属于回调错误。适配器记录错误并
应用 `on_error`：`report` 继续工具或结果传递，工具前的 `block` 返回拒绝结果；宿主取消仍向外
传播。标准输出不改写工具调用或结果。显式 `action: ask`、`action: approve` 和
`decision: ask` 产生不支持审批的错误，并按同一失败策略处理，不会打开审批界面。
原生权限检查仍先于 `on_acting` 执行。

## 安装与就绪

原生工作目录、profile、凭据和既有插件由 QwenPaw 管理。AW 只为本次启动安装自己拥有的
`plugins/aw-native` 目录，在所启动的 App 退出后移除该目录。同名目录已经存在时明确报错。
启动时必须明确原生工作目录；AW 不初始化模型，也不复制凭据。

插件在注册工厂前校验配置与运行时版本。工厂构造不执行配置 I/O，因为原生构造器遇到
工厂异常时只记录日志并跳过该中间件。原生 startup hook 在 App 插件加载阶段完成后才写入
私有就绪回执。启动器核对回执中的本次启动 token、适配器和 Hook 数量；未在期限内确认
就绪时，停止自己启动的进程。回执提供同用户的安装证据，不是进程身份认证。

就绪只证明插件已加载，不代表操作系统层面的强制防护。原生 App 可能在通用插件加载完成前
公布核心服务就绪状态，因此应在 AW 输出插件就绪信息后再发起请求。App 自动重载与多 worker
启动不在本适配器范围内。

## 覆盖范围

支持 App/API 入口。固定版本的 ACP 和 TUI 入口不加载此外部插件，因此在启动前拒绝。
原生权限已经拒绝的调用、交由外部执行的工具，均在进入 `on_acting` 前返回，不产生 AW 的
before/after 回调。没有形成原生终结响应的异常，也不会被补成虚构的 after 事件。

首期支持 before `observe/block` 与 after `observe`，不实现审批、输入或结果改写，以及
OS 强制的最后一道检查。Provider 命令使用 AW 实例的启动目录，QwenPaw 继续管理各工具的工作目录。
原生命令步骤接收实际回调环境，包括 QwenPaw 加载 profile 后的环境变化；结构化 Provider
继续使用启动时绑定的环境。

## 源码与验收

适配依据为固定版本的官方
[QwenPaw 插件 API](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/plugins/api.py)、
[App 启动流程](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/app/_app.py)
和[中间件构造](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/runtime/builder.py)。
原生测试不调用模型，直接验证已安装 AgentScope 的中间件链，覆盖共存、嵌套顺序、并发、
流式结果、阻断和启动回执。App/API 验收使用确定性的本地模型服务，验证真实工具执行、
Provider 阻断、after 观察、既有插件共存及清理，不代表云模型配置或浏览器控制台已验收。
