# 传输观测如何接入 AW 统一 Hook

[English](ARCHITECTURE_NOTES.md)

本包为 AW 提供可选的进程级传输证据。近期目标是固定版本、少量 Worker、低频任务的
只读观测：核对哪个进程发生了 TLS 写入、长度和摘要，以及采集覆盖。候选采集机制为
BPF uProbe；ptrace 实验的强杀故障影响目标进程，保持默认禁用。

## Qoder 和 Qoder SDK 的接入位置

当前 AW 的 [Adapter 能力目录](../../src/aw/crates/aw-adapter/src/adapter/catalog.rs)
为 `qoder` 和 `qoder-sdk` 声明 `input.submit`、`tool.before`、`tool.after`。
Qoder CLI 使用 `UserPromptSubmit`、`PreToolUse`、`PostToolUse` 原生回调。
Qoder SDK 的 Bridge 管理 daemon/Query 生命周期，并通过原生回调与执行接点完成检查
和结果采用。`model.before_request` 在当前[能力说明](../../docs/user-guide/zh/aw/capabilities.md)
中仍为规划项。各模式限制以 [Qoder](../../docs/developer-guide/zh/aw/agents/qoder.md)
和 [Qoder SDK](../../docs/developer-guide/zh/aw/agents/qoder-sdk.md) 指南为准。

| 需求 | 当前 Source 能提供的内容 | 不使用原生 Hook 时的条件 |
| --- | --- | --- |
| TLS 写入观测 | PID、线程、时间、长度、摘要、丢失和 Coverage | 实际发送进程使用受支持的动态 OpenSSL；逐版本确认命中 |
| Instance/Scope 关联 | 已有真实 AW fixture 的私有导入器 | 仍需合法实例生命周期和只读身份导出；不能从 PID 推导 Ready |
| `input.submit` 观察/阻断 | 无提交边界、输入对象及同步回执 | 需要新的应用输入接点及阻断采用机制 |
| `tool.before` 观察/阻断 | 无工具身份、参数及执行前暂停点 | 需要新的语义执行接点；本地工具可能完全不产生 TLS 写入 |
| `tool.after` 观察/替换 | 无工具结果及授权替换槽 | 需要新的结果交付接点及采用确认 |
| 模型请求检查/改写 | 仅 TLS API 入口元数据，发送采用为 unknown | 需要请求重组、协议语义、授权动作和服务端结果验证；当前未实现 |

传输观测可独立于原生 Hook。真实 LangChain 模型调用已通过采集验证；Claude Agent
SDK 完成本机模型调用，但实际 bundled 发送进程缺少受支持的动态 libssl；Qoder 认证后
的本机直连 fixture 受到实测账号自定义 Provider 能力限制，传输路径仍未验证。具体结果见[SDK 实测矩阵](POC_REPORT_zh.md#不使用原生-hook-的-sdk-验证)。
现有解析器识别映射中的 `libssl.so` 符号；静态链接、其他 TLS 库和 ABI 需要单独支持。

## 高代码 SDK 工具的两类执行检查

纯内存计算工具可以完全不产生带有工具语义的 TLS 或 OS 操作。传输摘要也没有工具名、
参数及调用身份。因此，下列实验分别验证各自的执行接点：

| 机制 | 已验证行为 | 接入统一 Hook 的剩余工作 |
| --- | --- | --- |
| 外部 OS launcher | Qoder SDK、Claude Agent SDK、LangChain 工具分发中，seccomp 拒绝 exec 和写方式打开文件 | 定义受支持的副作用策略与生命周期；工具体已经进入，内存和继承 FD 的副作用仍可能发生 |
| 固定版本运行时注入 | LangChain 同步/异步 `BaseTool.run/arun` 可在进入工具体前检查并阻断，无需原生回调 | 构建合法登记的 Adapter，提供调用身份、参数、同步 AW 决策、超时策略及采用确认；直接调用原函数会绕过当前检查 |
| 只读 TLS Source | 无 SDK Hook 时采集受支持发送进程的写入 | 为查询消费者提供来源与 Coverage；不会自动形成工具调用或阻断接点 |

可以继续采用受控启动、固定版本的运行时 Adapter 完成语义检查，并组合 OS 副作用限制
和传输证据。当前实验尚不能拦截任意已运行 Agent 的全部工具。Qoder、Claude 的工具入口
检查仍需在完整 Agent 循环中验证；本次用例仅覆盖真实 SDK 工具分发。AW 已有 Qoder 和
Claude MCP Adapter 会在已登记工具 handler 前检查，可作为新可选入口适配器的实现参考。

首个范围可限定一个已知工具分发入口、一个活跃 Query，准入声明该具体路径。版本不支持
或检查入口缺失时，应在受保护任务准入前拒绝；进程发现不能推导全工具覆盖。当前 fixture
检查仅返回预设 block 结果，不持有 AW 控制权限；本实验未改变生产 Adapter 合同和原生
能力声明。

## 统一框架中的职责

拟接入链路为：

```text
受授权的 Qoder / SDK Worker
  -> 独立 BPF Source -> 有界私有元数据存储
  -> 只读 Instance/Scope 关联 -> Observation Provider 查询/投影
  -> AW 上下文证据、诊断或现有 Hook 的策略输入
```

Source 持续采集；未来的 `aw-provider-observation` 按次执行查询/投影，遵循现有
Provider 调用生命周期。已有原生 Hook 可在自己的触发点读取这些补充证据；关闭原生
Hook 后，Source 不会自动产生 `tool.before` 等触发点。若要增加独立的 transport
事件消费路径，应先评审来源、准入和 Coverage 合同，不能将本包私有 Schema 直接注册
为正式 Hook。

Source 仅持有获准进程的描述符，不接收完整 AW 控制 Socket。未来身份桥应提供带代次、
有效区间和撤销状态的只读映射，覆盖实际发送 Worker 与 owner 的关系。当前 fixture
通过同 PID exec 保留 owner 身份；真实 SDK 子 Worker 的归属链尚需实现验证。同 PID
多实例保持 ambiguous，无法关联的进程保持 Observed；即使只允许一个 Query，也需要
核验身份与有效期。

事件保留 `source`、ProcessRef、单调时钟域、摘要范围和丢失统计。单次 write 的摘要
不能证明对端收到；缺少最终 summary 时区间丢失为 unknown。只读证据不授予执行权限。

## 小规模受限试点

首个候选场景为单机、固定 Qoder/SDK/Worker 版本、一个受管 Worker、一个活跃 Query，
仅用于诊断与辅助审计。允许缺口时明确呈现，不把它作为安全阻断或完整审计的唯一来源。
大规模并发和极限吞吐优化可后置，首轮应验证真实低频任务的可用性。

| 下一步 | 小规模验收内容 |
| --- | --- |
| 确认真实接点 | 定位实际发送 PID、TLS 库和符号；用授权的合成任务验证命中，不采集生产正文 |
| 核验归属 | 对照真实 AW 登记、owner/Worker 身份及 Query 生命周期，验证退出和重新启动时不串实例 |
| 测量使用负载 | 在选定任务速率和突发下核对命中、丢失、延迟、CPU/RSS；报告可持续范围 |
| 管理采集生命周期 | 先验证最长 30 秒窗口；持续运行需增加有界轮转、状态导出和重启缺口处理 |
| 固定权限入口 | 固定经审核的程序和对象资产；限制附着目标，处理 helper/collector 退出及权限不足 |
| 接入查询消费者 | 实现只读身份桥与独立查询 Provider，验证 unknown/缺失证据在消费侧的行为 |

当前 TLS fixture 带间隔用例及 LangChain 模型传输通过；Qoder 模型传输、Claude 发送
路径支持和受限生产试点仍待完成。
已有 helper 使用实验操作者指定的动态库和 BPF 对象，经现有 sudo 授权加载；持续部署
前须收紧到审核过的固定资产。README 中的命令只启动合成 fixture。按上述范围完成验证
后，可以单独批准只读小规模试点，无需先满足极端并发或通用生产改写的要求。
