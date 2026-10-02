# Headless 模式

[English](../../../../en/user-entrypoint/cosh-ng/core/headless-mode.md)

Headless 模式通过 stdin/stdout 提供逐行 JSON 协议。脚本使用一次性形式，前端适配器使用长驻形式。

## 一次性 prompt

```bash
cosh-core --headless "Check disk usage; do not modify anything"
```

Core 会流式输出事件，写入 `assistant` 消息，最后输出 `result` 对象。脚本需要不同设置时，可以使用 `--model`、`--approval-mode`、`--tools` 或 `--allowed-tools`。

## 长驻客户端

启动进程，并且每行发送一个 JSON 对象：

```bash
cosh-core --headless
```

```json
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}
{"type":"user","message":{"role":"user","content":"List files in current directory"}}
```

首个响应是初始化确认，随后是系统摘要：

```json
{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{...}}}}
{"type":"system","subtype":"init","session_id":"...","session_resumable":true,"model":"...","tools":[...]}
```

只有 `session_resumable` 为 `true` 时才保存 `session_id`。客户端应持续读取 `stream_event`，直到消息结束，再处理最终的 `assistant` 和 `result` 消息。

## 控制请求（Control request）

Core 可能因为工具决策或用户输入而暂停。使用相同的 request ID 回复 `control_response`：

```json
{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}
{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow","toolUseID":"toolu-1"}}}
```

Core 还可能发出 `ask_user`、`auth_required` 和可选的 `shell_evidence` 请求。前端可以使用下面的消息中断或停止进程：

```json
{"type":"control_request","request_id":"stop-1","request":{"subtype":"shutdown"}}
```

缺少凭据时，使用所选 `provider_id`、对应表单的 `values` 和 `persist` 回复 `auth_required`。可选 Provider 见 [Providers](providers.md)。

## 恢复或压缩会话

```bash
cosh-core --headless --resume <session-id>
cosh-core --headless --resume <session-id> --compact
```

ID 的作用域是工作空间。未传入 `--workspace <path>` 时，工作空间使用当前目录。设置 `session.auto_persist = false` 后，历史只保存在内存中，此类会话不能恢复。

## 结果与错误

成功轮次以类似下面的 result 结束：

```json
{"type":"result","subtype":"success","is_error":false,"result":"completed","session_id":"...","duration_ms":1234}
```

失败结果使用 `is_error: true` 并包含 `errors`；会话加载或持久化失败还会包含 `session_error_code` 和 `session_error_phase`。无效 JSONL 输入会输出错误 result，并以非零状态退出。stdout 只保留协议消息，诊断信息写入其他日志。

完整 schema 见开发者[IPC 协议参考](../../../../../developer-guide/zh/cosh-ng/ipc-protocol.md)。
