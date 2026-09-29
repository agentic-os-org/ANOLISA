# Hooks

[English](../../../../en/user-entrypoint/cosh-ng/core/hooks.md)

Hooks 在 Agent 事件前后运行命令，可用于策略检查、通知或补充上下文。只应从可信来源启用 Hooks。

## 启用和管理 Hooks

对于 `cosh-core`，声明 Hook 即默认启用，`hooks.enabled = true` 可省略。
显式设置 `hooks.enabled = false` 会禁用配置 Hook；存在 Hook 定义时会输出警告。
系统或用户配置中的禁用还会阻止 Extension Hook 自动启用。项目配置中的禁用仅影响
配置 Hook，已安装的 Extension 不会重新启用这些已禁用的配置 Hook。
`--bare` 仍会禁用所有 Hook。

在 `~/.copilot-shell/config.toml` 或受信任的项目配置中定义：

```toml
[[hooks.PreToolUse]]
name = "security-check"
command = "/usr/local/bin/my-security-hook"
matcher = "shell"
timeout = 60000
```

在交互式终端中运行：

```text
/hooks
/hooks history
/hooks trust-project
/hooks enable <id>
/hooks disable <id>
```

项目根目录受信任前，项目 Hooks 不会执行。使用 `/hooks untrust-project` 可以撤销信任。Shell Hook 状态只在当前会话有效；Agent Hook 状态由 registry 持久化。

## 事件名称

| 事件 | 运行时机 | 能否拦截 |
|---|---|---|
| `PreToolUse` | 工具调用前 | 可以 |
| `PostToolUse` | 工具调用成功后 | 可以 |
| `PostToolUseFailure` | 工具调用失败后 | 不可以 |
| `UserPromptSubmit` | 提交 prompt 时 | 可以 |
| `SessionStart` | 会话初始化后 | 不可以 |
| `Stop` | Agent 停止时 | 可以 |
| `BeforeModel` / `AfterModel` | 模型请求前后 | 不可以 |

使用 `matcher` 限制工具事件。Hook 命令从 stdin 接收一个 JSON 对象，其中包含 `hook_event_name`、`session_id`、`cwd`，以及 `tool_name`、`tool_input` 等事件数据。

## 返回决策

向 stdout 写入一个 JSON 对象：

```json
{
  "decision": "block",
  "reason": "Dangerous command",
  "systemMessage": "Command blocked by security policy"
}
```

`allow` 表示继续，`block`/`deny` 表示停止操作，`ask` 请求用户确认。退出码 `2` 也会拦截。
在 `cosh-core` 中，配置 `PreToolUse` Hook 遇到空输出、非法 JSON、超时或非预期的非零
退出码时默认阻断，除非该 Hook 显式设置 `fail_open = true`。Extension Hook 成功时
允许空输出，但执行错误仍默认阻断。配置 Hook 可返回 `{}` 显式透传。
默认超时为 60 秒；需要快速完成的检查可以设置更短的 `timeout`。
同一事件的多个 Hook 需要按顺序运行时，设置 `sequential = true`。

## 添加上下文或子进程变量

`hookSpecificOutput.additional_context` 会把文本加入 Agent 上下文。`env` 只注入 Hook 子进程：

```toml
[[hooks.SessionStart]]
name = "load-context"
command = "/usr/local/bin/load-context"
env = { TEAM = "platform" }
```

宿主进程不会被修改。变量名必须符合 `[A-Za-z_][A-Za-z0-9_]*`，变量值不会被打印。Extension Hook 使用相同配置和协议，参见 [Extensions](extensions.md)。
