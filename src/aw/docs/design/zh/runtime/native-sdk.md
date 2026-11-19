# 应用自有 SDK 协议

[English](../../en/runtime/native-sdk.md)

`aw-sdk/v1` 扩展让应用已安装的回调直接使用既有 Service 和 Adapter。应用拥有原生进程，
安装回调并确认 Ready；Service 负责策略准备、Scope 隔离、Provider 执行和持久元数据。

## 协商与归属

通过 `aw-service/v1alpha7` 读取 `status`，确认 `sdk_protocol: aw-sdk/v1` 后才发送
`registration` 的 `register_sdk` 动作。每次请求保留发现时的 identity。旧服务缺少该扩展时，
客户端报告协议不支持；不能改用旧动作重试登记。`register`、`register_native` 保留原严格字段。

`register_sdk` 在普通登记字段之外要求 `sdk_version` 和 `availability`，后者取
`required` 或 `native_fallback`。owner 是经过身份核验的应用 PID 和启动时间；同进程多实例
使用不同恢复 key。同 key 改变模式或输入会被拒绝。丢失登记回复后按原 key 查询，并检查成功
回复内的失败终态。防重放容量、owner 退出清理和代际检查沿用
[独立登记](local-service.md#independent-registration)。

`native_fallback` 要求注册目标适用的已启用事件都设置 `required: false`。策略更新按同一目标校验
候选配置；如有冲突，当前策略继续生效。Provider 仍可明确拒绝，`on_error: block` 保留在线
行为。策略错误和 Provider 故障均不能作为传输不可用的依据。

## 通用语义入口

应用已安装回调时，可声明 `adapter: sdk`、`version: aw-hook-source/v1`、
`entrypoint: application`。当前支持 `input.submit` 的 observe/block、
`tool.before` 的 observe/block、`tool.after` 的 observe，可按实际安装范围选择子集。
不支持的事件、效果、失败动作和协议版本会在准入时被拒绝。应用自行启动；该 profile
没有托管启动器。

两种语言均提供 `task.emit(event, payload)`。客户端绑定协议版本和事件名，按 Scope
快照的顺序调用 `sdk_callback`；Adapter 归一化后继续经过原有 Service、Host、Provider
和审计链路，共享事件预算。

| 事件 | 必填字段 |
| --- | --- |
| `input.submit` | `session_id`、`turn_id`、`text` |
| `tool.before` | `session_id`、`call_id`、`name`、对象类型的 `input` |
| `tool.after` | before 字段与 JSON `result` |

身份字段和工具名须为非空白字符串，最多 512 字节。未知信封字段被拒绝，binding 和
instance 身份由 Service 填充。来源提供稳定的原生 session/call 身份及规范工具名；
重复或结果未知的回调不得作为新的原生执行重试。

返回值为 `decision: continue | block` 和
`coverage: checked | unavailable | not_configured`。`checked` 表示配置步骤或观察已完成，
应用仍须在提交输入、执行工具前采用 `block`。仅观察不授予阻断或最终交付保证；
未配置策略与传输降级分别报告。`install(routes)` 负责确认实际安装的边界，控制效果
须经应用侧采用验证后才能声明。

公共协议使用统一的 16 个 Hook。Python 的 `aw_sdk.sources` 和 TypeScript 的
`@anolisa/aw-sdk/sources` 通过显式包装函数或对象方法插桩实现上述三个边界，每个会话使用
一个 Admission 和一个 Task。Qoder、QwenPaw 通过原生映射支持更丰富的效果。
原生 Hook 来源验证了事件语义和采用行为后也可接入同一入口。显式包装复用现有执行引擎，
可独立于 dowhen 运行。自动发现、逐 Hook 来源选择与 ptrace 属于后续交付。

## Ready、任务 Scope 与回调

登记返回 Adapter 的标准路由和策略修订；实际安装回调后才确认 Ready。支持入口沿用普通
resident 路由：Qoder SDK、QwenPaw、AgentScope、Pi、Claude SDK。Capabilities 描述已安装
接点，公开登记不设 Agent／Worker 精确版本列表；原生框架验收仍以实际集成和测试版本为准。

每个任务打开 Scope，读取 `scope_policy`，安装／确认返回路由，并在交接期限内发送
`acknowledge_scope`。SDK Scope 必须先读取策略。确认前或直接使用 root 调用回调都会失败。
直接操作继续检查 owner 或直接子进程身份。Ready/ACK 记录应用对接线的确认，
每个 Query 的实际接线及效果采用仍需原生验收证明。

`SdkCallback` 对应 wire 方法 `sdk_callback`，字段为 `instance_id`、`event`、`step_id`、
`native_input`（原生 JSON 对象）和 `close`（布尔值）。Service 从不可变 Scope 选择 Adapter
及步骤错误策略，复用现有 CLI 路径的 Adapter 归一化和响应处理。返回 `{code, stdout, stderr}`，
输出使用字节数组；code 2 保留原生拒绝。Provider 故障返回 `sdk_step_failed`，不得转为回退。
回调沿既有 Hook 路径记录原生元数据，不将返回效果声明为应用已采用。

按返回的原生 Hook 顺序调用；同事件跨调用共享原预算及单次步骤账本。`close: true` 等待该
关联清理，原生任务清理后关闭 Scope。回复未知时不能重试回调、原生工具或 Provider 步骤。
释放只影响本应用实例及其 Scope，共享服务继续运行。
SDK 控制消息与 `close: true` 的 `sdk_callback` 使用有界控制连接预留；普通 SDK 回调仍受
业务连接上限约束，Provider 占满业务连接时仍可发出清理请求。

## 客户端可用性合同

客户端为首次传输失效检测设置期限。回退任务失联后，后续回调立即返回原生中性结果，忽略迟到
回复，保留已采用效果。单个有界后台流程重新发现代际、登记并确认真实 Ready；新登记只承接
新任务。取消、明确拒绝、配置／协议错误和 Provider 故障继续分别处理。状态机由语言客户端
实现，Service 记录选定模式并检查策略兼容性。

## 协作式控制

`status.sdk_control_protocol` 声明 `aw-sdk-control/v1`。`sdk_control` 操作支持
`poll`、`list`、`request`、`query`、`acknowledge` 与 `withdraw`。应用稳定 key、
经核验的 PID／启动时间和 Service 代际共同标识发现记录，与可替换的登记分离。
轮询发布不可变 target／mode 和 Ready 句柄，并取得最近的有界意图。记录总量受 Service
binding 上限约束；owner 退出或十秒无公告后清理，待确认请求保留到其期限。

CLI 使用准确发现 ID 选择应用。同 ID 重试必须保持意图与超时一致，同一应用只接受一个
待确认意图。Service 持久记录请求与确认。attach 确认须携带该应用持有且已 Ready 的 SDK
登记；detach 确认前须释放此前的根实例。只有应用能够确认，控制操作不启动 Agent、安装
回调、改变可用性模式或转移所有权。

语言客户端在空闲 detach 期间阻止任务进入。attach 在控制确认后才发布连接；确认回复
丢失时，使用同一登记和请求恢复。过期请求不能发布迟到 Ready，结果未知的回调与工具
均不重试。发现记录保留最近请求结果；CLI 超时表示确认未知，调用方应先检查发现状态
再发出新请求。withdraw 只删除发现记录。这些规则复用已有私有 Unix socket 和 peer 边界。

## 验证

共享协商及回调 fixture 位于 `sdk/tests/fixtures/contract.json`。从 `src/aw` 执行：

```bash
cargo test --locked -p aw-service --lib runtime::registration
```

覆盖协议／模式冲突、查询恢复、owner／Ready 门控、标准回调执行、拒绝、Provider 错误和
防重放。两语言包 gate 覆盖协作选择、忙时脱离、迟到 Ready 与恢复；原生框架验收见接入指南。

[TypeScript 接线](../../../../../../docs/developer-guide/zh/aw/deployment/native-sdk.md)实现该生命周期，并打包 Adapter 的 Qoder 公开 Hook 源码。离线合同与安装测试纳入默认 AW gate。

## 仅工具后的延迟任务准入

进程内公共底座归 `sdk/typescript/src/admission.mjs` 和
`sdk/python/src/aw_sdk/admission.py`，复用既有 Instance／Task 合同。
Source 先安装原生捕获，再为每个请求向 Admission 传入已安装事件集合。
安装 input.submit 时，prepare 在输入边界等待；仅工具策略时，在首次工具进入前等待。
后续回调和原生清理沿用同一不可变 Scope。准入门不增加 Provider Hook，配置的 before／after
步骤继续在原阶段执行。可选可用性按准入期限回退，明确错误和取消继续传播。

Qoder 公开 Hook 复用 Admission 的就绪等待、取消与唯一 Task。
QwenPaw 配置式插件通过 DeferredScope 复用同一机制，只补充 resident 调用账本激活、
Coordinator 流、原生 finalizer 与经检查的模型／事件发布，采用 required 可用性。
QwenPawPlugin.from_config 按配置启用输入捕获；仅 before 或 after 策略允许模型先于 AW 启动。
兼容入口 AfterOnlyPlugin 保持原 after-only 限制。工具策略的纯文本回复不建远端 Scope。
提前到输入边界的保护需要在接收新请求前重新安装 Source，无法覆盖已发起的模型调用。

Python SDK Task 同时拥有 required 原生 Scope 续租：按服务间隔覆盖长模型／工具等待，
锁存原生接点或 Service 故障，向 owner 通知一次，释放 Scope 前停止并汇合监视任务。
协作 detach 先关闭 fallback Task，再释放根注册，避免并发关闭重复释放已删除的子 Scope。
TypeScript Admission 将各等待者的取消与共享操作、内部关闭分开。

配置加载继续复用 AW 权威离线解析器。Service 保留能力准入、Ready、Scope 修订，
Hook 经 Host／executor／Provider 执行；Adapter 保留原生字段映射和效果采纳。
部署时可以通过 CLI 预编译目标的 Descriptor，供 SDK 启动时读取。
通用 Source resolver 仍待实现。

`--sdk-pid` 选择已预装 SDK 的协作应用。CLI 在启动服务前固定既有 ProcessIdentity，
通过 launch_service 准备 AW，等待唯一匹配的 SDK 声明，再发送原控制意图。
SDK 确认及进程所有权仍由应用持有。后端启动沿外部部署，不引入第二套生命周期或 Provider
协议。

## Qoder 公开 Hook 的延迟准入

配置式接入默认采用 `native_fallback`，在安装监听器前拒绝适用于该目标的已启用 required 事件。
调用方可显式选择 `required`；Service 在登记和策略更新时继续执行可用性兼容校验。
接入模块在创建 Query 前安装原生公开回调。未启用 input.submit 时，模型派发不创建
Scope；首次工具前回调等待协作 Ready，并按该 Query 的目录和复制后的环境创建一个
不可变 Task。有序回调复用 Adapter 的规范化和效果处理，后回调使用同一 Scope。
required 模式在工具派发前等待 attach，超时拒绝；可选模式在尚未 Ready 时立即将该 Query 固定为原生回退。
输入策略在原生 UserPromptSubmit 处准入；初始化未安装输入来源时，新增输入路由会在
登记时拒绝。原生 Query 关闭和回调取消先于 Scope 释放。该接线不安装运行时补丁。
