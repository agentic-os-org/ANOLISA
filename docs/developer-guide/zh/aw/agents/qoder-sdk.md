# Qoder SDK

[English](../../../en/aw/agents/qoder-sdk.md)

## 依赖与工厂

已验证基线为 `@qoder-ai/qoder-agent-sdk@1.0.56` 与 bundled Worker 1.1.78 国际版。使用 Node.js 22+，SDK 安装在应用目录，认证沿用 SDK 设置。具备所需原生接口的其他 SDK/Worker 版本也可接入，AW 记录实际版本，并在启动和使用时检查包身份、声明站点、Hook 分发、调用身份和 Query 收尾。
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

## 原生配置

普通常驻 Query 保留原生 user/project/local 配置源、未知字段、插件路径、MCP 配置、
Hook 顺序、权限与会话行为。AW 不修改 SDK 或 Worker 文件。已验证版本为
SDK 1.0.54 / Worker 1.1.74 和 SDK 1.0.56 / Worker 1.1.78，覆盖 CN/global parser
及 Linux arm64 上的 global 认证联调；其他版本需要验证 AW 使用的接口。

AW 检查公开 `PreToolUse` 回调当时可见的参数。原生 Hook 和权限处理随后仍可改写
参数，这些后续参数没有经过最终执行前的再次检查。Post 策略记录沿用 AW 检查时的
输入快照，应用 Hook 收到原生 payload。原生拒绝保留优先级。Hook 命令和 Skill
插值位于模型工具边界之外；此接入不提供操作系统进程隔离。
Query 生命周期内须保持原生 Hook 开启。初始化发现有效配置为 `disableAllHooks: true`、
`hooksConfig.enabled: false` 或 `hooksConfig.headlessFastMode: true`，以及
`headless-fast-hooks` CLI 覆盖时，会在提交 prompt 前报错；不支持在活跃 Query
期间关闭 Hook。

省略 `settingSources` 可沿用原生默认值，也可传入应用原有的配置源列表。
settings、插件、Hook 和权限选项沿用原值。探测、普通 Query、预热 Query 和 daemon
创建均经过 Bridge；配置了 AW 时，准入失败须向调用方返回。应用入口支持 `.js` 与 `.mjs`。

一次性预热使用 `await bridge.startup({options})`：初始化期间不提交 prompt，
`warm.query(prompt)` 再次校验 scope 有效性。未使用的实例通过 `await warm.close()`
清理。AW 启停保持 SDK/Worker 文件不变；移除 AW 接入时，重启应用并恢复原生工厂调用。

原生 SDK 进程内 MCP 保留 schema transform 和默认值：AW 检查解析后的 handler 输入，
在 SDK 完成输出校验和异常转换后检查最终响应。模型回调委托到具有 Query 身份的 MCP
接点，取消时保留 handler finalizer 与 scope，直到清理完成。外部 MCP 回调检查公开
Hook 可见的参数，远端 server 内部执行由 server 负责。下文显式受管 MCP/私有执行
工厂继续沿用参数不得经 schema 改变的合同。

Pilot 沿用原生 Stop 设置，通过 `qoder_sdk.runtime_launchers` 授权 runtime wrapper。
wrapper 须启动所选 SDK 的 Worker，镜像保留其 preload 资产与环境。显式设置
`declarative_plugins: true` 以及使用 `awNativeExecutionFactory` 的 Query 继续沿用
下文的受限模式。Query 派发及原生 MCP 接管仍使用部分 SDK 内部接口，升级时需要回归验证。

## 应用约束

| 项目 | 接入要求 |
| --- | --- |
| 退出 | 普通与 `--reloadable` worker 均处理 TERM/INT，停止接单并等待清理后退出；没有处理器时 Node 立即退出，AW 随后强制回收遗留 SDK 子进程。 |
| 工厂覆盖 | 所有受保护 Query 经过 Bridge；直接使用原生工厂的 Query 无 AW 检查 |
| 原生设置 | 普通常驻 Query 保留原生配置；显式声明式与命令消费者模式保留原有限制。不支持替换 transport；runtime wrapper/import 需要[执行资产授权](../configuration.md#qoder-sdk-执行资产)。 |
| 权限与输入 | 保留原生拒绝，检查公开 Hook 可见的参数；后续原生改写未经过最终执行前再次检查 |
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
