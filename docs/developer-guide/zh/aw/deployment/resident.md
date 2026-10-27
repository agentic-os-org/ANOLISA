# 部署常驻 SDK 应用

[English](../../../en/aw/deployment/resident.md)

为常驻 SDK 宿主配置任务工厂、MCP 客户端和清理流程。

## Pi 和 Claude SDK 常驻宿主

镜像安装相互匹配的 AW、Provider 和固定版本 SDK。使用
[共用配置示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.resident-sdk.yaml)，
替换部署路径后选择目标：

```bash
aw run --config /etc/aw/aw.yaml --agent claude
aw run --config /etc/aw/aw.yaml --agent pi
```

Claude target argv 使用应用的 Python 解释器，后接 `-m aw_claude -- -m application`。
前台 Gunicorn 可使用 `-m aw_claude -- -m gunicorn -c conf/gunicorn.py main:app`，客户端
在 fork 后的 worker 内创建。公共工厂接入方式：

```python
from aw_claude import create_client

client = create_client(options)
try:
    await client.connect()
    await client.query(prompt)
    async for message in client.receive_response():
        yield message
finally:
    await client.disconnect()
```

每轮响应和回调排空后可在同一客户端继续 query，保留会话；resume/session-store 继续可用。
并发工作使用独立客户端，分别使用自己的 cwd/env。每次 query 接受字符串或单消息的有限 AsyncIterable，多消息输入在发送前被拒绝；连续轮次逐次 query。
`receive_response()` 和 `receive_messages()` 都在回调排空后交付 `ResultMessage`；
缺少该消息的 EOF 会报错。保留 `finally` 中的 `disconnect()`，确保未完成的 query 执行 interrupt。

进程内 MCP 同时从 `aw_claude` 导入 `create_sdk_mcp_server`，每客户端创建新 server，
保留 SDK tool 定义和业务闭包。[受管 MCP 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/claude-mcp.md) 说明成功单文本替换、可靠错误状态及明确关联。

常驻 Pi 和 Claude host 会在发布 ready 前拒绝启用 AgentSight evidence 的配置。
常驻 host 可设置 evidence `mode: off`；旧的任务模式继续支持 evidence。

Pi 保留已有 SDK session 工厂，增加常驻入口：

```javascript
export async function createSession({ sdk, cwd }) {
  return sdk.createAgentSession({ cwd /* application model/tools/options */ });
}
export async function runWorker({ withSession }) {
  await serveRequests(async (request) => {
    return withSession({ cwd: request.workspace, environment: request.environment },
      async (session) => session.prompt(request.prompt));
  });
}
```

serveRequests 为业务服务，在工厂返回前配置业务回调。任务完成即关闭 session，同一 session
并发 prompt 会被拒绝；其他路径也必须经过公共工厂。

示例组合自定义 Hook、sec-core Bash 扫描及 Tokenless PreTool/PostTool。需要单独启动
sec-core daemon，并提供真实自定义 Hook。不透明 MCP 工具还需要明确的安全语义。常驻模式
在回调错误时停止任务，包括 on_error: report；正常策略拒绝沿用原生语义。Claude 中断无法
确认时会终止 AW 的自有前台进程组，部署需配置监督器重启。发送退出信号前先排空请求，
并设置目标的 `shutdown_grace_ms` 为应用剩余清理留出时间。外层 launcher 从首次转发
退出信号到开始强制清理，默认给予两秒宽限期。
作用范围和限制见[退出宽限期](lifecycle.md)。
Schema/BeforeModel、Retrieve 的请求/效果准入和授权依赖见
[常驻合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/resident-sdk.md)。

## 为常驻 SDK 复用一份安全策略

[统一 SDK 安全示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
通过 `--agent` 选择 Qoder SDK 或 QwenPaw。替换全部绝对路径，按对应章节初始化
原生运行时，并在启动前按配置的 socket 启动 sec-core：

```bash
aw validate --config /absolute/path/aw.sdk-security.yaml
aw run --config /absolute/path/aw.sdk-security.yaml --agent qoder-sdk -- "your prompt"
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home aw run \
  --config /absolute/path/aw.sdk-security.yaml --agent qwenpaw
```

sec-core 步骤只匹配 `native:qoder-sdk:Bash` 和
`native:qwenpaw:execute_shell_command`。AW 在能力准入和 Provider 准备前投影所选
目标的步骤。其他工具仍执行自定义前后检查，不会隐式增加 sec-core 扫描。扩展扫描
范围时，显式增加 selector 及对应的 Provider 工具映射。

自定义脚本使用布尔 Hook 合同：从 stdin 读取一个规范化事件，输出 JSON 布尔值并以
零退出。本例 Before 的 `true` 请求 `block`，After 的 `true` 记录 `observe`；
After 的 `false` 不产生效果，检查仍视为成功。结果检查失败须非零退出，随后
`on_error: withhold_result` 阻止交付及后续派发。保留原生权限和调用方负责的
daemon/App 生命周期。使用普通 `aw run`，在 Query 关闭或 reply 完成后显式空闲更新。私有凭据需在应用代码
接入[显式执行工厂](../../../../../src/aw/docs/design/zh/adapters/private-execution.md)，
此 YAML 安装安全策略。
