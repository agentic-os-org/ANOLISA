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
参数/权限拒绝、直接 Toolkit 调用和外部执行请求需要另接。每条 `aw run` 管理一个
前台 worker；多个 worker 分别启动，共用 daemon。fork 型 Gunicorn supervisor
与子 worker 自动接线仍需独立工厂接入。工具调用记录按调用回收，轮次结束释放 Scope。

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
未配置输入路由时保留原生输入语义，每次回复仍使用独立 Scope。

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

原生暂停回复及稍后的续跑分别对应独立 AW 轮次。`tool.after` 路由配置
`on_error: withhold_result` 后抑制中间工具 chunk，全部检查和调用清理完成后
发布受检最终内容。`reply_stream` 消费者和模型收到相同的采用文本或拦截占位结果。
未配置严格 after 路由的目标保持原生中间 chunk。

## 任务私有工具凭据

在 `AwBridge.create_agent` 中传入 `execution_factory`，每次回复申请一份应用租约，
未配置 `input.submit` 时同样生效。复用 `aw_private.PrivateExecution` 合同；工具创建
子进程或客户端时，须显式消费 `execution_environment()`。

```python
import asyncio
from contextlib import asynccontextmanager
from aw_private import PrivateExecution, execution_environment

@asynccontextmanager
async def execution_factory():
    credentials = await credential_service.acquire()
    try:
        yield PrivateExecution(
            environment=credentials.environment,
            hook_environment=credentials.public_environment,
            secrets=credentials.secrets,
            redact=credentials.redact,
        )
    finally:
        await credentials.revoke()

agent = bridge.create_agent(
    name="worker", model=model, toolkit=toolkit,
    execution_factory=execution_factory,
)
# Inside an application tool:
process = await asyncio.create_subprocess_exec(command, env=execution_environment())
```

`credential_service`、`model`、`toolkit` 和 `command` 由应用提供。
同步脱敏函数须覆盖完整结果内容与 metadata，且重复调用保持幂等。AW 暂存工具分片，
对最终 `ToolResponse` 脱敏后再交给公开 Hook 和模型。全部 Hook 和调用退休完成后，
流消费者收到一个最终分片。原生参数与 worker 的全局环境保持原样，Hook 只接收 `hook_environment`。

AW 在申请凭据前保留回复占用，等待原生工具排空、子 Scope 关闭、凭据确认撤销后释放。
申请和撤销各限时 60 秒。撤销失败或超时会保留占用并拒绝后续回复，需要重启 worker；
晚到操作仍由原任务清理，不会进入新的任务执行。详见[租约合同](../../../../../src/aw/docs/design/zh/adapters/private-execution.md#agentscope-回复工厂)。

## 更新常驻 worker

普通 `aw run` 在全部 `reply` / `reply_stream` 及清理结束后支持策略更新。
Agent 和原生状态继续保留；下一次回复先取得不可变策略快照，再检查输入和派发工具。
业务 middleware 保留原有顺序，输入与工具路由的增删整体生效。

```bash
aw update --config current.yaml --agent agentscope --policy next.yaml
aw status --config current.yaml
```

更新前暂停派发。模型执行、活跃工具任务和未确认的清理都会保持 busy；
部分消费的回复流须显式关闭。Scope 打开响应丢失时需重启 worker 后继续；
子 Scope 已确认关闭但请求占用释放未确认时，下一次回复前重试释放。
原生 evidence 采集仍不在常驻策略 Scope 范围内。可修改字段与版本选择见
[策略更新](../deployment/policy-updates.md)。

## 启动多个 OS worker

每个 worker 独立运行一条 `aw run`，共用配置、运行用户与 daemon。各 worker
通过 `AwBridge.create_agent()` 构造 Agent，分别拥有实例身份、就绪回执和启动目录。

在各 worker 的终端执行：

```bash
aw run --config current.yaml --agent agentscope
```

查看就绪实例，暂停目标 worker 的任务派发后更新其策略。将 `INSTANCE` 换为
status 中该 worker 的 `instance_id`：

```bash
aw status --config current.yaml
aw update --config current.yaml --instance INSTANCE --policy next.yaml
```

停止一个 launcher 会终止并排空对应 worker，其余 worker 保留自己的身份、策略和
会话。全部 worker 退出后再停止共用 daemon。业务自管的 prefork/Gunicorn worker
需要独立登记与工厂接入；跨 OS 进程继承同一个 `AwBridge` 仍不支持。
