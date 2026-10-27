# Qoder SDK

[English](../../../en/aw/agents/qoder-sdk.md)

## 依赖与工厂

使用 `@qoder-ai/qoder-agent-sdk@1.0.56`、bundled Worker 1.1.78 国际版、Node.js 22+。SDK 安装在应用目录，认证沿用 SDK 设置。
在完整 AW 配置的 `spec` 中设置目标。

```yaml
agents:
  sdk:
    adapter: qoder-sdk
    argv: [node, /absolute/application/worker.mjs]
```
应用使用受管 daemon 和 Query 工厂，并在 finally 中关闭两者。

```javascript
import { startDaemon, query, accessTokenFromEnv } from "@qoder-ai/qoder-agent-sdk";
import { pathToFileURL } from "node:url";

const prompt = process.argv[2];
if (!prompt) throw new Error("A prompt is required");
const bridge = process.env.AW_QODER_SDK_BRIDGE
  ? await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE)) : undefined;
const makeQuery = bridge ? bridge.createQuery : async (request) => query(request);
const makeDaemon = bridge ? bridge.startDaemon : startDaemon;
const daemon = await makeDaemon({ auth: accessTokenFromEnv() });
let current;
try {
  current = await makeQuery({ prompt, options: {
    daemon, cwd: process.cwd(), settingSources: [],
  } });
  for await (const message of current) console.log(JSON.stringify(message));
} finally {
  await current?.return();
  await daemon.stop();
}
```
## 启动与验证

```bash
aw validate --config /absolute/aw.yaml
aw run --config /absolute/aw.yaml --agent sdk -- "Your task"
aw status --config /absolute/aw.yaml
aw stop --config /absolute/aw.yaml
```

`startDaemon()` 确认预热就绪；`createQuery()` 在 SDK 握手后确认 Query 就绪。按[原生验收](../validation.md)检查允许、阻断、后置故障和清理。
安全组合使用[SDK 示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)，先替换路径并启动 sec-core。

## 应用约束

| 项目 | 接入要求 |
| --- | --- |
| 工厂覆盖 | 所有受保护 Query 经过 Bridge；直接使用原生工厂的 Query 无 AW 检查 |
| 原生设置 | `settingSources: []`；不传 settings、plugins、额外 CLI 参数或 transport 覆盖 |
| 权限与输入 | 保留原生拒绝；不支持参数改写或权限 Hook 覆盖 |
| 环境与认证 | 共享 daemon 的 Query 不覆盖 env/auth；更换凭据用原生 `daemon.auth()`；不同凭据可用独立 daemon |
| 后置检查 | 支持观察和 `withhold_result`；不支持结果替换 |
| AgentSight | 普通 resident 设置 `evidence.mode: off` |
| worktree | 普通模式回调携带父 Query cwd，依赖实际执行目录的策略不支持 worktree；受限模式直接拒绝 |

清理须等待原生 Query、Hook、权限、凭据及 MCP 回调排空。私有租约清理失败或超时会封锁新工作，修复后重启 worker。

## 受管 MCP 与私有执行

使用 `await bridge.createSdkMcpServer(options)`，每个 Query 创建新 server。固定版本提供 Query 身份；要求来源模型工具 ID 的 `parentIdentity: "required"` 会被拒绝。
参数经 schema 默认值或 transform 改变时，AW 在 handler 前拒绝。Post 故障沿用 `withhold_result`，保留原生结构化结果。

私有执行通过 `options.awExecutionFactory` 显式接入，handler 消费 `executionEnvironment()`，结果在 Post 与交付前脱敏。内建 Bash 没有环境注入接点。
API 与清理顺序见[私有执行](../../../../../src/aw/docs/design/zh/adapters/private-execution.md)，MCP 身份见[结构化 MCP](../../../../../src/aw/docs/design/zh/adapters/structured-mcp.md)。

## 更新与长期运行

普通模式在全部 Query 完整关闭后[更新策略](../deployment/policy-updates.md)，下一 Query 取得新快照。
受限 `--reloadable` 可在同一 Query 的请求之间更新，要求显式 daemon、`msg_lifecycle_v1` 和原生空闲确认，拒绝严格 Post、私有租约、goal、后台调度和 worktree。
持续消费 Query 输出，停止消费时停止派发并调用 `close()` 或 `return()`。该模式没有固定输出缓冲上限，且保留 1024 条 Hook 关联及每类 64 个活动 claim 的限制。
完整受限入口清单见[更新合同](../../../../../src/aw/docs/design/zh/runtime/policy-reload.md)。
