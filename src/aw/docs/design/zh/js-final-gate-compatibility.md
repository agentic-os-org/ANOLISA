# JS 工具结果最终安全检查与版本兼容

[English](../en/js-final-gate-compatibility.md)

本文界定工具结果进入 Agent 模型上下文前的检查。工具已经产生的副作用仍然存在；
安全门控制结果交付，并沿用现有 `security.violation` Provider 操作。

## 边界与公共运行时

```text
原生工具 -> 原生 Post/Tokenless -> 原生最终转换
  -> 已验证的最终结果接点 -> AW security.violation
  -> 提交受检快照或暂扣 -> 模型可见提交
```

检查接点必须位于承诺覆盖的最后一次转换之后、模型首次可见提交之前。Provider
检查的快照须与实际提交的结果一致；后续改写使本次决定失效。未知结果结构或路径
不能计入覆盖。进程内更早的观察者不在这一边界内。

公共 Node Runtime 只负责三项操作：探测 `node:module.registerHooks`、校验实际加载
模块的 SHA-256 与唯一插桩锚点、在工具和 Provider 子进程启动前从 `NODE_OPTIONS`
移除本次启动的 `--import`。各 Adapter 自行维护锚点、身份映射、结果投影和原生回调。
回执区分 `registered`、`instrumented`、`verified`、`active`、`unsupported`，
记录包、Worker、入口、模块摘要和交付范围。ready 文件及 Hook 注册只证明
`registered`。

结果身份包含 AW instance、进程、session、工具调用和结果 generation；原生运行时
可提供 turn/batch 时也应纳入。必需安全门遇到身份缺失或过期时暂扣结果。本阶段无需
增加公开配置字段或改动 Provider 协议。`tool.after` 替换先于最终检查。空策略不装载
preload、最终回调，也不增加每次工具调用的进程。

OpenClaw 2026.9.7 在原生异步请求体编码完成后校验，并以不可变 JSON 字符串提交受检结果；其他版本提交独占快照。检查期间会话关闭或调用身份变化会使决定失效，非工具消息、工具元数据及其他请求字段被改写时停止模型请求。

## Bun 运行时后端

共享 `fetch-gate.mjs` 在等待语义检查之前固定序列化请求体与 headers，覆盖全局
fetch 和 Bun 连接池 FetchSession 发送，保留原生 session 实例与取消信号，检查
失败时不发送。Node 源码插桩继续复用 `module-patch.mjs`；两个后端各自覆盖运行时
实际采用的交付边界。

Claude 通过 `BUN_OPTIONS=--preload=…` 加载脚本，安装目录内的 ELF 保持原样。
SDK／CLI 指纹选择 Profile；preload 通过 `/proc/self/exe` 再次核对正在运行的
可执行文件，并检查 Bun 1.3.14 或 1.4.4。启动后移除自己的环境注入，避免传给工具
子进程。原生 Post 只记录身份，最终文本来自序列化请求。摘要回执将历史内容绑定到
相同调用与已检查字节。注册就绪需要 preload 回执；安全验收独立核对实际 HTTP
及模型回复证据。具体范围见
[Claude 工厂限制](../../../../../docs/developer-guide/zh/aw/agents/claude-sdk.md#最终工具结果安全边界)。

## 已验证的发行版证据

| Agent | 发行构建 | 已观察顺序 | 最终安全门状态 |
| --- | --- | --- | --- |
| DeepSeek Harness | 0.2.1-rc.5 | `finalize/finish` → 精确构建 preload → 顶层 Session commit | 仅既有 headless 测试范围已验证 |
| OpenClaw | 2026.9.5、2026.9.6、2026.9.7 | middleware → `after_tool_call` → 上下文转换 → Provider 请求 Hook → HTTP 请求 | 下述受控 Gateway 与显式 OpenAI-completions 路径已验证 |
| Qoder SDK | SDK 1.0.55／Worker 1.1.75、1.0.56／1.1.78、1.0.57／1.1.81，Linux arm64 | `PostToolUse` → `updatedToolOutput`／Read 转换／附加上下文 → Worker 交付 | 三版真实 Worker 后置改写红灯通过；最终安全门暂不支持 |

受测 OpenClaw 的 middleware runner 逐个等待处理器，并把上一项结果传给
下一项。内建 `tool_result` 扩展先运行这条链，随后执行已观察到的
`after_tool_call`。本地 Gateway 夹具记录：早期 middleware 看到安全结果，后续
middleware 注入 `AW_FINAL_GATE_SECRET_MARKER`，下一次模型 HTTP 请求包含标记。
这是对早期 middleware 检查的红灯基线。一次探针中的 `tool_result_persist` 修改了
持久化副本，但下一次模型请求未采用该修改，不能用它替代模型请求捕获。

已验证的 OpenClaw 路径采用两个启动期接点。对应发行版 Agent core 中
`convertToLlm` 的返回点核对源结果身份、纯文本结构，以及上下文转换后非工具消息的
来源。`@openclaw/ai/dist/transports.mjs` 的 OpenAI-completions 分支在
`onPayload` 和 `before_provider_request` 完成后、调用
`chat.completions.create` 前，检查最终 HTTP 请求。它把请求中实际采用的工具
文本送给现有 `security.violation` Provider；阻断或检查失败时，改为安全的暂扣
文案。序列化后若非工具消息或工具元数据被改写，模型请求直接停止。这两个接点分别
防止上下文转换把工具内容复制到用户消息，以及后续请求 Hook 绕过检查。

启动 admission 精确校验实际 `openclaw.mjs` 入口、Agent core、内建转换器、
会话投影、调用 ID 规范化代码，以及对应 `@openclaw/ai` 的传输与序列化
文件。Node 加载时再校验各插桩目标的字节和唯一锚点。2026.9.6 的 core 与 transport 目标摘要分别为
`96221c65ea8e15d6499ca9f817e5d7629151b42c6d9742000791f2f48f2cebcd` 和
`038e2c72e79a3e1c9d06e9c5f3a61a13acc297e1026696da24fee634a3bc4abc`；
入口摘要为 `4dd36d2ed0cb154456c4a09a951a2e8f2508a5dd6ff2fe0688d4e1c8f773fcb5`。
2026.9.5 和 2026.9.7 各自有独立的入口、Agent core、传输和依赖文件指纹，
详见 `adapters/openclaw_bundle.rs`；各插桩锚点也逐版核对。
原生结果 middleware 只记录 session、run、tool call 和工具名。OpenClaw 启动时
会注册两次插件，两次注册共享本次启动的身份表。固定传输实现可能删除调用 ID 的
标点；AW 只在同一会话、同一工具中唯一对应一项待检调用时接受映射。歧义或身份缺失
时暂扣。相同的已检查请求文本可复用摘要回执；历史文本变化时暂扣。admission
要求显式主模型使用 `openai-completions` 且不配置 fallback，运行时再次核对实际
模型 API，其他 API 在模型调用前停止。

stream resolver 同样经过指纹校验与插桩，只允许原生
`boundary-aware:openai-completions` 策略；Provider stream 覆盖和自定义会话
stream 在发送前被拒绝。缺少原生工具身份的 `bashExecution`、自定义消息、分支
及压缩摘要在转换前停止投影。会话结束或请求取消会终止活动回调、跳过后续 step，
并显式关闭未完成的最终事件。

真实 Gateway 夹具验证安全内容放行、后置 middleware 植入
`AW_FINAL_GATE_SECRET_MARKER` 后暂扣，以及 Provider 故障、超时暂扣。Provider 收到
实际序列化的工具文本，模型 HTTP 请求对危险结果只收到暂扣文案；原有 `tool.before` 阻断仍阻止
工具副作用。同一 Gateway 路径还通过了真实 `agent-sec-core` 的
`check_final_tool_output` Provider 与确定性本地 PII 扫描器。当前模型采用证据
来自受测 OpenAI 兼容序列化路径。单元测试另覆盖序列化后的请求改写；
`before_provider_request` 原生扩展的实际运行尚未验证。其他模型 API、
reloadable 策略、多模态、移除工具结果身份的上下文投影、嵌套 Agent 和后台交付
尚未覆盖。

受检的三个 Qoder Worker 都在 `firePostToolUseEvent` 之后应用 `getUpdatedToolOutput()`／
`getUpdatedMCPToolOutput()`；同一路径还可能改写 Read 的 `llmContent` 并附加 Hook
上下文。Worker 是独立版本的混淆构建。三个版本的隔离真实 Query
均验证：AW `tool.after` 读取原始安全输出，随后应用 SDK `PostToolUse` 回调
把结果改为 `AW_QODER_LATE_POST_SECRET`；Worker 发布的 `tool_result` 含该标记，
托管模型的最终回答也准确复述该标记。该红灯证明公开 Post 回调不足以提供最终保证；
本次未捕获托管模型的原始网络请求，且未安装 Qoder Worker 的可阻断最终接点。
因此三个版本仍不开放 `security.violation`。版本矩阵逐版校验 SDK 与 Worker 字节、
SDK 解析器和真实后置改写采用。隔离环境中无凭据启动
Worker 时，三版均被模型目录或认证门拒绝。

本次检查的 OpenClaw 包要求 Node `>=24.16.0 <25 || >=26.1.0`，Gateway 探针使用
Node 24.16.0。2026.9.6 的 middleware runner 摘要为
`23deae7d2d70f52451eeb8bb64ab8bd9542d464cabecdd80db6b771dca0278db`。
Qoder SDK 的 `dist/index.js` 摘要为
`e76eb037555c5b8e74c0cbe7df9071f343be8d081d4c27738cb73491bdf827c1`；
Linux arm64 Worker 摘要为
`dcab64392be3381ec82c3def0ca60667f1d8c5d834733f0fc6bb028e349f83c6`。
这些摘要标识已检查文件，不代表最终安全门 Profile 已验证。

## Admission 与覆盖范围

Qoder SDK 当前未在 Adapter catalog 声明 `security.violation`。OpenClaw 只对上文
已验证的 2026.9.5、2026.9.6、2026.9.7 受控 Gateway Profile 开放该能力；
其他版本和配置拒绝
admission。普通 `tool.after` 与空策略启动沿用既有行为。必需安全门遇到不支持的
Node loader、模块摘要、唯一
锚点、SDK／Worker 组合或交付路径时必须拒绝。多模态、嵌套、后台、直接工具运行时
及第三方早期观察者均需独立证据。

使用发行版 OpenClaw、符合其 Node engine 范围的 Node 和本地构建的 `aw` 运行
红灯基线：

```bash
python3 src/aw/crates/aw-cli/tests/openclaw_gateway.py \
  --package /absolute/path/to/node_modules/openclaw \
  --node /absolute/path/to/node \
  --aw /absolute/path/to/aw \
  --output /absolute/path/to/empty-output \
  --model-turn --final-gate-order-probe
```

输出的 `final-gate-baseline.json` 记录 middleware、Post 和模型可见阶段。
`baseline_red` 表示标记进入模型夹具，不计作最终安全门通过。原生验收矩阵固定
发行包并调度该用例。增加 `--final-gate-enforce` 验证最终检查；再增加
`--final-gate-provider-error` 验证故障暂扣。Qoder Worker 源码检查也不计作
模型采用证据。增加 `--sec-core-provider /absolute/path/to/aw-provider-sec-core`
可验证真实 Provider。
Qoder 的授权隔离红灯用例运行 `post-native.py --case late-mutation`；它要求测试凭据，
只输出结果布尔值并在退出时清理 Worker 和临时配置，不保存凭据。
