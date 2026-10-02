# 接入 MCP 服务

[English](../../../en/user-entrypoint/cosh-ng/mcp.md)

MCP server 可以为 Agent 添加本地进程或远程 Streamable HTTP 服务提供的工具。完成一次配置后，从 `cosh` 连接并检查工具名称，再让 Agent 使用它们。

## 配置本地 stdio server

将 server 定义写入 `~/.copilot-shell/config.toml` 或 `/etc/copilot-shell/config.toml`。项目配置不能添加 MCP server。

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path/to/workspace"]
startup_timeout_ms = 30000
timeout_ms = 10000
allowed_tools = ["read_file", "list_directory"]
```

命令会直接启动，不经过交互式 Shell。需要环境变量时，在配置中明确传入：

```toml
[mcp.servers.filesystem.env]
SERVICE_TOKEN = "${FILESYSTEM_MCP_TOKEN}"
```

`allowed_tools` 可以列出已发现的工具名；省略表示暴露全部工具，设为 `[]` 表示不暴露工具。

## 配置远程 server

Streamable HTTP endpoint 使用 `url`，不使用 `command`：

```toml
[mcp.servers.search]
url = "https://mcp.example.com/mcp"
allowed_tools = ["query"]

[mcp.servers.search.oauth]
scopes = ["search"]
```

使用静态 token 时，删除 OAuth 表并设置：

```toml
bearer_token = "${SEARCH_MCP_TOKEN}"
```

远程 endpoint 使用 HTTPS。只有 `localhost`、`127.0.0.1` 或 `::1` 等 loopback 主机允许 HTTP。每个 server 必须且只能设置 `command` 或 `url` 之一。

## 连接并检查

在 server 应访问的工作空间启动 `cosh`，然后运行：

```text
/mcp list
/mcp connect filesystem
/mcp inspect filesystem
```

`list` 确认已读取定义；`connect` 启动或连接 server 并发现工具；`inspect` 显示发现的工具和 Agent 可见的名称，不会打印凭据。MCP 工具名称形如 `mcp__<server>__<tool>`，仍受审批规则约束。

OAuth 登录需要在 Shell 中运行（交互式 `/mcp login` 只会显示这条提示）：

```bash
cosh-core mcp login search
```

完成浏览器授权后，再回到 `cosh` 连接并检查 server。

## 刷新或断开

```text
/mcp refresh filesystem
/mcp disconnect filesystem
/mcp logout search
```

`refresh` 重新发现工具；`disconnect` 禁用启动时连接并删除保存的 OAuth 凭据，再次 `connect` 可重新启用；`logout` 只删除 OAuth 凭据，不修改定义。任务正在运行时，连接变化会在下一项 Agent 任务生效。

## 排查

| 现象 | 检查 |
|---|---|
| `/mcp list` 为空 | 使用系统或用户配置，不要使用项目配置 |
| 本地 server 无法启动 | 检查程序、参数、`env` 和 `startup_timeout_ms` |
| 已连接但没有工具 | 检查 `allowed_tools`（`[]` 不暴露任何工具） |
| 远程 endpoint 被拒绝 | 使用 HTTPS；HTTP 只允许 loopback；检查 token/OAuth 设置 |
| `cosh` 中 OAuth 无法启动 | 在 Shell 运行 `cosh-core mcp login <server>`，再连接 |

进入 Agent 上下文的 MCP 输出上限为 64 KiB。
