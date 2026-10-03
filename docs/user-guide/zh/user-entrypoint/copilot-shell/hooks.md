# 使用 Hooks

Hooks 是 Copilot Shell 的拦截机制，可以在工具调用前后、代理处理前后
执行自定义脚本，实现安全防护、自动化审批、上下文注入等功能。

## 快速开始

列出所有已注册的 Hooks：

```
/hooks list
```

## Hook 事件

Copilot Shell 支持以下 hook 事件：

| 事件 | 触发时机 | 典型用途 |
|------|----------|----------|
| `SessionStart` | 会话开始（启动/恢复/清除） | 初始化环境、加载上下文 |
| `SessionEnd` | 会话结束（退出/清除） | 清理资源、保存状态 |
| `UserPromptSubmit` | 用户提交提示后、规划前 | 注入上下文、验证输入、阻止本轮 |
| `Stop` | 代理即将停止时 | 审查输出、强制重试 |
| `BeforeModel` | 发送 LLM 请求前 | 切换模型、修改参数、模拟响应 |
| `AfterModel` | 收到 LLM 响应后 | 过滤响应、记录日志 |
| `BeforeToolSelection` | LLM 选择工具前 | 过滤可用工具集 |
| `PreToolUse` | 工具执行前 | 拦截危险命令、修改参数、安全审查 |
| `PostToolUse` | 工具执行后 | 处理结果、日志记录、隐藏敏感输出 |
| `PostToolUseFailure` | 工具执行失败后 | 错误恢复、沙箱绕过 |
| `PreCompact` | 上下文压缩前 | 保存状态、通知用户 |
| `Notification` | 系统通知发生时 | 转发桌面提醒 |
| `PermissionRequest` | 权限对话框显示时 | 自动批准或拒绝权限 |

## 管理 Hooks

### 查看已注册 hooks

```
/hooks list
```

显示所有 hooks 的名称、来源、状态（启用/禁用）。

### 启用或禁用特定 hook

通过配置文件禁用：

```json
{
  "hooksConfig": {
    "disabled": ["sandbox-guard"]
  }
}
```

### 全局关闭 hooks 系统

```json
{
  "hooksConfig": {
    "enabled": false
  }
}
```

## Hook 配置格式

在 `settings.json` 中配置自定义 hook：

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "run_shell_command",
        "sequential": true,
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/my-hook.sh",
            "name": "my-hook",
            "timeout": 10000
          }
        ]
      }
    ]
  }
}
```

### 配置字段

每个事件下是一个匹配组数组，每个匹配组包含：

| 字段 | 类型 | 说明 |
|------|------|------|
| `matcher` | string | 匹配的工具名（正则），空或 `"*"` 为匹配所有 |
| `sequential` | boolean | 是否串行执行（默认并行） |
| `hooks` | array | 该匹配组下的 hook 列表 |

每个 hook 对象的字段：

| 字段 | 类型 | 说明 |
|------|------|------|
| `type` | string | 执行引擎，目前仅支持 `"command"` |
| `command` | string | Hook 脚本路径或命令 |
| `name` | string | Hook 名称（用于日志和管理命令） |
| `timeout` | number | 超时时间（毫秒），默认 60000 |

## Hook 的来源优先级

当多个来源定义了同一事件的 hooks 时，执行优先级为：

1. **User** — 用户设置中定义的 hooks
2. **Extension** — 扩展注入的 hooks
3. **Remote** — 远程加载的 hooks

## Hook 输入输出

Hook 脚本通过 stdin 接收 JSON 输入，通过 stdout 返回 JSON 输出。

### PreToolUse 输入示例

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "run_shell_command",
  "tool_input": {
    "command": "rm -rf /tmp/test"
  },
  "session_id": "abc123",
  "cwd": "/home/user/project",
  "timestamp": "2025-01-01T00:00:00Z"
}
```

### PreToolUse 输出示例

阻止执行：

```json
{
  "decision": "deny",
  "reason": "危险命令已被拦截"
}
```

允许执行并修改参数：

```json
{
  "hookSpecificOutput": {
    "tool_input": {
      "command": "linux-sandbox -- rm -rf /tmp/test"
    }
  }
}
```

## 相关文档

- [Hook 开发指南](../../../../developer-guide/zh/copilot-shell/hooks/index.md)
- [Hook API 参考](../../../../developer-guide/zh/copilot-shell/hooks/reference.md)
- [编写自定义 Hook](../../../../developer-guide/zh/copilot-shell/hooks/writing-hooks.md)
- [AgentSecCore 内部命令](../../agent-security/agent-sec-core/internal-commands.md) — `sandbox-guard` spawn 的 `agent-sec-cli log-sandbox` 审计命令契约
