# MCP Servers

MCP (Model Context Protocol) is a standard protocol that allows Copilot Shell
to communicate with external tool servers. By configuring MCP servers, you can
extend the set of tools available to the AI.

## View MCP Servers

```
/mcp
```

Lists configured MCP servers and their status.

## Configuring MCP Servers

Configure in the `mcpServers` field of `settings.json`:

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

### Configuration Fields

| Field | Type | Description |
|-------|------|-------------|
| `command` | string | Command to start the MCP server |
| `args` | array | Command arguments |
| `env` | object | Environment variables passed to the server |
| `url` | string | URL of a remote MCP server (mutually exclusive with command) |

### stdio Mode

Local MCP servers communicating via stdin/stdout:

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

### SSE Mode

Remote MCP servers communicating via HTTP Server-Sent Events:

```json
{
  "mcpServers": {
    "remote-tools": {
      "url": "https://mcp.example.com/sse"
    }
  }
}
```

## Filtering MCP Servers

### Allow List

Enable only specified MCP servers:

```json
{
  "mcp": {
    "allowed": ["filesystem", "my-server"]
  }
}
```

### Exclude List

Disable specific MCP servers:

```json
{
  "mcp": {
    "excluded": ["risky-server"]
  }
}
```

## MCP Server Command

Launch MCP servers via a custom command:

```json
{
  "mcp": {
    "serverCommand": "/usr/local/bin/my-mcp-launcher"
  }
}
```

## OAuth Authentication

Some MCP servers require OAuth authentication. Copilot Shell has built-in
OAuth 2.0 + PKCE support and automatically guides through the authentication
flow on first connection.

## CLI Arguments

Specify allowed MCP servers via the command line:

```bash
cosh --allowed-mcp-server-names filesystem,my-server
```

## Configuration Layers

MCP server configuration supports multi-layer overrides:

- **System-level**: Admin pre-installed MCP servers
- **User-level**: Personal frequently-used MCP servers
- **Project-level**: Project-specific MCP servers

Multi-layer configuration uses shallow merge strategy (same-name keys use
the higher-priority value).
