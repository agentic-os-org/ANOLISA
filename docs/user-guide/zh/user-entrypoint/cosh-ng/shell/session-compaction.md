# 会话压缩

[English](../../../../en/user-entrypoint/cosh-ng/shell/session-compaction.md)

会话压缩会缩短发送给模型的对话历史，但不会删除已保存的会话记录。长时间Agent会话接近上下文上限时，可以使用此功能。

## 手动压缩

在Shell提示符中运行下面的命令：

```text
/session compact
/session compact status
/session compact cancel
```

`/session compact`作用于当前活跃或已选择的可恢复cosh-core会话。压缩期间仍可使用Shell，但Agent请求会暂停。`status`显示后台任务状态；`cancel`不会改变已保存的会话记录或当前模型上下文。

压缩只使用已完成的Agent run，永远不会摘要进行中的run。如果没有完整前缀可压缩、Provider失败，或任务运行期间会话发生变化，`cosh`会返回可操作的错误，并保留之前的模型上下文。

## 自动压缩

自动压缩默认启用。模型可见历史达到可用上下文的70%后通常启动，目标是压缩到30%，并原样保留最近两个已完成的Agent run。达到90%时，如果下一次Provider请求需要更多空间，会先执行紧急保护。

这些限制只影响发送给模型的内容，已保存的会话记录仍然完整。降低模型输出上限可以给历史留出更多空间，但也会缩短单次回复的最长长度。

## 配置

```toml
[session.compaction]
enabled = true
auto = true
trigger_ratio = 0.70
emergency_ratio = 0.90
target_ratio = 0.30
preserve_recent_runs = 2
```

可选覆盖项包括`auto_compact_token_limit`、`model_context_window`和`model_max_output_tokens`。修改前请先阅读[配置](../configuration.md)。
