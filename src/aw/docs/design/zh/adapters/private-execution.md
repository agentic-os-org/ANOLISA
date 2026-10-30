# 私有执行租约

[English](../../en/adapters/private-execution.md)

应用通过显式执行工厂提供短期工具凭据。AW 负责准入、结果脱敏与清理顺序，应用负责
获取和撤销凭据。原生模型与 daemon 认证保留各自生命周期。使用普通 resident 启动，
全部 Query/reply 和私有租约排空后，可显式空闲更新下一任务的策略；工厂变更需要滚动重启。

## 所有权与环境边界

| Adapter | 租约边界 | 显式消费接点 |
| --- | --- | --- |
| Qoder SDK 1.0.56 / Worker 1.1.78 | 每次受管 MCP handler 调用一个租约，由所属 Query 管理 | 创建客户端或子进程时调用 `bridge.executionEnvironment()` |
| QwenPaw 2.2.2.post1 / AgentScope 2.0.10 | 每次受管 Agent reply 一个租约，由该 reply 的工具共享 | 已安装的原生环境扩展调用 `execution_environment()` |

两者使用 `aw.private-execution/v1` 合同。执行 helper 返回已准入环境与声明的秘密值的
副本，Hook helper 返回独立公开视图，均不修改进程环境。应用须在 `secrets` 中声明敏感值，
并从 Hook 视图中排除其键名和值。凭据不得放入模型可见工具参数。脱敏函数须覆盖返回值
的全部字段，包括结构化 MCP 数据与错误内容，并保留原生结果结构和错误状态。

接入范围限于显式消费 helper 的原生工具。固定版本 Qoder SDK 的内建 Bash 没有私有
环境注入接点。QwenPaw 应用须先安装原生环境扩展，再启用自定义工厂。普通 shell 安全
Hook 不会安装这些执行扩展。

## Qoder 公共工厂

受管 Bridge 导出 `PRIVATE_EXECUTION_VERSION`、`executionEnvironment` 与
`hookEnvironment`。在 `createQuery().options` 中传入 `awExecutionFactory`；Bridge
在调用原生 SDK 前移除此选项。工厂接收冻结的 `{ invocationId, toolName, input }`
副本及 `{ signal }`，为每次调用返回一个新的租约：

| 租约字段 | 合同 |
| --- | --- |
| `version` | `bridge.PRIVATE_EXECUTION_VERSION` |
| `environment` | 执行环境字符串映射 |
| `hookEnvironment` | 公开 Hook 环境字符串映射 |
| `secrets` | 可选字符串映射，合入显式执行视图 |
| `redact(value)` | 对完整结果副本进行同步脱敏 |
| `release()` | 异步撤销凭据并清理自有文件 |

应用凭据服务可实现以下工厂，沿用 SDK 自身的认证回调：

```javascript
const current = await bridge.createQuery({
  prompt,
  options: {
    daemon,
    cwd: process.cwd(),
    settingSources: [],
    awExecutionFactory: async (invocation, { signal }) => {
      const lease = await acquireToolLease(invocation, { signal });
      return {
        version: bridge.PRIVATE_EXECUTION_VERSION,
        environment: lease.environment,
        hookEnvironment: lease.publicEnvironment,
        secrets: lease.secrets,
        redact: (value) => lease.redact(value),
        release: () => lease.revoke(),
      };
    },
  },
});
```

`acquireToolLease` 由应用实现；获取过程抛错时须清理已经创建的资源。
通过 `bridge.createSdkMcpServer()` 注册的 handler，将
`bridge.executionEnvironment()` 显式传给子进程或客户端。
`bridge.hookEnvironment()` 在该操作内只暴露已准入的公开视图。原生 schema、权限和
已批准参数保留现有检查，参见[结构化 MCP 派发](structured-mcp.md)。

Query 持有原始工厂和 handler Promise，直到其 `finally` 完成。handler 须返回完整
结果，流式迭代器会被拒绝。AW 复制并脱敏结果，等待 `release()`，随后才允许 Post
检查和原生交付。取消时会等待原始操作，包括取消后才完成获取的工厂，再释放租约。
应用须响应传入的 signal，并在 Query 共用清理截止时间内完成终结器。

## QwenPaw reply 工厂

既有 middleware context 接收异步上下文管理器工厂 `aw_execution_factory`，每次
reply 调用一次；`aw_environment_version: 1` 声明已安装原生执行扩展。
工厂 yield 的 `PrivateExecution` 包含 `version`、`environment`、
`hook_environment`、`secrets` 和同步幂等的 `redact` 函数。工厂的 `finally` 撤销
凭据并删除自有文件。

工具从活动 reply context 中调用 `execution_environment()`；Hook 使用
`hook_environment()`，任务 scope 也以此公开视图建立。流式输出保留至完整响应可供
脱敏，覆盖跨 chunk 拆分的秘密值。授权所依据的工具参数须保持原样。原生工具、
生成器和回调终结器结束后才进入清理。QwenPaw 先尝试关闭任务 scope，再退出凭据
工厂；reply 清理等待该 finalizer 完成。租约失败会在进程范围封锁新请求。offload
工具及其完成回调也在此所有权范围内。

## 故障与生命周期

Qoder 私有工厂、handler 或脱敏器抛出异常时，会停止所属的整个 Query 及其后续工具派发。
AW 报告 `AW private execution failed; tool result withheld`；原始异常可能包含凭据，
因此不会转发给 Hook 或模型。返回的 MCP 错误 envelope（`isError: true`）作为工具结果，
继续经过正常脱敏及 Post 检查。原生工作排空且撤销确认后，应用可创建新的 Query，
原 Query 的 scope 随之退休。
原生清理失败会保留所有权；租约清理失败或超时还会在进程范围内封锁新工作，直到 Worker 重启。
Qoder 的原生关闭、租约清理和回调排空共用 60 秒期限，QwenPaw 的 shutdown 与嵌套清理共用十秒。
取消等待本身无法确认原始操作已经结束。

普通 resident Qoder 与 QwenPaw 准入 `tool.after` 的
`on_error: withhold_result`。必要 Post 检查失败或超时，会阻止结果交付和后续派发。
Qoder 在回复失败回调或 MCP 请求前关闭原生 Query，再排空原始回调后退休 scope。
Post 之前已经发生的工具副作用继续生效。`on_error: report` 保留配置的报告行为。
Qoder 结果替换仍不支持。

`--reloadable` 使用独立的空闲更新合同，不提供严格 Post 和私有执行租约；Qoder 在
该模式拒绝 `awExecutionFactory`。保留原生权限
及调用方负责的 daemon 启停生命周期。Qoder 要求 `settingSources: []`，拒绝 settings、
未经授权的插件、额外 CLI 参数与运行时可执行文件/transport 覆盖。依赖这些输入的业务工厂，
须在部署前完成符合运行时准入合同的接入。上面的公共工厂提供凭据接点，保留这些检查。

启动命令见[统一安全配置](../../../../crates/aw-service/examples/aw.sdk-security.yaml)与
[原生接入指南](../../../../../../docs/developer-guide/zh/aw/deployment/resident.md)。
实现合同位于 [Qoder 私有执行](../../../../crates/aw-adapter/native/qoder-sdk/private.mjs)与
[QwenPaw 私有执行](../../../../crates/aw-adapter/native/qwenpaw/aw_private.py)。

## 原生 Bash 消费者

`awNativeExecutionFactory` 为每次原生 Bash 调用持有一份租约，与受管 MCP 的
`awExecutionFactory` 分开。参数与租约 API 见 [Qoder SDK 指南](../../../../../../docs/developer-guide/zh/aw/agents/qoder-sdk.md#原生-bash-命令消费者)。
Adapter 保持以下顺序不变量：

1. 安全检查看到冻结的逻辑输入，原生规则检查同一份输入。随后只有显式 `canUseTool`
   允许才能发布 wrapper 执行参数。固定 Worker 在 bypass/auto 中可能跳过 Hook ask，
   因此创建、模式切换和 Bash Hook 实际模式均须拒绝这些路径。
2. 发送权限回复前先登记发布所有权；回复状态不明时，无法判定票据未被使用。
   原生拒绝发生在发布前时，回收未使用租约，无需消费者 settle。
3. 消费者确认完成后才结束有限输入和关闭原生传输，原始回调排空后才撤销租约。
   超时转后台、结果消息与后台快照均无法单独证明消费者结束；通过结构化 session/tool/task
   身份关联每个已确认的消费者。
4. 正常 settle 没有任务时长上限。取消使用既有共享 60 秒预算，涵盖消费者 join、原生关闭、
   回调排空与租约清理。清理未确认时保留租约、快照及 scope 所有权，封锁新工作直至 worker 重启。

## 执行资产验证

每个 Query 以 no-follow 方式打开授权文件。进程内最多缓存 64 个成功结果，按路径、预期摘要及
`(dev, ino, size, mtimeNs, ctimeNs)` 匹配；身份或摘要变化时重新 hash。
最近一秒或整秒精度的 ctime 无法确定可复用的文件代次，因此重新验证。
流式读取限制实际字节数，并在 hash 后复查元数据；缓存淘汰仅增加重新验证的成本。
快照遍历将 no-follow descriptor 锚定到已打开目录，统计实际条目和字节。
资源上限与插件字段范围见[配置参考](../../../../../../docs/developer-guide/zh/aw/configuration.md#qoder-sdk-执行资产)。
