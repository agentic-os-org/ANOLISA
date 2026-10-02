# Configuration System

Copilot Shell uses a layered configuration system that supports overrides
from system-level down to project-level.

## Configuration File Locations

| Level | Path | Purpose |
|-------|------|---------|
| System Settings | `/etc/copilot-shell/settings.json` | Admin global policies |
| System Defaults | `/etc/copilot-shell/system-defaults.json` | System default values |
| User Settings | `~/.copilot-shell/settings.json` | Personal preferences |
| Project Settings | `.copilot-shell/settings.json` | Project-level overrides |

## Priority (Highest to Lowest)

1. CLI arguments
2. Environment variables
3. System settings (admin-enforced overrides)
4. Project settings
5. User settings
6. System defaults

Higher-priority values override lower-priority ones. System settings serve as
an administrator policy layer with priority above user and project settings,
used to enforce organization-wide security policies.

## Configuration Categories

### general — General Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `general.language` | string | `"auto"` | UI language (`auto`/`en`/`zh-CN`) |
| `general.outputLanguage` | string | `"auto"` | LLM output language |
| `general.vimMode` | boolean | `false` | Enable Vim keybindings |
| `general.preferredEditor` | string | — | Preferred editor |
| `general.gitCoAuthor` | boolean | `true` | Auto-add Co-authored-by |
| `general.terminalBell` | boolean | `true` | Play terminal bell on completion |
| `general.chatRecording` | boolean | `true` | Save chat history to disk |
| `general.checkpointing.enabled` | boolean | `false` | Enable session checkpoints |

### ui — UI Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `ui.theme` | string | `"Copilot Shell Dark"` | Color theme |
| `ui.hideTips` | boolean | `false` | Hide tip messages |
| `ui.showLineNumbers` | boolean | `false` | Show line numbers in code |
| `ui.compactMode` | boolean | `false` | Compact mode (`Ctrl+O` toggle) |
| `ui.enableWelcomeBack` | boolean | `true` | Show "Welcome back" dialog |
| `ui.customThemes` | object | `{}` | Custom theme definitions |

### tools — Tool Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `tools.approvalMode` | enum | `"default"` | Approval mode (plan/default/auto-edit/yolo) |
| `tools.allowed` | array | — | Tools allowlisted for auto-execution |
| `tools.exclude` | array | — | Tools to exclude |
| `tools.shell.enableInteractiveShell` | boolean | `false` | Enable PTY interactive shell |
| `tools.useRipgrep` | boolean | `true` | Use ripgrep for search |

### security — Security Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `security.auth.selectedType` | string | — | Current authentication type |
| `security.auth.enforcedType` | string | — | Enforced authentication type |
| `security.auth.apiKey` | string | — | OpenAI-compatible API key |
| `security.auth.baseUrl` | string | — | OpenAI-compatible Base URL |
| `security.folderTrust.enabled` | boolean | `false` | Folder trust |

### model — Model Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `model.name` | string | — | Current model in use |
| `model.maxSessionTurns` | number | `-1` | Max session turns (-1 = unlimited) |
| `model.sessionTokenLimit` | number | — | Session token limit |
| `model.chatCompression` | object | — | Chat compression configuration |
| `model.generationConfig.timeout` | number | — | Request timeout (ms) |
| `model.generationConfig.maxRetries` | number | — | Max retry count |
| `model.generationConfig.contextWindowSize` | number | — | Override context window size |

### context — Context Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `context.fileName` | string | — | Context file name |
| `context.includeDirectories` | array | `[]` | Additional directories to include |
| `context.fileFiltering.respectGitIgnore` | boolean | `true` | Respect .gitignore |
| `context.fileFiltering.respectQwenIgnore` | boolean | `true` | Respect .copilotignore |

### mcp — MCP Servers

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mcpServers` | object | `{}` | MCP server configuration |
| `mcp.allowed` | array | — | Allowed MCP servers |
| `mcp.excluded` | array | — | Excluded MCP servers |

### hooksConfig — Hooks Configuration

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `hooksConfig.enabled` | boolean | `true` | Enable hooks system |
| `hooksConfig.disabled` | array | `[]` | List of disabled hook names |

### hooks — Hook Event Configuration

| Key | Type | Description |
|-----|------|-------------|
| `hooks.PreToolUse` | array | Hooks triggered before tool execution |
| `hooks.UserPromptSubmit` | array | Hooks triggered before agent processing |
| `hooks.Stop` | array | Hooks triggered after agent processing |

### skills — Skills Configuration

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `skills.customPaths` | array | `[]` | Custom skill search paths |
| `skillOS.baseUrl` | string | — | Remote Skill-OS address |
| `clawhub.registry` | string | — | Clawhub registry URL |

### webSearch — Web Search

| Key | Type | Description |
|-----|------|-------------|
| `webSearch.provider` | array | Search provider config (Tavily/Google/DashScope) |
| `webSearch.default` | string | Default search provider |

### autoMemory — Auto Memory

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `autoMemory.enabled` | boolean | `false` | Enable automatic memory extraction |
| `autoMemory.cooldownSeconds` | number | `1800` | Extraction cooldown (seconds) |

## Configuration Example

Full user configuration example (`~/.copilot-shell/settings.json`):

```json
{
  "general": {
    "language": "en",
    "outputLanguage": "English",
    "vimMode": false
  },
  "ui": {
    "theme": "Copilot Shell Dark",
    "compactMode": false
  },
  "tools": {
    "approvalMode": "default",
    "shell": {
      "enableInteractiveShell": true
    }
  },
  "security": {
    "auth": {
      "selectedType": "aliyun"
    }
  },
  "model": {
    "generationConfig": {
      "timeout": 60000,
      "maxRetries": 3
    }
  }
}
```

## Environment Variables

Some configuration supports environment variables:

| Variable | Corresponding Setting |
|----------|----------------------|
| `OPENAI_API_KEY` | `security.auth.apiKey` |
| `OPENAI_BASE_URL` | `security.auth.baseUrl` |

## Folder Trust

When Copilot Shell runs in a new project directory for the first time,
project-level configuration (`.copilot-shell/settings.json`) is not trusted
by default. Project settings only take effect after the user confirms trust
for that directory.

This behavior is controlled by `security.folderTrust.enabled`.
