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

## 基线发行版证据

| Agent | 发行构建 | 已观察顺序 | 最终安全门状态 |
| --- | --- | --- | --- |
| DeepSeek Harness | 0.2.1-rc.5 | `finalize/finish` → 精确构建 preload → 顶层 Session commit | 仅既有 headless 测试范围已验证 |
| OpenClaw | 2026.9.6 | 顺序执行结果 middleware → `after_tool_call` → 模型请求；后续 middleware 可改写前一项结果 | 暂不支持；须验证原生最终提交点 |
| Qoder SDK | SDK 1.0.56、Worker 1.1.78、Linux arm64 | `PostToolUse` → `updatedToolOutput`／Read 转换／附加上下文 → Worker 交付 | 暂不支持；公开 Post 回调早于最终转换 |

OpenClaw 2026.9.6 的 middleware runner 逐个等待处理器，并把上一项结果传给
下一项。内建 `tool_result` 扩展先运行这条链，随后执行已观察到的
`after_tool_call`。本地 Gateway 夹具记录：早期 middleware 看到安全结果，后续
middleware 注入 `AW_FINAL_GATE_SECRET_MARKER`，下一次模型 HTTP 请求包含标记。
这是对早期 middleware 检查的红灯基线。一次探针中的 `tool_result_persist` 修改了
持久化副本，但下一次模型请求未采用该修改，不能用它替代模型请求捕获。

Qoder Worker 在 `firePostToolUseEvent` 之后应用 `getUpdatedToolOutput()`／
`getUpdatedMCPToolOutput()`；同一路径还可能改写 Read 的 `llmContent` 并附加 Hook
上下文。Worker 是独立版本的混淆构建。源码观察只定位候选接点；授予能力前仍须
在原生 SDK／Worker 中捕获真实模型输入。

本次检查的 OpenClaw 包要求 Node `>=24.16.0 <25 || >=26.1.0`，Gateway 探针使用
Node 24.16.0。其 middleware runner 摘要为
`23deae7d2d70f52451eeb8bb64ab8bd9542d464cabecdd80db6b771dca0278db`。
Qoder SDK 的 `dist/index.js` 摘要为
`e76eb037555c5b8e74c0cbe7df9071f343be8d081d4c27738cb73491bdf827c1`；
Linux arm64 Worker 摘要为
`dcab64392be3381ec82c3def0ca60667f1d8c5d834733f0fc6bb028e349f83c6`。
这些摘要标识已检查文件，不代表最终安全门 Profile 已验证。

## Admission 与覆盖范围

OpenClaw 和 Qoder SDK 当前未在 Adapter catalog 声明 `security.violation`，必需
最终策略因此会拒绝 admission；普通 `tool.after` 与空策略启动沿用既有行为。
只有精确构建、可等待的最终接点、身份关联、Provider 故障暂扣及原生结果采用测试
全部通过，才能改变该状态。必需安全门遇到不支持的 Node loader、模块摘要、唯一
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
发行包并调度该用例。Qoder Worker 源码检查也不计作模型采用证据。
