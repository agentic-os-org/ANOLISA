# MCP 服务器

MCP（Model Context Protocol）是一种标准协议，允许 Copilot Shell 与外部
工具服务器通信。通过配置 MCP 服务器，你可以扩展 AI 可用的工具集。

## 查看 MCP 服务器

```
/mcp
```

列出已配置的 MCP 服务器及其状态。

## 配置 MCP 服务器

在 `settings.json` 中的 `mcpServers` 字段配置：

```json
{
  "mcpServers": {
    "my-server": {
      "command": "npx",
      "args": ["-y", "@my-org/mcp-server"],
      "env": {
        "API_KEY": "xxx"
      }
    }
  }
}
```

### 配置字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `command` | string | 启动 MCP 服务器的命令 |
| `args` | array | 命令参数 |
| `env` | object | 传递给服务器的环境变量 |
| `url` | string | 远程 MCP 服务器的 URL（与 command 二选一） |

### stdio 模式

通过 stdin/stdout 通信的本地 MCP 服务器：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    }
  }
}
```

### SSE 模式

通过 HTTP Server-Sent Events 通信的远程 MCP 服务器：

```json
{
  "mcpServers": {
    "remote-tools": {
      "url": "https://mcp.example.com/sse"
    }
  }
}
```

## 过滤 MCP 服务器

### 允许列表

只启用指定的 MCP 服务器：

```json
{
  "mcp": {
    "allowed": ["filesystem", "my-server"]
  }
}
```

### 排除列表

禁用特定的 MCP 服务器：

```json
{
  "mcp": {
    "excluded": ["risky-server"]
  }
}
```

## MCP 服务器命令

通过自定义命令启动 MCP 服务器：

```json
{
  "mcp": {
    "serverCommand": "/usr/local/bin/my-mcp-launcher"
  }
}
```

## OAuth 认证

部分 MCP 服务器需要 OAuth 认证。Copilot Shell 内置了 OAuth 2.0 + PKCE
支持，首次连接时会自动引导认证流程。

## CLI 参数

通过命令行指定允许的 MCP 服务器：

```bash
cosh --allowed-mcp-server-names filesystem,my-server
```

## 配置层级

MCP 服务器配置支持多层覆盖：

- **系统级**：管理员预装的 MCP 服务器
- **用户级**：个人常用的 MCP 服务器
- **项目级**：项目特定的 MCP 服务器

多层配置采用浅合并策略（同名 key 以高优先级为准）。
