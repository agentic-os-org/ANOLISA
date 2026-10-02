# CLI Reference

Copilot Shell's command-line interface supports various flags and options to
control startup behavior, authentication, tool configuration, and output format.

## Basic Usage

```bash
cosh [options] [query]
```

Aliases: `co`, `copilot`

## Common Options

| Option | Short | Description |
|--------|-------|-------------|
| `--help` | `-h` | Show help information |
| `--version` | `-v` | Show version number |
| `--debug` | `-d` | Enable debug mode |
| `--model <name>` | `-m` | Specify the model to use |
| `--prompt <text>` | `-p` | Non-interactive mode: execute prompt then exit |
| `--prompt-interactive <text>` | `-i` | Execute prompt then stay interactive |
| `--yolo` | `-y` | Auto-approve all operations (YOLO mode) |

## Session Options

| Option | Description |
|--------|-------------|
| `--continue` | Resume the most recent session |
| `--resume <id>` | Resume a session by ID |
| `--max-session-turns <n>` | Limit maximum session turns |

## Approval Options

| Option | Description |
|--------|-------------|
| `--approval-mode <mode>` | Set approval mode (plan/default/auto-edit/yolo) |
| `--checkpointing` | Enable file edit checkpoints (allows rollback) |

## Authentication Options

| Option | Description |
|--------|-------------|
| `--auth-type <type>` | Specify authentication type |
| `--openai-api-key <key>` | OpenAI-compatible API key |
| `--openai-base-url <url>` | OpenAI-compatible Base URL |

## Tool Options

| Option | Description |
|--------|-------------|
| `--allowed-tools <list>` | Allowed tools (comma-separated) |
| `--exclude-tools <list>` | Excluded tools (comma-separated) |
| `--core-tools <path>` | Core tool definition file path |
| `--allowed-mcp-server-names <list>` | Allowed MCP servers (comma-separated) |

## Extension Options

| Option | Description |
|--------|-------------|
| `--extensions <list>` | Extensions to load (comma-separated) |
| `--list-extensions` | List all loaded extensions then exit |

## Input/Output Options

| Option | Short | Description |
|--------|-------|-------------|
| `--input-format <fmt>` | `-I` | Input format (text/stream-json) |
| `--output-format <fmt>` | `-O` | Output format (text/json/stream-json) |
| `--include-partial-messages` | — | Include partial messages (stream-json only) |

## Advanced Options

| Option | Description |
|--------|-------------|
| `--all-files` / `-a` | Include all files in context |
| `--acp` | ACP mode (Zed integration) |
| `--proxy <url>` | Network proxy (format: schema://user:password@host:port) |
| `--screen-reader` | Screen reader accessibility mode |
| `--skip-startup-context` | Skip workspace startup context |
| `--skip-loop-detection` | Skip loop detection |

## Usage Examples

### Non-interactive Execution

```bash
# Execute a task then exit
cosh -p "List all TODO comments"

# Specify a model
cosh -m qwen3.7-max -p "Explain what this code does"
```

### Resume Session

```bash
# Resume the most recent session
cosh --continue

# Resume a specific session
cosh --resume abc123
```

### YOLO Mode

```bash
# Auto-approve all operations
cosh -y -p "Fix all lint errors"
```

### JSON Output

```bash
# Output results in JSON format
cosh -O json -p "Analyze project dependencies"
```

### Proxy Settings

```bash
cosh --proxy http://proxy.example.com:8080
```
