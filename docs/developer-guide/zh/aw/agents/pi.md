# Pi SDK

[English](../../../en/aw/agents/pi.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 Pi SDK worker

高代码 Pi 应用使用 `adapter: pi`、公开 Coding Agent/Core SDK 0.84.2 与
Node.js >=22.19.0。AW 管理前台 SDK worker，并接通工具前后两个 Hook 点。
按应用现有依赖流程安装这些 SDK 包，保留应用模型与认证配置。原生 Pi CLI 是
独立入口，这个 Adapter 不启动该 CLI。

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
