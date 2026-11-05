# Pi SDK

[English](../../../en/aw/agents/pi.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 Pi SDK worker

高代码 Pi 应用使用 `adapter: pi`、公开 Coding Agent/Core SDK（已验证基线为 0.84.2）与
Node.js >=22.19.0。AW 管理前台 SDK worker，并接通工具前后两个 Hook 点。
按应用现有依赖流程安装这些 SDK 包，保留应用模型与认证配置。原生 Pi CLI 是
独立入口，这个 Adapter 不启动该 CLI。

其他版本接入时，AW 检查包身份、模块布局和原生 Agent/AgentSession 接口，记录已安装的 Coding Agent 版本。

在绝对路径的 `.mjs` 应用模块中暴露已有公共工厂与前台任务：

```javascript
export async function createSession({ sdk }) {
  return sdk.createAgentSession({
    cwd: process.cwd(),
    agentDir: "/absolute/existing-pi-agent-directory",
  });
}

export async function run(session, { args }) {
  await session.prompt(args.join(" "));
}
```

复制 [Pi 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.pi.yaml)，
将 `argv` 改为 `[node, /absolute/application.mjs]` 并设置工具前后策略。
附带命令为中性示例。随后执行：

```bash
target/debug/aw validate --config /absolute/aw.pi.yaml
target/debug/aw run --config /absolute/aw.pi.yaml --agent pi -- 'Your task'
target/debug/aw status --config /absolute/aw.pi.yaml
target/debug/aw stop --config /absolute/aw.pi.yaml
```

`--agent` 选择配置中的目标名，`adapter` 保持为 `pi`。`--native-settings`、
`--native-profile` 与 `--native-state-dir` 不是 Pi SDK 选项。AW 绑定前核验实际
安装的 SDK 版本，等待工厂与 callbacks 安装，随后报告 `AW pi native hooks ready`。
应用任务必须留在同一个前台 worker。new/resume/fork/import 替换会话使用传给
`run` 的 `createSession`，保证每次工厂返回后安装同样的 Hook 组合。已有 before
拒绝/参数修改、after 结果替换继续生效，AW after 观察最终结果。Pi 被拒绝、参数
非法、提前取消的工具没有 after callback。

Tokenless 使用[共享优化示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)中的 Pi SDK 目标。
安全检查位于 `pre_tool` 之前，观察器/自定义 Hook 位于最后 `post_tool` 替换之前。
重试理由进入模型，建议命令只能通过新工具调用执行。PostTool 替换成功结果的唯一
文本块并保留 SDK metadata；report 故障保留原结果。原生 Hook 不能提供替换。
AW 每次生成 invocation UUID 配对，原工具 ID 保留于 `native`，提前拒绝时关闭事件。
精确 callback、身份、取消、容量与可
复现的 SDK 验收见 [Adapter 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/pi-adapter.md)。
修改策略前退出 Agent 并停止原服务；审计状态按同样的本地服务清理流程保留或删除。

## 检查用户输入

[Pi 输入检查示例](../../../../../src/aw/crates/aw-service/examples/aw.pi-submission.yaml)
使用导出 `runWorker` 的常驻工厂。替换可执行文件及 sec-core socket 路径，保留应用的
模型与认证配置，再运行 `aw run --config /absolute/aw.pi-submission.yaml --agent pi -- 'Your task'`。
应用保留现有工厂，并导出以下 worker 入口：

```javascript
export async function createSession({ sdk, cwd }) {
  return sdk.createAgentSession({ cwd, agentDir: "/absolute/existing-pi-agent-directory" });
}

export async function runWorker({ args, withSession }) {
  await withSession({ cwd: process.cwd() }, async (session) => {
    await session.prompt(args.join(" "));
  });
}
```

`session.prompt(text, options)` 等待已配置的 `scan_prompt` 和 `scan_user_input`
步骤完成后，才进入原生 prompt 执行。中文、空白和空字符串均原样交给 Pi。
策略拒绝或 `on_error: block` 故障会使 Promise 拒绝；应用捕获后，可在同一会话提交
下一条输入。`on_error: report` 记录失败检查后继续。提供 `preflightResult` 时，
被检查拒绝的输入会收到 `false`。

启用检查后支持 interactive 来源的文本。图像/附件、`input`、`context` 和
`before_provider_request` 扩展处理器、展开的斜杠命令、
`steer`、`followUp`、`sendUserMessage`、`sendCustomMessage`、直接 Agent 提交及
并发 prompt 均被拒绝。使用 `expandPromptTemplates: false` 可提交字面量斜杠文本。
其余合法 prompt options 保留；模型续跑和工具重试属于既有执行，不生成新的用户提交。
最终 Agent prompt 中的用户文本必须与检查内容一致。context 和 provider-request
变换会在该检查之后改写文本，因此启用输入检查时拒绝这两类扩展。
使用上述未支持入口的应用需移除
输入事件配置，或使用支持的纯文本工厂。直接设置的 `Agent.transformContext` 和
`Agent.convertToLlm` 回调必须保留用户消息内容及顺序，包括既有历史；
`Agent.onPayload` 必须保留序列化请求。AW 隔离回调输入和输出，并在交付前拒绝改写。
受管会话的这些回调槽位固定。会新增用户消息的转换（包括 custom/summary 消息）
需要使用未开启输入检查的工厂。未配置 `input.submit` 时保留 Pi 原生输入模式和回调。

等待 `session.abort()`、`session.dispose()` 或工厂返回的 `close()`，即可取消并释放
受管任务。取消后的会话不可继续使用，后续请求需创建新工厂会话。清理最多等待 12 秒，
覆盖原生 abort、在途 prompt/检查和 dispose，随后释放 AW scope。清理失败需要重启
worker。扫描结果迟到不会触发模型请求。
