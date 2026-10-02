# 集成其他前端

[English](../../../../en/user-entrypoint/cosh-ng/core/overview.md)

`cosh-core` 在没有交互式终端 UI 的情况下运行 Agent。日常使用请启动 `cosh`；只有在其他前端需要 JSONL 进程、一次性 prompt 或会话管理时，才直接调用 `cosh-core`。

## 启动 Core

```bash
# One prompt, then exit
cosh-core --headless "Inspect disk usage; do not modify anything"

# Long-running JSONL process
cosh-core --headless

# Resume or compact a saved conversation
cosh-core --headless --resume <session-id>
cosh-core --headless --resume <session-id> --compact

# Handle one provider-free registry request from stdin
cosh-core --registry
```

stdin 不是 TTY 时，`cosh-core` 会自动选择 headless 模式。在 headless 和 registry 模式下，stdout 只输出 JSONL 协议消息；日志写入配置的日志文件或 stderr。

## 集成常用选项

| 选项 | 用途 |
|---|---|
| `--model <name>` | 为本次进程覆盖配置中的模型 |
| `--approval-mode <mode>` | 选择 `recommend`、`auto` 或 `trust` |
| `--allowed-tools <names>` | 让精确匹配的工具名跳过审批 |
| `--tools <selection>` | 暴露 `default`、`empty` 或逗号分隔的工具子集 |
| `--bare` | 忽略项目配置、Hooks、Skills、Extensions 和持久化 |
| `--resume <id>` | 选择当前工作空间中的已保存会话 |
| `--compact` | 压缩选中的会话并退出 |
| `--enable-shell-evidence-tool` | 向 cosh-shell 暴露有边界的终端证据 |

`--tools` 决定模型能看到哪些工具，`--allowed-tools` 改变审批边界；把工具加入允许名单可能授予真实执行权限。

## 连接前端

1. 启动 `cosh-core --headless`，保持 stdin/stdout 打开。
2. 发送 `subtype: "initialize"` 的 `control_request`，再逐行发送 JSON 对象形式的 `user` 消息。
3. 读取流式输出，并使用相同 request ID 回复 Core 发出的 `control_request`。客户端需要在发生时处理工具审批、用户提问和认证。
4. 前端结束时发送 `subtype: "shutdown"`。

消息示例见 [Headless 模式](headless-mode.md)，完整 schema 见[IPC 协议参考](../../../../../developer-guide/zh/cosh-ng/ipc-protocol.md)。凭据配置见 [Providers](providers.md)，工作空间和持久化设置见[配置](../configuration.md)。
