# Tools & Approval Modes

Copilot Shell executes file operations, shell commands, search, and other tasks
through built-in tools. This document covers the tool system and approval modes.

## Built-in Tools

Copilot Shell provides the following core tools:

| Tool | Function |
|------|----------|
| File Read/Write | Read, create, and edit files |
| Shell Execution | Run shell commands |
| Code Search | Full-text search powered by ripgrep |
| File Search | Find files by name pattern |
| Directory Listing | List directory contents |
| Web Search | Search the internet for information |
| MCP Tools | Call external tools via MCP protocol |

Use the `/tools` command to see all available tools in the current session.

## Approval Modes

Approval modes determine whether Copilot Shell requires user confirmation
before executing tools. Set via the `/approval-mode` command or the
`tools.approvalMode` configuration key.

### plan

Only generates an action plan without executing any tool calls. Useful for
reviewing AI decision-making.

### default (Default)

Requests confirmation before each tool call. The user can:

- Press `y` or Enter to confirm
- Press `n` to reject
- Press `a` to accept all for the current session

### auto-edit

Auto-approves file edit operations; other operations (such as shell commands)
still require confirmation. Suitable when you trust the AI's code modifications
but want to control external command execution.

### yolo

Auto-approves all tool calls without any confirmation.

> [!WARNING]
>
> `yolo` mode skips all safety confirmations. Recommended only in controlled
> environments or when combined with sandbox hooks. Can also be specified at
> launch with `cosh --yolo` or `cosh -y`.

## Tool Filtering

### Allowlist

Set specific tools for auto-execution without confirmation:

```json
{
  "tools": {
    "allowed": ["ReadFile", "ListDir", "GrepSearch"]
  }
}
```

Tools on the allowlist execute automatically even in `default` mode.

### Excluding Tools

Prevent specific tools from being called:

```json
{
  "tools": {
    "exclude": ["WebSearch"]
  }
}
```

Excluded tools are invisible to the AI and will not appear in the tool list.

## Shell Tool Configuration

### Interactive Shell (PTY)

When enabled, shell commands execute through a pseudo-terminal, supporting
`sudo`, interactive programs, etc.:

```json
{
  "tools": {
    "shell": {
      "enableInteractiveShell": true
    }
  }
}
```

### Output Display

```json
{
  "tools": {
    "shell": {
      "showColor": true,
      "pager": "cat"
    }
  }
}
```

## Tool Output Truncation

When tool output is too large, Copilot Shell automatically truncates it to
save tokens:

```json
{
  "tools": {
    "enableToolOutputTruncation": true,
    "truncateToolOutputThreshold": 50000,
    "truncateToolOutputLines": 200
  }
}
```

- `truncateToolOutputThreshold`: Trigger truncation above this character count (-1 to disable)
- `truncateToolOutputLines`: Number of lines to keep after truncation

## Search Tool

Copilot Shell uses the built-in ripgrep for code search by default:

```json
{
  "tools": {
    "useRipgrep": true,
    "useBuiltinRipgrep": true
  }
}
```

- `useRipgrep`: Enable ripgrep (faster than the default implementation)
- `useBuiltinRipgrep`: Use the bundled `rg` binary. Set to `false` to use the system `rg`
