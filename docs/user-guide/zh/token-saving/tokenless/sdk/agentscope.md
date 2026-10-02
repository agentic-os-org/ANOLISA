# Tokenless AgentScope SDK 集成

[English](../../../../en/token-saving/tokenless/sdk/agentscope.md)

`anolisa-tokenless-agentscope` 是通用 `anolisa-tokenless` SDK 之上的 AgentScope 专用层。
它把 AgentScope 生命周期 API 映射到 `TokenlessSdk`，并依赖完全相同的包版本；它不单独
实现压缩。

选择通用 SDK 或 AgentScope 层时，先阅读 [Python SDK 概览](../sdk.md)。Claude Code、
OpenCode 等产品 Adapter 单独记录在 [Agent 集成](../framework-integration.md)。

## 支持的 AgentScope 版本

| AgentScope 版本 | 支持的入口 |
|-----------------|------------|
| 1.0.11 至 1.0.x | Tokenless Toolkit 加 `install(..., session_id=...)` |
| 2.0.0 至 2.0.2 | 通过 `integration.tools` 和 `integration.middlewares` 直接构造 Agent |
| 2.0.3 至 2.0.x | 直接构造 Agent，或通过 `integration.app_options()` 接入 App |

## 安装

AgentScope 集成 Wheel 要求原生 Runtime Wheel 的版本与它完全相同。请从同一个
Tokenless GitHub Release 同时安装两个产物。例如，在 Linux x86_64 上安装
[v0.8.4](https://github.com/alibaba/anolisa/releases/tag/tokenless/v0.8.4)：

```bash
python3 -m venv .venv
. .venv/bin/activate
python -m pip install \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.4/anolisa_tokenless-0.8.4-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl" \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.4/anolisa_tokenless_agentscope-0.8.4-py3-none-any.whl"
```

在 Linux aarch64 或 macOS Apple 芯片上，请按 [SDK 概览](../sdk.md) 替换原生 Runtime
URL，并保持两个包的版本完全一致。

### 从源码构建

也可以从源码 checkout 构建并同时安装两个相同版本的 Wheel：

```bash
make python-wheel agentscope-wheel
python -m pip install \
  target/wheels/anolisa_tokenless-*.whl \
  target/wheels/anolisa_tokenless_agentscope-*.whl
```

## AgentScope 1.x

AgentScope 1.x 使用 Tokenless Toolkit。普通工具与 MCP 注册入口也会覆盖构造后新增的
工具：

```python
from agentscope.agent import ReActAgent
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = integration.create_toolkit()
toolkit.register_tool_function(application_tool)
agent = ReActAgent(..., toolkit=toolkit)
integration.install(agent, session_id="conversation-id")
```

## AgentScope 2.x

构造 Toolkit 和 Agent 时传入恢复 Tool 和 Middleware。该方式从 2.0.0 开始可用，不依赖
后续补丁版本才引入的 Toolkit 动态修改 API：

```python
from agentscope.agent import Agent
from agentscope.tool import Toolkit
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
        # retrieve_tool_name="tenant_tokenless_retrieve",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = Toolkit(tools=[*application_tools, *integration.tools])

agent = Agent(
    ...,
    toolkit=toolkit,
    middlewares=integration.middlewares,
)
```

现有 `TokenlessMiddleware` 2.x API 继续保留兼容。新代码应使用 `TokenlessAgentScope`，避免依赖
特定补丁版本的 Toolkit 动态修改或 Tool 自动收集行为。

## AgentScope App

AgentScope App 从 2.0.3 开始支持。`app_options()` 会在配置的绝对基础目录下，为每个
user/agent/session 派生独立的 Tokenless 数据目录：

```python
from agentscope.app import create_app
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig

integration = TokenlessAgentScope(
    TokenlessConfig(data_dir="/srv/tokenless-tenants"),
)
app = create_app(..., **integration.app_options())
```

`app_options()` 只提供一个 Middleware Factory。AgentScope 通过该 Middleware 实例的
`list_tools()` 发布静态 Retrieve Tool，并在 `AgentState.middle_context` 中持久化 Marker 授权。

AgentScope 2.0.0 至 2.0.2 只支持直接构造 Agent；这些版本的 App API 尚未同时提供由
Middleware 发布 Tool 和持久化 Middleware 状态的能力。

## 配置与行为

如果应用已定义 `tokenless_retrieve`，应在 `TokenlessConfig` 中设置唯一的
`retrieve_tool_name`；App 组装阶段不会把其他工具暴露给该 Factory，无法预先检查重名。

集成为已知 AgentScope Shell、文件和 API 工具提供显式契约。每个自定义工具都必须通过
`tool_contracts` Map 注册。`ToolContract` 要求从 `COMMAND_OUTPUT`、`FILE_CONTENT` 和
`API_RESPONSE` 中选择一个 `ContentOrigin`；只有可能由 RTK 改写参数的 `COMMAND_OUTPUT`
契约才设置 `command_field`。未知自定义工具在 AgentScope 1.x 注册时或 AgentScope 2.x
Model 边界快速失败，绝不通过输出文本猜测 Origin。

`TokenlessConfig` 只包含 `data_dir`、`retrieve_tool_name` 和 `rtk_enabled`。压缩阈值、
内容检测、TOON 选择、错误诊断、Marker 授权和 Stash 策略都由 Rust Core 持有。

集成会原样转发中间流式 Chunk 并保留框架对象，只转换复制后的调用参数和最终模型可见
文本。Tokenless 优化失败或 UTF-8 结果没有严格变小时保留原文，`DataBlock` 永不修改。

集成还提供默认名为 `tokenless_retrieve` 的恢复 Tool。它的声明是静态的，并在模型调用之间
保留在工具列表中，因此 Marker 可见性变化不会造成工具列表抖动。它只接受当前 Model Call
精确保留的 Marker 集合中的完整 Marker 或 24 位十六进制 Hash。Retrieve 输出会绕过 PostTool。

每个用户或租户必须传入独立的绝对 `data_dir`。`TOKENLESS_DATA_DIR` 只是进程级回退，
不得由多个租户共用；也不要依赖跨节点恢复。Stash 条目使用当前固定的一小时 TTL。

两条 AgentScope 版本路径都启用 Schema 压缩、RTK 命令改写、响应压缩、TOON、恢复、
环境错误提示和逐调用归属。平台 Wheel 内置 RTK 并直接链接 TOON，不会搜索系统 Helper。
Tool Ready 仍保持硬关闭。

## 验证集成

在源码 checkout 中运行 installed-wheel 与支持版本矩阵测试：

```bash
make test-agentscope-integration
```

随后在应用中执行一次成功且可压缩的工具响应。确认 Middleware 返回更小的结果，并确认
`tokenless_retrieve` 可以从同一 `data_dir` 恢复 Marker 对应的内容。

## 相关文档

- [Python SDK 概览](../sdk.md)
- [Agent 集成](../framework-integration.md)
- [配置与数据隐私](../configuration-and-privacy.md)
- [故障排查](../troubleshooting.md)
