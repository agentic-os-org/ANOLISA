# AgentScope SDK

[English](../../../en/aw/agents/agentscope.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 AgentScope SDK

独立 SDK 的已验证基线为 AgentScope 2.0.6，QwenPaw 的已验证运行时使用
AgentScope 2.0.10。其他版本可通过相同的公共工厂与 middleware 合同接入。
`argv` 指定 SDK 环境的 Python 和 `.py` 应用或 `-m MODULE`。AW 记录已安装版本，
提供 `aw_agentscope` 与受保护的运行配置，等待真实公共工厂安装 middleware 后生成的回执。

在 `src/aw` 中，将该环境的 `python3` 放到 PATH 后执行：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

示例使用确定性的离线 model 与本地 Echo 工具，无需凭据。before 布尔 Hook 返回
false（没有 block 命中），after 返回 true（observe）。把 before 命令改为
`printf true` 即可拒绝工具；原生 final response 为 denied，after 观察该拒绝。
修改配置之前先停止服务。

已有应用在**每个**公共工厂中使用 `AwBridge().create_agent(...)`，原样传入已有
middleware 列表。可信参数注入应先于追加的 AW middleware。readiness 回执只
覆盖所构造的 Agent，不覆盖其他工厂或直接 dispatch 路由。
[工厂合同](../../../../../src/aw/docs/design/zh/adapters/agentscope-adapter.md)说明顺序、预算、
调用身份与覆盖边界。

before 等待并可在工具副作用前阻断；after 等待一次 final `ToolResponse`，保留
success/error/interrupted，不重复中间 stream chunk。`on_acting` 之前的原生
参数/权限拒绝、直接 Toolkit 调用和外部执行请求需要另接。launcher 支持单个
前台 worker；fork 型 Gunicorn supervisor、多 worker 和远端部署不在此合同内。
工具调用记录在每次调用后回收；启用输入检查时，轮次结束还会释放该轮 Scope。

接入 PreTool 重试建议和最终 PostTool 压缩时，复制
[SDK 生命周期示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)，
替换所有路径，并在 sec-core 映射 Toolkit 的准确工具名和参数指针。示例先做安全检查，
再执行 Tokenless PreTool；工具完成后先观察原始结果，再压缩一个成功的文本槽。
custom/native after Hook 放在最终压缩步骤之前；启用替换后 AW 按声明顺序执行此链。
native 和观察 Hook 不替换响应；只有最后一个已准入的结构化替换步骤可以修改模型
可见文本。Hook 故障按 report 处理后，后续步骤仍收到原始输入。
只有观察的链保持原生逆序退出。

PreTool 拒绝原始调用，向模型返回完整重试参数；每次新尝试重新经过原生权限和
sec-core。带 RTK 归属的重试即使使用新 call ID 也不会重复压缩。文本槽适用条件、
故障处理和遥测配置见
[Tokenless 行为](../../../../user-guide/zh/aw/tokenless.md)。无效的最终响应替换指令会报告故障并保留原结果。

## 在回复前检查用户输入

启用[生命周期配置](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)
中的 `input.submit`，在写入 memory、请求模型之前执行 sec-core `scan_prompt` 和
`scan_user_input`。工厂使用 AgentScope 2.0.6 原生 `on_reply` 中间件与公共 `Agent`
子类管理回复生命周期。业务中间件保持原顺序，AW 检查它们传下来的实际输入。

| `reply` / `reply_stream` 的输入 | 检查行为 |
| --- | --- |
| role 为 `user` 的原生 `Msg` | 按顺序用换行拼接全部 `TextBlock.text` |
| `list[Msg]` | 按消息顺序拼接全部用户文本块 |
| system、assistant 消息（含工具结果）及已有 state/memory | 不计入本次用户输入 |
| `None`、原生确认、中断和外部执行结果事件 | 沿用原生续跑，不产生新提交 |
| 字符串、任意对象、用户媒体或非文本块 | 执行前抛出 `ValueError` |

历史消息保留在 Agent state；再次显式传入的用户消息会作为新提交检查。
启用检查时不支持直接调用私有 `_reply` 或嵌套派发子 Agent。同一 Agent 并发回复会
抛出 `CallbackFailure`；单个前台 worker 内的不同 Agent 可以并发，分别拥有轮次 Scope。
未配置输入路由时保留原有原生输入模式与进程级 Scope 行为。

策略拒绝抛出 `aw_submission.SubmissionDenied`，模型请求和输入 memory 写入均为零。
Provider 故障遵循 `on_error: block` 或 `report`；不支持的输入形状始终在执行前拒绝。
清理完成后同一 Agent 可继续输入。每次公共 reply 调用建立一个 AW turn ID 与 Scope，
纯模型回复也包含在内。输入步骤共享本轮身份和预算，工具调用复用本轮账本。
正常返回、拒绝、异常和取消都会释放 Scope；关闭未确认时，后续分配须等待有界清理重试成功。

完整消费 `reply_stream`，提前停止时在消费任务内显式关闭。可使用 `contextlib.aclosing`：

```python
from contextlib import aclosing
from agentscope.message import UserMsg
from aw_submission import SubmissionDenied

try:
    async with aclosing(agent.reply_stream(UserMsg("user", "hello"))) as stream:
        async for event in stream:
            handle(event)
except SubmissionDenied as error:
    show_rejection(str(error))
```

原生暂停回复及稍后的续跑分别对应独立 AW 轮次。完成的 `ToolResponse` 保留严格后置
拦截，中间 `ToolChunk` 输出仍在完成结果保证的范围之外。
