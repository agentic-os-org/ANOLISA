# DeepSeek Harness

[English](../../../en/aw/agents/deepseek-harness.md)

DeepSeek Harness 通过 `aw run` 使用一份 AW 配置接入 Provider。Adapter 把 DSH 的输入和
工具回调转换为 AW 事件；Provider 根据事件内容返回观察、阻断或结果替换决定。
模型、凭据、工具权限及 profile 仍由 DSH 配置。当前在 Linux、DSH 0.2.1-rc.5 和
Node 24.15.0 上验证了 headless 的实际调用。

## 准备与配置

先按[接入教程](../integration.md)构建 AW，安装并初始化 DSH，在 `DSH_HOME` 中配置模型
和凭据。准备一个实现 [`aw-provider/v1alpha1`](../providers/protocol.md) 的 Provider。
仓库的[完整示例](../../../../../src/aw/crates/aw-service/examples/aw.deepseek.yaml)使用
sec-core；沿用示例时须替换 Provider、CLI 和 socket 路径，并按
[sec-core 接入说明](../providers/sec-core.md)启动后端。也可以使用自己的 Provider。

从仓库的 `src/aw` 目录复制示例：

```bash
cp crates/aw-service/examples/aw.deepseek.yaml ./aw.yaml
```

把 `spec.agents.dsh.argv` 中的可执行文件改为实际 DSH 路径，并选择业务使用的 profile。示例选用 `headless`；`aw run` 后面的任务文本会作为该 profile 的应用参数传入。

使用其他 Provider 时，在同一份 `aw.yaml` 的 `spec` 下替换 `providers` 和 `events`。
下面的操作名只是配置位置示例，须改成该 Provider 在 `describe` 中声明的名称；
`config` 也须填写其实际设置。

```yaml
providers:
  policy:
    protocol: aw-provider/v1alpha1
    transport:
      type: stdio
      location: agent
      argv: [/absolute/path/to/provider]
    timeout_ms: 4500
    max_output_bytes: 4096
    config: {}
events:
  input.submit:
    enabled: true
    required: true
    steps:
      - id: check-input
        provider: policy
        operation: check_input
        effects: [observe, block]
        on_error: block
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-tool
        provider: policy
        operation: check_tool
        effects: [observe, block]
        on_error: block
  tool.after:
    enabled: true
    required: true
    steps:
      - id: transform-result
        provider: policy
        operation: transform_result
        effects: [observe, replace_result]
        on_error: report
  security.violation:
    enabled: true
    required: true
    steps:
      - id: check-final
        provider: policy
        operation: check_final
        effects: [observe, block]
        on_error: block
```

AW 启动时调用 Provider 的 `describe` 和 `validate_config`，核对操作、事件与效果。运行时每个步骤接收标准化事件并返回候选效果；Adapter 再把决定应用到 DSH：

| AW 事件 | DSH 接点 | Provider 决定的效果 |
| --- | --- | --- |
| `input.submit` | `agent/pre-step` | 检查本次提交的用户文本；`block` 拒绝该次提交 |
| `tool.before` | `tools/pre-execute` | 检查工具名和参数；`block` 阻止执行 |
| `tool.after` | `tools/post-execute` | 观察工具结果，或用 `replace_result` 替换可采用的文本 |
| `security.violation` | 最终处理后、顶层 Session 提交前 | 检查最终结果；`block` 暂扣交付 |

需要最终结果阻断时，Provider 应检查 `security.violation` 收到的完整结果并在违规时
返回 `block`；`on_error: block` 使检查故障也暂扣结果。`tool.after` 的替换只适用于
成功的根调用单文本结果。DSH 的 `finalizeContent` 可能在该事件之后继续修改内容，
因此最终检查使用独立的 `security.violation` 步骤。

DSH 插件还注册原生 `tools.guard`：它在执行前同步复核 `tool.before` 已得到的决定，
阻止其他插件绕过前置回调后放行工具，不会再次调用 Provider。AW 的
`events.tool.before.guard` 引用字段会被运行时准入拒绝，这里无需设置。

## 运行与核对

在 `src/aw` 目录运行：

```bash
target/debug/aw validate --config ./aw.yaml
target/debug/aw run --config ./aw.yaml --agent dsh -- "Review this repository"
target/debug/aw status --config ./aw.yaml
```

`validate` 检查配置；`run` 还会完成 Provider 准入、插件注册及实际 Hook 调用。
验证策略时分别触发允许、工具前阻断、工具后替换和最终结果阻断：被拒绝的工具
不应执行；headless 顶层最终结果被阻断时，模型和 CLI 只接收暂扣错误结果。
需要停止按需启动的 AW 服务时运行 `target/debug/aw stop --config ./aw.yaml`。

## 适用范围

启用 `security.violation` 时，Adapter 检查已发布的 DSH 0.2.1-rc.5 Agent loop 构建；
构建不匹配会阻止启动。AW 不按版本或 profile 名称限制普通入口。Web、SDK、ACP 和
自定义 profile 已有插件就绪检查，实际输入、工具调用及最终交付须按
[原生验收](../validation.md)逐一确认。

最终检查覆盖已验证的 headless 顶层模型、Session 和 CLI 交付。嵌套/PTC 结果及
直接调用 `ToolRuntime.execute` 的消费者不经过该提交点；同进程的 `tools/result`
观察插件会在终检前看到原始结果。DSH 的 `skill` 工具只向 Hook 提供名称，无法提供
解析后的 Skill 对象，`check_skill` 步骤会在准入时被拒绝。
