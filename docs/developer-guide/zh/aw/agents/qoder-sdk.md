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
let daemon, current, stopping = false, cleanup, initialized;
function close() {
  return cleanup ??= (async () => {
    // Creation failures are reported by the main task; still close its daemon.
    await initialized.catch(() => {});
    try { await current?.return(); }
    finally { await daemon?.stop(); }
  })();
}
function stop() {
  stopping = true; // A service also closes its HTTP/queue admission here.
  void close().catch(error => { console.error(error); process.exitCode = 1; });
}
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
initialized = (async () => {
  daemon = await makeDaemon({ auth: accessTokenFromEnv() });
  if (!stopping) current = await makeQuery({ prompt, options: {
    daemon, cwd: process.cwd(), settingSources: [],
  } });
})();
try {
  await initialized;
  if (!stopping) {
    for await (const message of current) {
      if (stopping) break;
      console.log(JSON.stringify(message));
    }
  }
} finally {
  try { await close(); }
  finally {
    process.removeListener("SIGTERM", stop);
    process.removeListener("SIGINT", stop);
  }
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
| 退出 | 普通与 `--reloadable` worker 均处理 TERM/INT，停止接单并等待清理后退出；没有处理器时 Node 立即退出，AW 随后强制回收遗留 SDK 子进程。 |
| 工厂覆盖 | 所有受保护 Query 经过 Bridge；直接使用原生工厂的 Query 无 AW 检查 |
| 原生设置 | `settingSources: []`；不传 settings、额外 CLI 参数或 transport 覆盖。Import 与本地插件需[执行资产授权](../configuration.md#qoder-sdk-执行资产)。 |
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

## 原生 Bash 命令消费者

内置 Bash 需要通过应用的凭据消费者执行时，使用 `options.awNativeExecutionFactory`。
先在[执行资产配置](../configuration.md#qoder-sdk-执行资产)中授权 wrapper executable。
普通 Daemon 和直接 Query 均支持此工厂，`--reloadable` 会拒绝。

将下面返回的 options 合入现有 Query 工厂，保留 `daemon`、`cwd` 和 `settingSources: []`。
应用提供原生权限回调 `approveTool`，以及负责获取绑定命令的票据、管理执行进程树的
`acquireConsumer`。

```javascript
export function nativeBashOptions(bridge, approveTool, acquireConsumer) {
  return {
    permissionMode: "default",
    canUseTool: approveTool,
    awNativeExecutionFactory: async (context, control) => {
      const consumer = await acquireConsumer(context, control);
      return {
        version: bridge.NATIVE_COMMAND_WRAPPER_VERSION,
        commandWrapper: {
          executable: consumer.executable,
          ticket: consumer.ticket,
        },
        settle: (control) => consumer.settle(control),
        release: () => consumer.release(),
      };
    },
  };
}
```

每个 wrapper Query 都需显式 `canUseTool` 回调，权限模式使用 `default` 或 `acceptEdits`。
创建与模式切换拒绝 bypass/auto 等其他模式；每次 Bash Hook 还会在获取票据前核对
Worker 上报的模式。PreTool 保持逻辑命令供原生规则检查，回调明确允许后才通过 SDK
权限结果发布 wrapper。应用 Hook 不能改写获准输入。

工厂收到冻结的 `{invocationId, nativeToolUseId, sessionId, policyRevision, toolName, input}`
与 `{signal}`。租约仅接受示例中的四个字段，`commandWrapper` 仅接受 `executable` 和 `ticket`。
票据为 16–256 个 base64url 字符。已授权 executable 接收两个参数：票据与 base64url
编码的原始命令。将票据绑定到该命令，隔离凭据、保留退出状态并脱敏输出。
原生规则、权限回调、AW 策略和应用 Post 输入使用逻辑命令；后续执行元数据可能显示
wrapper 句柄。票据应为短期 opaque 句柄，并在等待审批期间保持有效。

`settle({signal})` 须确认进程树与消费者 finally 已结束，包括原生工具超时转后台的情况。
正常完成没有固定任务时长上限；取消要求消费者在共享清理期限内停止并 join。
应用须限制获取期限、响应 signal，并在获取失败时清理已创建的资源。
AW 等待消费者确认完成后才结束有限输入、关闭原生传输，
随后排空原生工作与原始回调，最后调用 `release()` 撤销票据。
原生拒绝发生在发布前时，直接回收未使用票据，不调用 `settle`。
settle 或清理失败会保留所有权并封锁 worker；排查原因后重启。
顺序不变量见[执行设计](../../../../../src/aw/docs/design/zh/adapters/private-execution.md#原生-bash-消费者)。

## 更新与长期运行

普通模式在全部 Query 完整关闭后[更新策略](../deployment/policy-updates.md)，下一 Query 取得新快照。
受限 `--reloadable` 可在同一 Query 的请求之间更新，要求显式 daemon、`msg_lifecycle_v1` 和原生空闲确认，拒绝严格 Post、私有租约、goal、后台调度和 worktree。
持续消费 Query 输出，停止消费时停止派发并调用 `close()` 或 `return()`。该模式没有固定输出缓冲上限，且保留 1024 条 Hook 关联及每类 64 个活动 claim 的限制。
完整受限入口清单见[更新合同](../../../../../src/aw/docs/design/zh/runtime/policy-reload.md)。
