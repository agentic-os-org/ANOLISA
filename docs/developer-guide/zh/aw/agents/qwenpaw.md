# QwenPaw

[English](../../../en/aw/agents/qwenpaw.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 QwenPaw

安装 QwenPaw 2.2.2.post1 与 AgentScope 2.0.10，通过 QwenPaw 初始化模型配置。示例
使用 `argv: [qwenpaw, app]`。将 `QWENPAW_WORKING_DIR` 指向已初始化的绝对目录。
AW 只添加自身临时插件，保留已有插件和原生配置，App 退出后删除自身插件。

普通启动会检查每个已构造 Agent 实际安装的 resident middleware，并创建已确认的
任务作用域。Before 正序执行、After 逆序展开，原生工具并发由宿主决定。Offload 工具
保留作用域直到生成器、原生 Hook 和后台完成回调全部结束；已完成调用及时回收状态，
长会话按活跃调用容量准入。

后台 HTTP 输出和下一轮提示在全部必要原生回调成功后才发布；拒绝结果保留策略说明及
Tokenless 重试参数。脱敏函数必须幂等，以支持多个交付及 Hook 边界重复检查。
After Hook 接收脱敏后的终结 `ToolResponse`，包括 AW 拒绝结果。应用凭据扩展提供
每次 Agent reply 一个私有执行租约，由该 reply 的工具共享，并负责清理；见
[私有执行租约](../../../../../src/aw/docs/design/zh/adapters/private-execution.md)。
授权所依据的工具参数保持原样。`on_error: report` 保持配置的
回调故障报告策略，工具前 `block` 拒绝执行。结构化 Provider 和布尔 Hook 的工具后
`withhold_result` 阻止未核验结果交付及后续派发，仅普通 resident 启动准入该动作。
严格 After 路径暂扣匹配工具的流式 chunk；任务私有执行也会暂扣中间 chunk，直到终态脱敏。
原生 Hook 或生命周期故障会封锁 Agent，包括 AW 步骤配置为 `report` 的情况；
修复原因后重启失败的 Agent。清理超时或 finish/release 确认状态不明会在进程范围内
封锁新请求，须重启 worker；凭据保留至原生工作确认排空。Shutdown 与嵌套清理
共用十秒预算，包含取消后的排空。
事件预算为 1..55,000 ms，ask/approve 指令尚不支持。
原生权限检查在这些回调之前执行；外部执行工具和此前的权限拒绝沿各自原生路径处理。

Startup 回执确认插件加载，每个 Agent 在请求开始前另行通过实际安装检查。看到原生
Hook 就绪信息后使用 App/API；ACP/TUI、多 worker 启动继续被拒绝。`--reloadable`
保留独立的请求空闲更新合同；普通启动提供 resident 子作用域、长会话回收，以及 reply
完整排空后的显式更新。复用的 Agent 在每轮新 reply 预留任务后读取活动策略。
App startup 在首个 reply 前确认更新就绪；启动策略持续生效，直到更新成功。共享
子作用域尚无原生证据采集合同，普通 resident 启动会拒绝该配置。

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

[QwenPaw 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

零 Provider、零事件步骤的普通配置也会安装 resident 准入与清理 middleware。
