# DeepSeek Harness

[English](../../../en/aw/agents/deepseek-harness.md)

Linux 下已用 DeepSeek Harness 0.2.1-rc.5 的 headless CLI 验证用户输入与工具 Hook。
安装 DS、AW sec-core Provider 和 CLI，启动 sec-core，并在 `DSH_HOME` 中保留
模型、凭据和原生权限配置。AW 在本次启动时通过补丁加载插件，不修改 DSH 配置文件。
AW 读取实际 DS 版本，不按版本或 profile 名称限制 Agent 启动；其他组合需按
[原生验收](../validation.md)确认工具调用的实际采用。

从 `src/aw` 目录出发，替换
[`aw.deepseek.yaml`](../../../../../src/aw/crates/aw-service/examples/aw.deepseek.yaml)
中的可执行文件和 socket 绝对路径，然后运行：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.deepseek.yaml --agent dsh -- "检查这个仓库"
```

示例将任务文本作为 headless 的应用参数传入。其他业务 profile 可替换 `argv`
中的 `headless`，并使用该 profile 的原生应用参数。`aw run` 等待插件就绪，DS 的
Agent loop 在 Hook 注册后才处理工具工作，进程退出后释放绑定。示例与 Qoder SDK、
QwenPaw 使用同一 sec-core Provider 合同：模型请求前检查用户文本中的提示词注入
和 PII；工具执行前检查参数中的 PII 和 `execute_shell_command` 命令；工具完成后
检查结果中的 PII。
接入其他可执行代码的工具时，同时更新
`providers.security.config.tools` 和 `scan-code` 选择器。被拒绝的调用不会执行。
示例的输出检查使用 `on_error: report`。DS 在 `tools/post-execute` 之后还会执行
工具的 `finalizeContent`，此时追加的文本未经过 AW 扫描，可能进入模型。
DS 0.2.1-rc.5 在最终处理后没有可写 Hook，因此 Adapter 在准入阶段拒绝
`withhold_result`。需要严格检查全部模型可见工具输出的业务暂不适用此接入。

已验证的原生边界是 `agent/pre-step`、`tools/pre-execute`、`tools.guard` 和
`tools/post-execute`。`input.submit` 检查待提交用户消息中的文本块；配置该检查后，
含非文本内容的提交会被拒绝。拒绝的轮次不会发起模型请求。DS 模型配置和权限仍由
DS 管理。

AW 配置词汇识别 16 个 Hook 名称。公共 Provider 执行链目前支持 `input.submit`、
`tool.before` 和 `tool.after`，此 Adapter 将这三个事件映射到 DSH 插件。其余名称
需要先定义 AW 公共 Provider 执行与效果合同，并由 Adapter 验证相应原生边界，
才能在 DSH 上启用。DSH 的生命周期通知本身不能形成策略决策及其生效保证。

当前自动化的真实工具调用
验收覆盖 headless；Web、SDK 和 ACP 已验证插件就绪，尚未完成工具调用验收。
自定义 profile 也需验证实际工具调用。
DS 面向模型的 `skill` 工具经过 `tool.before`，其参数只含 Skill 名称；具体对象由
工具内部解析。用户显式调用 `/skill` 时，DS 还会在 `agent/pre-step` 直接注入内容。
目前这两条路径都未向 AW 提供解析后的 Skill 对象，因此 Adapter 会在准入阶段
拒绝 `check_skill` 步骤。工具结果文本替换仅支持成功的根调用单文本块，且保留的
value 和元数据必须安全；原生 value 替换和嵌套程序调用结果保留原生内容。
