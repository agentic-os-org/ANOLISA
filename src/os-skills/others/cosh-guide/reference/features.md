# Features Reference

## Approval Mode

Four permission modes controlling how AI interacts with your code:

| Mode | File Editing | Shell Commands | Best For | Risk |
|---|---|---|---|---|
| **Plan** | Read-only | Not executed | Code exploration, planning | Lowest |
| **Default** | Manual approval | Manual approval | New codebases, critical systems | Low |
| **Auto-Edit** | Auto-approved | Manual approval | Daily development, refactoring | Medium |
| **YOLO** | Auto-approved | Auto-approved | Trusted projects, CI/CD | Highest |

### Switching Modes

- **During session**: `Shift+Tab` to cycle
- **Command**: `/approval-mode <plan|default|auto-edit|yolo>`
- **Config**: `"tools": { "approvalMode": "auto-edit" }` in `settings.json`

### Plan Mode for Complex Refactors

```bash
/approval-mode plan
```
```
I need to refactor our auth system to use OAuth2. Create a detailed migration plan.
```

---

## Headless Mode

Run Copilot Shell programmatically without interactive UI.

### Basic Usage

```bash
# Direct prompt
co --prompt "What is machine learning?"
co -p "query"

# Stdin input
echo "Explain this code" | co

# File input
cat README.md | co -p "Summarize this documentation"
```

### Output Formats

| Format | Flag | Description |
|---|---|---|
| Text (default) | `--output-format text` | Human-readable |
| JSON | `--output-format json` | Structured, buffered |
| Stream-JSON | `--output-format stream-json` | Real-time streaming |

### Resume Sessions

```bash
co --continue -p "Run the tests again"
co --resume <session-id> -p "Apply the refactor"
```

### Key Options

| Option | Description |
|---|---|
| `--prompt`, `-p` | Run headless |
| `--output-format`, `-o` | Output format (text/json/stream-json) |
| `--yolo`, `-y` | Auto-approve all |
| `--approval-mode` | Set approval mode |
| `--all-files`, `-a` | Include all files in context |
| `--include-directories` | Include extra directories |
| `--continue` | Resume most recent session |
| `--resume [id]` | Resume specific session |

---

## MCP (Model Context Protocol)

Connect Copilot Shell to external tools and data sources.

### Quick Start

```bash
# Add HTTP server
co mcp add --transport http my-server http://localhost:3000/mcp

# Add stdio server
co mcp add pythonTools python -m my_mcp_server

# List servers
co mcp list

# Remove server
co mcp remove my-server
```

### Transport Types

| Transport | When to Use | Config Field |
|---|---|---|
| `http` | Remote services (recommended) | `httpUrl` |
| `sse` | Legacy SSE servers | `url` |
| `stdio` | Local processes | `command`, `args` |

### Configuration via settings.json

```json
{
  "mcpServers": {
    "myServer": {
      "command": "python",
      "args": ["-m", "my_mcp_server"],
      "env": { "API_KEY": "$MY_API_KEY" },
      "timeout": 15000
    }
  }
}
```

### Scopes

- **Project** (default): `.copilot-shell/settings.json`
- **User**: `~/.copilot-shell/settings.json` (use `--scope user`)

### Safety

- `trust: true` bypasses confirmations (use sparingly)
- `includeTools` / `excludeTools` to filter tools per server
- Global allow/deny: `mcp.allowed` / `mcp.excluded` in settings

---

## Agent Skills
Modular capabilities extending Cosh.

### Create a Skill

Skills are directories containing `SKILL.md`:

- **Personal**: `~/.copilot-shell/skills/<skill-name>/SKILL.md`
- **Project**: `.copilot-shell/skills/<skill-name>/SKILL.md`

```yaml
---
name: your-skill-name
description: Brief description of what this Skill does and when to use it
---

# Your Skill Name

## Instructions
Step-by-step guidance.

## Examples
Concrete examples.
```

### Invoke

Skills are auto-invoked by the model based on your request. Explicit invocation:

```bash
/skills <skill-name>
```

### Supporting Files

```
my-skill/
  SKILL.md (required)
  reference.md (optional)
  scripts/helper.py (optional)
  templates/template.txt (optional)
```

---

## Subagents

Specialized AI assistants for specific task types.

### Management

```
/agents create    # Create new subagent
/agents manage    # View/manage existing
```

### Storage

- **Project**: `.copilot-shell/agents/` (highest precedence)
- **User**: `~/.copilot-shell/agents/`
- **Extension**: Provided by installed extensions

### Configuration Format

```markdown
---
name: agent-name
description: Brief description
tools:
  - read_file
  - write_file
  - run_shell_command
---

System prompt content goes here.
Use ${variable} templating for dynamic content.
```

### Invocation

- **Automatic**: AI delegates based on task matching
- **Explicit**: "Let the testing-expert subagent create unit tests for the payment module"

### Best Practices

- Single responsibility per agent
- Specific expertise areas
- Clear, actionable descriptions
- Limit tool access to what's needed

---

## Checkpointing

Auto-saves project state before AI file modifications.

### Enable

```bash
co --checkpointing
```

Or in `settings.json`:
```json
{ "general": { "checkpointing": { "enabled": true } } }
```

### How It Works

Before file modifications, creates:
1. Git snapshot in shadow repository (`~/.copilot-shell/history/<project_hash>`)
2. Conversation history backup
3. Tool call record

### Restore

```
/restore           # List available checkpoints
/restore <file>    # Restore specific checkpoint
```

---

## LSP (Language Server Protocol) - Experimental

Code intelligence features via language servers.

### Enable

```bash
co --experimental-lsp
```

### Supported Languages

| Language | Server | Install |
|---|---|---|
| TypeScript/JS | typescript-language-server | `npm install -g typescript-language-server typescript` |
| Python | pylsp | `pip install python-lsp-server` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` |
| Rust | rust-analyzer | See official docs |

### Configuration (.lsp.json)

```json
{
  "typescript": {
    "command": "typescript-language-server",
    "args": ["--stdio"],
    "extensionToLanguage": {
      ".ts": "typescript", ".tsx": "typescriptreact",
      ".js": "javascript", ".jsx": "javascriptreact"
    }
  }
}
```

### Available Operations

- `goToDefinition`, `findReferences`, `goToImplementation`
- `hover`, `documentSymbol`, `workspaceSymbol`
- `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`
- `diagnostics`, `workspaceDiagnostics`
- `codeActions` (quickfix, refactor, source.organizeImports, etc.)

---

## Token Caching

Automatic API cost optimization for API key users.

- Caches system instructions and conversation history
- Reduces tokens processed in subsequent requests
- Available for API key users (not OAuth)
- Check savings with `/stats` command

---

## Language / i18n

### UI Language

```bash
/language ui zh-CN    # Chinese
/language ui en-US    # English
```

### LLM Output Language

```bash
/language output Chinese
/language output English
```

Stored at `~/.copilot-shell/output-language.md`. Restart required after change.

### Custom Language Packs

Create files in `~/.copilot-shell/locales/`:

```javascript
// ~/.coplit-shell/locales/es.js
export default {
  Hello: 'Hola',
  Settings: 'Configuracion',
};
```
