# Agent 工具

[English](../../../../en/user-entrypoint/cosh-ng/core/tools.md)

模型可以使用一组有边界的内置工具。审批模式、明确的允许名单和 Hooks 共同决定调用是否执行。

## 内置工具

| 类型 | 工具 | 常见用途 |
|---|---|---|
| ReadOnly | `read_file`、`read_many_files`、`grep`、`glob`、`list_directory` | 查看文件和路径 |
| FileEdit | `edit`、`write_file`、`save_memory` | 修改文件或保存记忆 |
| ShellExec | `shell` | 执行 Shell 命令 |
| Network | `web_fetch` | 获取 HTTP 资源 |
| Other | `skill`、`todo`、`ask_user_question` | 使用指令、跟踪工作、向用户提问 |

只有在启动 Core 时加入 `--enable-shell-evidence-tool` 才会提供 `cosh_shell_evidence`。已连接的 MCP 工具使用 `mcp__<server>__<tool>` 形式的名称；Extensions 也可以加入自己的外部工具名。

## 审批选择

| Core mode | ReadOnly | FileEdit | Shell、network、MCP、external |
|---|---|---|---|
| `trust` | 执行 | 执行 | 执行 |
| `auto` | 执行 | 执行 | 询问 |
| `recommend` | 执行 | 询问 | 询问 |

未知工具名会被拒绝。Core 和交互式 Shell 统一使用 `recommend`、`auto` 和
`trust`；旧的 `balanced`、`suggest` 和 `strict` 输入会归一为 `recommend`。

## 限制模型可见工具

使用 `--tools` 控制暴露范围；只有确实需要跳过审批的精确工具名才使用 `--allowed-tools`：

```bash
cosh-core --headless --tools read_file,grep,ask_user_question
cosh-core --headless --tools empty
cosh-core --headless --allowed-tools mcp__search__query
```

把 `shell`、网络工具或外部工具加入允许名单会授予真实权限，请按任务需要保持名单最小。

## 工作空间与输出边界

文件读取工具以 Core 启动时捕获的工作空间为根；之后在 Shell 中执行 `cd` 不会改变边界，越界路径会被拒绝。搜索结果有大小限制，不完整时会标记截断。进入 Agent 上下文的 MCP 输出上限为 64 KiB。

外部工具配置见 [MCP 设置](../mcp.md)，Extension 工具见 [Extensions](extensions.md)。
