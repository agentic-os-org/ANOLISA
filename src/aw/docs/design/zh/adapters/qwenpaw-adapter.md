# QwenPaw App 适配器

[English](../../en/adapters/qwenpaw-adapter.md)

AW 通过固定的 QwenPaw `2.2.2.post1` / AgentScope `2.0.10` 原生 middleware 和
Coordinator 管理工具调用。普通 `aw run` 为每个 Agent 请求创建共享 SDK 任务作用域。
原生权限、调度与模型配置由 QwenPaw 负责。

## 准入与身份

App startup 回执确认插件加载。在 `AgentBuilder.build` 返回 Agent 前，AW 检查实际
安装的 reply、acting middleware。构造守卫要求恰好一个 resident middleware 和一个
Coordinator。QwenPaw 会记录并跳过普通插件工厂异常；AW 必需工厂异常、中间件缺失或
Coordinator 缺失均使 Agent 构造失败。Resident middleware 位于 Coordinator 之前。

每个请求通过与 Claude SDK 共用的 CLI 传输和服务调用账本取得已确认的子作用域。
原生派发取得新的 `aw_invocation_id` UUID，`tool_call.id` 保留 QwenPaw 原始 ID。
服务按 UUID 关联事件，适配器只保留活跃原生调用的所有权记录。原生执行前精确比较
参数，包括大于 `2**53` 的整数，再进入共享协议的数值归一化。输入变化、过期回调
身份和已回收作用域均拒绝执行。准入复用固定版本 Toolkit 的解析器和工具 schema，
保留原生空输入及修复语义；Toolkit 在实际派发前再次核对有效参数映射，后续改写会
封锁所属 Agent。该派发核对在进入工具函数前消费，内部嵌套 Toolkit 调用
可使用自身参数。

存在 offload 工具时，请求作用域继续保留。原生流、原生 Hook 和 Coordinator 清理
全部结束后回收调用；请求释放等待所有活跃调用排空。顺序完成的调用及时释放账本
记录；历史累计 512 次限制仅保留在旧 reloadable 路径。活跃容量饱和时拒绝新调用，
已有调用排空后可恢复准入。

## 原生结果与取消

Before Hook 按配置顺序执行；After Hook 按注册逆序执行，保持最终结果替换顺序。
AW 拒绝产生原生 denied `ToolResponse`，执行已准入的 After Hook，工具函数不会进入。
Resident 传输保留拒绝文本，包括 Tokenless 重试参数及原生命令解释。
原生权限检查继续位于 acting middleware 之前。

适配器包裹 Coordinator 的 `_run_tool_with_hooks`、`_begin_offload`、
`_finalize_completed` 与 `_supervise` 接点，等待受 shield 保护的原生 After Hook
真实结束、生成器耗尽，并将 offload 所有权保留到后台处理和完成回调结束。原生 shutdown
与嵌套清理共用十秒截止时间，包含取消后的排空。
后台结果保存在私有完成 entry 中，处理器及全部 completion handler 成功后才发布。
Handler 收到该私有 entry，保留原 context、task 和 stream；发布前 HTTP 查询和下一轮
Agent 提示均看不到终态结果；offload 通知回调也属于该发布屏障。
回调失败时不发布缓存结果或提示。核验通过的结果发布早于账本退休：后续 finish/release
失败会封锁 worker 新活动，已经提交的已核验结果仍可读取。
并发退休路径共享一次 finish 确认；确认失败会封锁 worker。确认原生工作排空后，
即使服务确认失败也释放本地凭据；原生工作尚未结束则保留 lease 并封锁新请求。
公开 `reply_stream.aclose()` 与 `agent.close()` 等待被中断的前台工作清理后释放 scope。
正常请求关闭保留 offload，直到原始回调完成。Offload 与 completion 回调异常在进入
原生日志前移除敏感内容，并将所属请求标记为失败。

`on_error: report` 报告 AW 回调传输、大小限制及 Provider 故障并保留原结果。
结构化 Provider 和布尔 Hook 的 After 步骤可用 `on_error: withhold_result`：普通
resident Adapter 向共享准入声明该失败动作，Host 在失败时记录动作，原生边界拒绝
最终结果交付及后续派发。`tool.after block` 是无效配置；reloadable 和原生命令步骤
不准入 withholding。事件与步骤选择器共同决定哪些工具暂扣流式 chunk。
全部已配置回调仍交给 Host，使跳过的步骤推进有序执行状态。Host 确认 skip 前的
传输失败仍采用已准入的失败动作，包括 withholding；本地流选择不会豁免该动作。
受支持的文本替换保留响应外壳、状态与结构化字段。

原生应用 Hook、所有权及清理异常始终封锁 Agent。AW 明确加强 QwenPaw 的记录后继续
行为：凭据终结器或原始 Hook 状态不明时，吞异常接点不能将其转成成功结果。
其他原生 Hook 保留顺序及正常返回值。清理超时后取消并短暂等待所属任务，仍未结束的
任务保留至可收取其最终异常；不会无截止时间地等待。清理超时还会在进程范围内
封锁新请求及派发，包括新构造的 Agent；修复原因后重启 worker。其他原生 Hook
故障封锁所属 Agent。

## 任务私有执行扩展

应用可在 `ctx.aw_execution_factory` 提供返回 `PrivateExecution` 的异步上下文管理器，
并声明 `ctx.aw_environment_version = 1`。原生工具启动子进程时显式将
`execution_environment()` 传给 `env=`。任务上下文会传递至 Coordinator 创建的后台任务；
AW 不向 `os.environ` 写入任务凭据。缺少声明的原生接点时，在作用域创建前拒绝准入。

Lease 分别提供 Hook 环境和结果脱敏函数。结果脱敏先于原生 After Hook、AW 对外 Hook
及受支持的输出处理。私有 lease 会对公开 reply 和原生流订阅者暂扣中间 chunk，
包括 report 模式请求；脱敏处理累积后的终态响应，避免跨 chunk 拆分的凭据泄漏。
脱敏函数必须幂等：重复处理已脱敏内容时保持其内容不变；多个交付及 Hook 边界会重复调用它。
脱敏函数必须保留授权所依据的工具参数，改写参数会拒绝调用；
凭据通过私有环境传递。成功、拒绝、故障和取消路径均在任务排空后执行 lease 清理，
AW 清空自身持有的环境与凭据字典。凭据文件创建、进程失主清理及应用部署接入由应用
lease 实现负责，需单独验收。

版本化租约字段及显式消费者要求见[私有执行租约](private-execution.md)。

## 入口与兼容边界

启动器仅拥有 `plugins/aw-native`，写入私有运行资源并在 App 退出后删除该目录。
已有插件目录与原生 profile 由原所有者管理。使用已初始化的绝对 `QWENPAW_WORKING_DIR`
和 App/API 入口；ACP、TUI、多 worker 启动仍在本适配器准入范围之外。

`--reloadable` 保留既有整请求/工具 busy 跟踪及策略更新行为，其 binding 无法创建
resident 子作用域；混合启用两种模式会明确失败。共享子作用域尚无原生证据采集合同，
普通 resident 启动会拒绝该配置；reloadable 证据采集行为保持原状。

原生权限拒绝和外部执行工具在 acting middleware 前完成，因而不产生 AW 工具回调。
零步骤的普通配置仍会安装 resident 准入及清理 middleware。

## 验证

每 PR 的 `qwenpaw` Rust 目标通过 `AwPlugin.register`、工厂及安装守卫加载实际
暂存插件，贯通 `aw run`、配置准入、Scope CLI、服务和 Provider。原生框架替身用于
隔离启动接线，覆盖 withholding 失败、report 继续、工具前拒绝、注册失败、清理和
reloadable 拒绝；真实固定版本的生命周期语义由下述独立套件覆盖。

`native_resident.py` 运行已安装的 AgentScope acting 链、QwenPaw Coordinator 和实际
middleware 构造函数。传输替身用于隔离验证原生顺序、严格 Post 失败、取消、offload
收尾、输入完整性、私有环境/脱敏、容量恢复及连续 1,025 次调用。
`native_resident_service.py --bin <directory>` 将相同 Coordinator 接至真实 Scope CLI、
服务和 Provider，验证拒绝及完成排空。原生矩阵继续保留既有 middleware、reload 和 App 套件。

这些确定性用例无需模型凭据。完整 App 部署、应用凭据生命周期、真实安全后端组合及
生产流量验收在整套 SDK/MCP 接入完成后单独执行。

## 源码参考

初始插件接入基于 QwenPaw 固定提交 `3822ec7`：
[plugin API](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/plugins/api.py),
[App startup](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/app/_app.py),
[AgentBuilder](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/runtime/builder.py).
当前 resident 验收使用 `tests/native-matrix.json` 声明的已安装
`qwenpaw==2.2.2.post1` 和 `agentscope==2.0.10` 制品，包括
`qwenpaw.tool_calls`、`qwenpaw.agents.react_agent` 与
`agentscope.tool._toolkit`；调整版本时须重新验证这些原生接点。
