# 结构化 MCP 派发

[English](../../en/adapters/structured-mcp.md)

受管 SDK 边界在进程内 handler 或远端 `tools/call` 执行前授权实际 MCP 参数。
策略沿用 `tool.before`、`tool.after` 步骤及工具选择器。需要代码扫描和远端授权时，
将二者分别配置为步骤；Adapter 不添加隐式 Provider 调用。

## QwenPaw 2.2.2.post1 / AgentScope 2.0.10

AW 的 `aw-native` plugin 加载后，应用可将已连接的原生 `MCPClient` 转为受管工具：

```python
from agentscope.tool import Toolkit
from plugin_aw_native.aw_mcp import managed_mcp_tools

tools = await managed_mcp_tools(client)
toolkit = Toolkit(tools=tools)
```

将该 Toolkit 交给既有受管 Agent 工厂组装的 Agent。原生 client 的连接及关闭生命周期
由应用持有。helper 解析固定工具列表，发现结果变化时由应用显式刷新。
动态 `Toolkit(mcps=[client])` 发现保留既有原生行为，不安装此实际调用边界。

helper 调用实际 `MCPClient.list_tools()` / `get_tool()`，保留完整输入 schema、
annotations 和原生权限检查。resident 在 PreTool 前捕获已选工具；MCP 工具在
`ClientSession.call_tool()` 前再次比对 descriptor、连接、解析后参数及活动 invocation。
每个 invocation 允许一次 MCP 调用。身份缺失、参数或 descriptor 变化、重复进入、
取消及已退休的归属均失败关闭。无状态 client 在同一受管操作内创建原生 session；
有状态 client 保留既有 session。

`native.aw_mcp_context` 包含 `server`、原始 MCP `tool`、模型可见 `subject`、
`parent_tool_call_id`、以 scope 内 invocation 序号表示的 `generation`，以及
`identity_scope: model_tool`。这些字段来自实际原生工具及 resident 上下文，
不会通过模型工具名的字符替换反推 server/tool。Pre/Post 各由现有 resident 执行一次；
MCP 边界只核验实际派发，不重复调用阶段。

原生渲染保留文本、媒体和错误状态；完整 MCP `CallToolResult` 同时保存在
`ToolResponse.metadata.aw_mcp_result`，包括 `structuredContent`、`isError`、
内容块及扩展字段。完整回执经过既有 lease 脱敏器及 Post 屏障。选中的
`withhold_result` 失败会阻止交付及后续派发。取消通过原生 Coordinator 排空本地
client 调用；远端操作的取消及清理由远端 server 持有。

## Qoder SDK 1.0.56 / Worker 1.1.78

使用 AW 受管 Qoder bridge 既有的 `createSdkMcpServer()` 导出。工厂接受
`parentIdentity: "query"`（默认）或 `"required"`。固定 Worker 在 MCP 派发边界
未传递父模型身份，因此 `required` 在创建原生 server 前拒绝。

Worker sender 向控制通道传递 `{subtype: "mcp_message", server_name, message}`。
`SDKControlMcpMessageRequest` 使用同一字段；SDK 的
`QueryRunner.processControlRequest()` 将 `message` 转发到进程内 MCP transport。
这些合同提供 Query 归属及实际 server、tool、参数，未携带模型工具 ID。
需要父模型身份的接入必须在该派发点提供可信 Worker 生成的身份，并绑定到已登记的
原生模型 invocation 和代次。相同参数、命令拼写及派发顺序无法提供该证据。

`native.aw_mcp_context` 明确报告 `identity_scope: query`、
`parent_tool_call_id: null`、`parent_query_id` 及执行 `generation`。AW 登记新的
子 invocation，快照授权参数，核验原生 parser 派发与 schema 输出，每张 handler
票据仅消费一次。handler 函数在工厂构建时捕获。原生 MCP schema、annotations、
结构化错误及多模态结果 envelope 保留 SDK 语义。结果在 Post 前复制，后续变更无法
替换已检查结果。resident 在执行及交付前核验取消与代次，并在退休登记前排空原始
handler 的 finally。

受管工具的原生 AW 策略回调委托给实际 MCP 边界；应用原生 Hook 和权限检查继续执行。
配置的每个 AW 阶段只在实际调用点执行一次。非受管 server/tool 上下文沿用普通原生
策略回调。委托依赖固定 Worker Hook 中的 `mcp_context.server_name` / `tool_name`
识别受管工具；这些字段属于原生元数据，与模型提供的工具参数分开。后续 control 请求
仍须通过实际调用的身份及参数检查。原生元数据合同变化时，需要重新验证两个边界。
已安装的 `resident.executionOwner` 以私有执行 lease 包裹原始 handler
Promise，并在 Post 前对完整结果脱敏；Query owner 负责提供及关闭该 owner。

## 验收

`qwenpaw/native_mcp.py` 使用真实 MCP 协议 session、原生 Toolkit parser 及
QwenPaw Coordinator，覆盖拒绝后 server 零调用、篡改、并发归属、取消、完整结构化
错误和媒体。`qwenpaw/native_mcp_service.py` 增加真实 AW scope/Host/Provider，
核验身份及原始 envelope 到达两阶段且各执行一次。`qoder_sdk/parser.test.mjs`
使用固定真实 SDK parser、MCP server 和 fixture transport 核验身份、拒绝、schema、
取消及代次。仓库 native matrix 自动执行在线 Qoder daemon/Hook、reload 和严格 Post
测试。在线受管 MCP 的允许、拒绝、严格 Post 及取消目前保留独立人工容器验收证据，
记录在 PR 中，尚未纳入定时矩阵。部署候选须重跑这四项，核验原生 Hook 委托、真实
handler 进入、私有结果脱敏和原始 finalizer 排空；纳入 native matrix 的工作由 #9071
继续跟踪。
