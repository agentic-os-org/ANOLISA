# 接入 MCP 服务

[English](../../en/cosh-ng/mcp.md)

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

## 信任 stdio server

本地 stdio server 是在会话启动时运行的代码，因此 cosh 只有在你显式信任后才启动它。新配置的 stdio server 会标记为待信任，在信任之前不会自动启动。远程 HTTP server 不运行本地代码，不受此门控。

```text
/mcp show filesystem
/mcp trust filesystem
```

`show` 不启动 server，只打印命令、参数、环境变量名和信任指纹。`trust` 记录该指纹并允许启动；在 TUI 中会先预览配置，并把信任绑定到你看到的指纹。在终端里 `cosh-core mcp trust filesystem` 会要求 `y/N` 确认；管道或非交互调用除非传 `--yes` 否则被拒绝。

指纹覆盖解析后的命令、参数、显式环境变量值和 `allowed_tools`。改动其中任何一项——包括从不同工作空间启动会话导致相对命令解析到别处——都会使指纹失效，server 停止自动启动，直到你重新信任。编辑定义后请重新 `show` 和 `trust`。

运行中的会话使用启动时加载的配置。信任或撤销会立即写盘并应用于后续启动；当前会话在下次快照重建时生效。若修改了配置或重建失败，请重启 `cosh` 使变更在当前会话生效。

```text
/mcp untrust filesystem
```

`untrust` 按名撤销启动信任，即使 server 已不在配置中也可撤销，因此被删除的 server 无法复活旧信任。撤销在下次快照重建时生效。

## 连接并检查

在 server 应访问的工作空间启动 `cosh`，然后运行：

```text
/mcp list
/mcp connect filesystem
/mcp inspect filesystem
```

`list` 确认已读取定义；`connect` 启动或连接 server 并发现工具，对 stdio server 要求已有信任指纹，未信任的 server 会被拒绝，直到你先 `trust`；`inspect` 和 `refresh` 同样拒绝未信任的 stdio server。`inspect` 显示发现的工具和 Agent 可见的名称，不会打印凭据。MCP 工具名称形如 `mcp__<server>__<tool>`，仍受审批规则约束。

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

`refresh` 重新发现工具（对 stdio server 要求已有信任指纹）；`disconnect` 禁用启动时连接并删除保存的 OAuth 凭据，再次 `connect` 可重新启用；`logout` 只删除 OAuth 凭据，不修改定义。任务正在运行时，连接变化会在下一项 Agent 任务生效。

## 排查

| 现象 | 检查 |
|---|---|
| `/mcp list` 为空 | 使用系统或用户配置，不要使用项目配置 |
| stdio server 不自动启动 | 先信任：`/mcp show <server>` 再 `/mcp trust <server>` |
| 本地 server 无法启动 | 检查程序、参数、`env` 和 `startup_timeout_ms` |
| 已连接但没有工具 | 检查 `allowed_tools`（`[]` 不暴露任何工具） |
| 远程 endpoint 被拒绝 | 使用 HTTPS；HTTP 只允许 loopback；检查 token/OAuth 设置 |
| `cosh` 中 OAuth 无法启动 | 在 Shell 运行 `cosh-core mcp login <server>`，再连接 |

进入 Agent 上下文的 MCP 输出上限为 64 KiB。
