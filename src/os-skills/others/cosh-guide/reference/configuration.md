# Configuration Reference

## Configuration Layers (Precedence)

| Level | Source | Description |
|---|---|---|
| 1 | Default values | Hardcoded defaults |
| 2 | User settings | `~/.copilot-shell/settings.json` |
| 3 | Project settings | `.copilot-shell/settings.json` |

---

## Authentication
### Switch Method

```
/auth
```

---

## Settings (settings.json)

### Key Settings

#### general

| Setting | Type | Default | Description |
|---|---|---|---|
| `general.preferredEditor` | string | undefined | Preferred editor |
| `general.vimMode` | boolean | false | Enable Vim keybindings |
| `general.disableAutoUpdate` | boolean | false | Disable auto-updates |
| `general.gitCoAuthor` | boolean | true | Add Co-authored-by to commits |
| `general.checkpointing.enabled` | boolean | false | Enable checkpointing |

#### model

| Setting | Type | Default | Description |
|---|---|---|---|
| `model.name` | string | undefined | Model to use |
| `model.maxSessionTurns` | number | -1 | Max turns (-1 = unlimited) |
| `model.chatCompression.contextPercentageThreshold` | number | 0.7 | Compression threshold (0-1) |
| `model.generationConfig` | object | undefined | Advanced overrides (timeout, maxRetries, samplingParams, etc.) |

#### ui

| Setting | Type | Default | Description |
|---|---|---|---|
| `ui.theme` | string | undefined | Color theme |
| `ui.hideTips` | boolean | false | Hide tips |
| `ui.showLineNumbers` | boolean | true | Line numbers in code blocks |

#### tools

| Setting | Type | Default | Description |
|---|---|---|---|
| `tools.approvalMode` | string | "default" | Default approval mode |
| `tools.core` | array | undefined | Allowlist of built-in tools |
| `tools.exclude` | array | undefined | Tools to exclude |
| `tools.allowed` | array | undefined | Tools bypassing confirmation |

#### context

| Setting | Type | Default | Description |
|---|---|---|---|
| `context.fileName` | string/array | undefined | Context file name(s) |
| `context.includeDirectories` | array | [] | Extra directories |
| `context.fileFiltering.respectGitIgnore` | boolean | true | Respect .gitignore |

#### privacy

| Setting | Type | Default | Description |
|---|---|---|---|
| `privacy.usageStatisticsEnabled` | boolean | true | Usage statistics |

### Example settings.json

```json
{
  "general": { "vimMode": true, "preferredEditor": "code" },
  "ui": { "theme": "GitHub" },
  "tools": { "approvalMode": "auto-edit" },
  "model": { "name": "qwen3-coder-plus" },
  "context": {
    "fileName": ["COPILOT.md"],
    "includeDirectories": ["path/to/dir1"]
  }
}
```

---

## Themes

### Built-in Themes

- **Dark**: ANSI, Atom One, Ayu, Default, Dracula, GitHub
- **Light**: ANSI Light, Ayu Light, Default Light, GitHub Light, Google Code, Xcode

### Change Theme

```
/theme
```

### Custom Themes

In `settings.json`:

```json
{
  "ui": {
    "customThemes": {
      "MyTheme": {
        "name": "MyTheme",
        "type": "custom",
        "Background": "#181818",
        "Foreground": "#F8F8F2",
        "LightBlue": "#82AAFF",
        "AccentBlue": "#61AFEF",
        "AccentPurple": "#BD93F9",
        "AccentCyan": "#8BE9FD",
        "AccentGreen": "#50FA7B",
        "AccentYellow": "#F1FA8C",
        "AccentRed": "#FF5555",
        "Comment": "#6272A4",
        "Gray": "#ABB2BF"
      }
    }
  }
}
```

Load from file: `"ui": { "theme": "/path/to/theme.json" }`

---

## .copilotignore

Exclude files from Copilot Shell tools (similar to `.gitignore`).

Create `.copilotignore` in project root:

```
# Exclude directories
/packages/

# Exclude specific files
apikeys.txt

# Wildcards
*.md

# Negate (include back)
!README.md
```

Restart session after changes.

---

## Trusted Folders

Security feature controlling which projects can use full COSH capabilities.

### Enable

```json
{ "security": { "folderTrust": { "enabled": true } } }
```

### Trust Dialog Options

- **Trust folder**: Full trust for current folder
- **Trust parent**: Trust parent directory and all subdirectories
- **Don't trust**: Restricted "safe mode"

### Untrusted Restrictions

1. Project `.copilot-shell/settings.json` ignored
2. Project `.env` files ignored
3. Extension management restricted
4. Tool auto-acceptance disabled
5. Automatic memory loading disabled

### Manage

- View all rules: `~/.copilot-shell/trustedFolders.json`

---

## Context Files (COPILOT.md)

Hierarchical instructional context for the AI.

### Loading Order

1. `~/.copilot-shell/COPILOT.md` (global)
2. Project root and parent directories (project-specific)

### Commands

- `/memory refresh` - Reload all context files
- `/memory show` - Display current loaded context
- `/memory add --project/--global <text>` - Add to memory

### Import Syntax

Use `@path/to/file.md` in context files to import other Markdown files.

---

## Command-Line Arguments

| Argument | Alias | Description |
|---|---|---|
| `--model` | `-m` | Specify model |
| `--prompt` | `-p` | Headless mode |
| `--output-format` | `-o` | Output format |
| `--debug` | `-d` | Debug mode |
| `--all-files` | `-a` | Include all files |
| `--yolo` | | Auto-approve all |
| `--approval-mode` | | Set approval mode |
| `--continue` | | Resume recent session |
| `--resume` | | Resume specific session |
| `--include-directories` | | Add extra directories |
| `--experimental-lsp` | | Enable LSP |
| `--extensions` | `-e` | Specify extensions |
| `--proxy` | | Set proxy |
| `--checkpointing` | | Enable checkpointing |

---

## Environment Variables

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | API key for OpenAI-compatible provider |
| `OPENAI_BASE_URL` | Custom API base URL |
| `OPENAI_MODEL` | Model to use |
| `TAVILY_API_KEY` | Tavily web search API key |
| `NO_COLOR` | Disable color output |
| `DEBUG` / `DEBUG_MODE` | Enable debug logging |
