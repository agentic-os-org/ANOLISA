# AgentScope SDK

[English](../../../en/aw/agents/agentscope.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 AgentScope SDK

使用安装了 `agentscope==2.0.6` 的 Python 环境；这与 QwenPaw 已验证的
AgentScope 2.0.10 运行时分开。`argv` 指定 SDK 环境的 Python 和 `.py` 应用或
`-m MODULE`。AW 校验该 SDK 版本，提供 `aw_agentscope` 与受保护的运行配置，
等待真实公共工厂安装 middleware 后生成的回执。

在 `src/aw` 中，将该环境的 `python3` 放到 PATH 后执行：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

示例使用确定性的离线 model 与本地 Echo 工具，无需凭据。before 布尔 Hook 返回
false（没有 block 命中），after 返回 true（observe）。把 before 命令改为
`printf true` 即可拒绝工具；原生 final response 为 denied，after 观察该拒绝。
修改配置之前先停止服务。

已有应用在**每个**公共工厂中使用 `AwBridge().create_agent(...)`，原样传入已有
middleware 列表。可信参数注入应先于追加的 AW middleware。readiness 回执只
覆盖所构造的 Agent，不覆盖其他工厂或直接 dispatch 路由。
[工厂合同](../../../../../src/aw/docs/design/zh/adapters/agentscope-adapter.md)说明顺序、预算、
调用身份与覆盖边界。

before 等待并可在工具副作用前阻断；after 等待一次 final `ToolResponse`，保留
success/error/interrupted，不重复中间 stream chunk。`on_acting` 之前的原生
参数/权限拒绝、直接 Toolkit 调用和外部执行请求需要另接。launcher 支持单个
前台 worker；fork 型 Gunicorn supervisor、多 worker 和远端部署不在此合同内。
常驻进程必须显式管理 service 每实例 1024 个事件关联的容量限制。

接入 PreTool 重试建议和最终 PostTool 压缩时，复制
[SDK 生命周期示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)，
替换所有路径，并在 sec-core 映射 Toolkit 的准确工具名和参数指针。示例先做安全检查，
再执行 Tokenless PreTool；工具完成后先观察原始结果，再压缩一个成功的文本槽。
custom/native after Hook 放在最终压缩步骤之前；启用替换后 AW 按声明顺序执行此链。
native 和观察 Hook 不替换响应；只有最后一个已准入的结构化替换步骤可以修改模型
可见文本。Hook 故障按 report 处理后，后续步骤仍收到原始输入。
只有观察的链保持原生逆序退出。

PreTool 拒绝原始调用，向模型返回完整重试参数；每次新尝试重新经过原生权限和
sec-core。带 RTK 归属的重试即使使用新 call ID 也不会重复压缩。PreTool 需独立安装
RTK，PostTool 不依赖 RTK。文本槽适用条件、故障处理和遥测配置见
[Tokenless 行为](../../../../user-guide/zh/aw/tokenless.md)。无效的最终响应替换指令会报告故障并保留原结果。
