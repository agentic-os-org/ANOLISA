# Hooks 参考

本文档提供 copilot-shell hooks 的技术规范，包括 13 个已接入事件的
JSON Schema 和 API 细节。

## 全局 Hook 机制

- **通信方式**：`stdin` 接收输入（JSON），`stdout` 输出结果（JSON），`stderr` 输出日志
- **退出码**：
  - `0`：成功，`stdout` 被解析为 JSON
  - `2`：系统阻止，操作被中止，`stderr` 作为拒绝原因
  - `其他`：警告，非致命失败，CLI 继续执行
- **黄金法则**：脚本**不得**向 `stdout` 输出 JSON 以外的任何内容

---

## 基础输入 Schema

所有 hooks 通过 `stdin` 接收以下公共字段：

```json
{
  "session_id": "string",
  "run_id": "string | undefined",
  "transcript_path": "string",
  "cwd": "string",
  "hook_event_name": "string",
  "timestamp": "string (ISO 8601)"
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 会话唯一标识 |
| `run_id` | string \| undefined | 当前代理运行标识（格式：`{sessionId}########{counter}`） |
| `transcript_path` | string | 会话 JSONL 转录文件路径 |
| `cwd` | string | 当前工作目录 |
| `hook_event_name` | string | 触发此 hook 的事件名 |
| `timestamp` | string | 事件触发时间（ISO 8601） |

---

## 公共输出字段

大多数 hooks 在 `stdout` JSON 中支持以下字段：

| 字段 | 类型 | 说明 |
|------|------|------|
| `systemMessage` | string | 向用户展示的通知信息 |
| `suppressOutput` | boolean | 为 `true` 时隐藏内部元数据 |
| `continue` | boolean | 为 `false` 时立即停止代理循环 |
| `stopReason` | string | 停止时展示给用户的原因 |
| `decision` | string | `"allow"` / `"deny"` / `"ask"` / `"approve"` |
| `reason` | string | 拒绝/阻止时的反馈消息 |
| `hookSpecificOutput` | object | 事件特定的输出字段 |

---

## 工具 Hooks

### `PreToolUse`

工具执行前触发。用于参数验证、安全检查和参数重写。

**输入字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `tool_use_id` | string | 工具调用唯一标识 |
| `tool_name` | string | 被调用的工具名 |
| `tool_input` | object | 模型生成的原始参数 |
| `mcp_context` | object | MCP 工具的可选元数据 |
| `original_request_name` | string | 尾调用时的原始名称 |

**输出字段**：

- `decision`：设为 `"deny"` 阻止工具执行
- `systemMessage`：展示为带 hook 名标签的独立通知框
- `reason`：拒绝时必需，作为工具错误发送给代理
- `hookSpecificOutput.tool_input`：**合并覆盖**模型参数
- `continue`：设为 `false` 终止整个代理循环

### `PostToolUse`

工具执行后触发。用于结果审计、上下文注入或隐藏敏感输出。

**输入字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `tool_use_id` | string | 工具调用唯一标识 |
| `tool_name` | string | 工具名 |
| `tool_input` | object | 原始参数 |
| `tool_response` | object | 执行结果 |
| `mcp_context` | object | MCP 元数据 |

**输出字段**：

- `decision`：设为 `"deny"` 隐藏真实输出
- `reason`：拒绝时**替换**发送给模型的工具结果
- `hookSpecificOutput.additionalContext`：追加到工具结果
- `hookSpecificOutput.tailToolCallRequest`：`{ name, args }` 立即执行另一工具
- `continue`：设为 `false` 终止代理循环

### `PostToolUseFailure`

工具执行失败后触发。用于错误恢复和沙箱绕过。

**输入字段**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `tool_use_id` | string | 工具调用唯一标识 |
| `tool_name` | string | 工具名 |
| `tool_input` | object | 原始参数 |
| `error` | string | 错误描述 |
| `error_type` | string | 错误类型（如 `"timeout"`, `"permission"`） |
| `is_interrupt` | boolean | 是否由用户中断导致 |

**输出字段**：

- `hookSpecificOutput.additionalContext`：帮助代理恢复的上下文
- `hookSpecificOutput.sandbox_bypass_request`：`{ original_command, reason }` 请求绕过沙箱

---

## 代理 Hooks

### `UserPromptSubmit`

用户提交提示后、代理开始规划前触发。

**输入字段**：

- `prompt`：用户提交的原始文本

**输出字段**：

- `hookSpecificOutput.additionalContext`：**追加**到本轮提示的文本
- `decision`：设为 `"deny"` 阻止本轮并丢弃消息
- `continue`：设为 `false` 阻止本轮但保留消息
- `reason`：拒绝或停止时必需

### `Stop`

代理即将停止时触发。用于响应验证和自动重试。

**输入字段**：

- `stop_hook_active`：是否已在重试序列中
- `last_assistant_message`：代理生成的最终文本

**输出字段**：

- `decision`：设为 `"deny"` 拒绝响应并强制重试
- `reason`：拒绝时作为新提示发送给代理
- `continue`：设为 `false` 停止会话
- `stopReason`：停止时展示给用户

---

## 模型 Hooks

### `BeforeModel`

发送 LLM 请求前触发。通过 Hook Translator 使用稳定的 SDK 无关格式。

**输入字段**：

- `llm_request`：包含 `model`、`messages`、`config` 和可选 `toolConfig`

**输出字段**：

- `hookSpecificOutput.llm_request`：**覆盖**请求的部分字段（如切换模型、调整温度）
- `hookSpecificOutput.llm_response`：**合成响应**，提供时跳过 LLM 调用
- `decision`：设为 `"deny"` 阻止本次模型请求

### `BeforeToolSelection`

LLM 决定调用哪些工具前触发。用于过滤可用工具集。

**输入字段**：

- `llm_request`：与 `BeforeModel` 相同格式

**输出字段**：

- `hookSpecificOutput.toolConfig.mode`：`"AUTO"` / `"ANY"` / `"NONE"`
  - `"NONE"`：禁用所有工具（优先级最高）
  - `"ANY"`：强制至少调用一个工具
- `hookSpecificOutput.toolConfig.allowedFunctionNames`：工具白名单

**合并策略**：多个 hook 的白名单取**并集**。

### `AfterModel`

收到 LLM 响应后触发。用于观察、日志或停止信号。

**输入字段**：

- `llm_request`：原始请求
- `llm_response`：模型响应

**输出字段**：

- `hookSpecificOutput.llm_response`：**替换存储的历史记录**
- `decision`：设为 `"deny"` 从历史中丢弃响应
- `continue`：设为 `false` 在当前轮后停止

---

## 生命周期与系统 Hooks

### `SessionStart`

应用启动、恢复会话或 `/clear` 命令后触发。

**输入字段**：`source`（`"startup"` / `"resume"` / `"clear"` / `"compact"`）

**输出字段**：

- `hookSpecificOutput.additionalContext`：注入为首轮内容
- `systemMessage`：会话开始时显示
- 仅通知性质：`continue` 和 `decision` 被**忽略**

### `SessionEnd`

CLI 退出或会话清除时触发。

**输入字段**：`reason`（`"clear"` / `"logout"` / `"prompt_input_exit"` / `"other"`）

**输出字段**：`systemMessage`（关闭时显示）

### `Notification`

CLI 发出系统提醒时触发（如工具权限提醒）。

**输入字段**：

- `notification_type`：通知类型
- `message`：提醒摘要
- `details`：提醒元数据

仅观察性质，无法阻止提醒。

### `PreCompact`

CLI 压缩历史以节省 token 前触发。

**输入字段**：`trigger`（`"auto"` / `"manual"`）

仅通知性质，无法阻止或修改压缩过程。

### `PermissionRequest`

权限对话框显示时触发。

**输入字段**：

- `permission_mode`：当前权限模式
- `tool_name`：工具名
- `tool_input`：工具参数
- `permission_suggestions`：建议列表

**输出字段**：

- `hookSpecificOutput.decision`：`{ behavior: "allow"|"deny", updatedInput?, message?, interrupt? }`

---

## 稳定模型 API

copilot-shell 使用 **Hook Translator** 层将 hook 脚本与底层 SDK 解耦。

### LLMRequest

```json
{
  "model": "string",
  "messages": [
    { "role": "user | model | system", "content": "string" }
  ],
  "config": {
    "temperature": 0.7,
    "maxOutputTokens": 8192,
    "topP": 0.95,
    "topK": 40
  },
  "toolConfig": {
    "mode": "AUTO | ANY | NONE",
    "allowedFunctionNames": ["read_file", "write_file"]
  }
}
```

### LLMResponse

```json
{
  "text": "string",
  "candidates": [
    {
      "content": { "role": "model", "parts": ["text"] },
      "finishReason": "STOP | MAX_TOKENS | SAFETY | OTHER",
      "index": 0
    }
  ],
  "usageMetadata": {
    "promptTokenCount": 100,
    "candidatesTokenCount": 200,
    "totalTokenCount": 300
  }
}
```

### 模式优先级（多 Hook 聚合）

- `NONE` 始终优先（最严格）
- `ANY` > `AUTO`
- `allowedFunctionNames` 取**并集**（排序以保证确定性）
