# Command Reference

Copilot Shell provides a rich set of slash commands to control session behavior.
Type `/help` in a session to see all available commands.

## Basic Commands

| Command | Description |
|---------|-------------|
| `/help` | Show help information |
| `/clear` | Clear screen (shortcut `Ctrl+L`) |
| `/quit` | Exit Copilot Shell (alias `/exit`) |
| `/about` | Show version and system information |

## Authentication & Model

| Command | Description |
|---------|-------------|
| `/auth` | Switch authentication method |
| `/model` | Switch the current model |

## Language & Appearance

| Command | Description |
|---------|-------------|
| `/language` | View language settings |
| `/language ui <lang>` | Set UI language (e.g., `zh-CN`, `en`) |
| `/language output <lang>` | Set LLM output language |
| `/theme` | Switch color theme |
| `/statusline` | Configure status bar display |

## Session Management

| Command | Description |
|---------|-------------|
| `/resume` | Resume a previous session |
| `/rename` | Rename the current session |
| `/export` | Export the current session |
| `/restore` | Restore a session from backup |
| `/compress` | Replace chat history with a summary to save tokens |
| `/summary` | Generate a summary of the current session |
| `/copy` | Copy the most recent reply to clipboard |
| `/stats` | Show token usage statistics for the current session |

## Interactive Tools

| Command | Description |
|---------|-------------|
| `/bash` | Enter interactive shell; type `exit` to return |
| `/editor` | Open editor to compose content |
| `/vim` | Toggle Vim mode |
| `/directory` | Browse directory structure |

## Tools & Permissions

| Command | Description |
|---------|-------------|
| `/tools` | List all available tools |
| `/approval-mode` | Set tool approval mode |
| `/permissions` | Manage tool permissions |

Approval mode options:

- `plan`: Plan only, no execution
- `default`: Confirm before each execution
- `auto-edit`: Auto-approve file edits; others require confirmation
- `yolo`: Auto-approve all operations (use with caution)

## Hooks Management

| Command | Description |
|---------|-------------|
| `/hooks` | Show hooks help |
| `/hooks list` | List all registered hooks and their status |

## Extension Management

| Command | Description |
|---------|-------------|
| `/extensions` | View loaded extensions |

## Skills Management

| Command | Description |
|---------|-------------|
| `/skills` | List available skills |
| `/clawhub` | Manage Clawhub remote skills |
| `/agents` | Manage subagents |

## MCP Servers

| Command | Description |
|---------|-------------|
| `/mcp` | View and manage MCP servers |

## IDE Integration

| Command | Description |
|---------|-------------|
| `/ide` | IDE integration management |

## Settings

| Command | Description |
|---------|-------------|
| `/settings` | Open settings management interface |

## Git & Project

| Command | Description |
|---------|-------------|
| `/init` | Initialize project configuration |
| `/setup-github` | Configure GitHub integration |
| `/bug` | Submit a bug report |

## Other

| Command | Description |
|---------|-------------|
| `/memory` | Manage session memory |
| `/terminal-setup` | Terminal setup recommendations |

## Keyboard Shortcuts

| Shortcut | Function |
|----------|----------|
| `Ctrl+L` | Clear screen |
| `Ctrl+C` | Cancel current operation |
| `Ctrl+O` | Toggle compact mode (hide tool output) |
| `?` | View all shortcuts |
| `Tab` | Command completion |
| `↑` / `↓` | Browse command history |
| `/` | Trigger command list |
