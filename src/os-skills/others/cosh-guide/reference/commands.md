# Commands Reference

Copilot Shell commands use three prefixes:

| Prefix | Function | Example |
|---|---|---|
| `/` (Slash) | Meta-level control of Copilot Shell | `/help`, `/settings` |
| `@` (At) | Inject local file content into conversation | `@src/main.py explain this` |
| `!` (Exclamation) | Direct system shell interaction | `!git status`, `!ls` |

---

## 1. Slash Commands (`/`)

### 1.1 Session and Project Management

| Command | Description | Usage |
|---|---|---|
| `/init` | Analyze directory and create initial context file | `/init` |
| `/summary` | Generate project summary from conversation | `/summary` |
| `/compress` | Replace chat history with summary to save tokens | `/compress` |
| `/resume` | Resume a previous conversation session | `/resume` |

### 1.2 Interface and Workspace Control

| Command | Description | Usage |
|---|---|---|
| `/clear` | Clear terminal screen | `/clear` (Ctrl+L) |
| `/theme` | Change visual theme | `/theme` |
| `/vim` | Toggle Vim editing mode | `/vim` |
| `/directory` | Manage multi-directory workspace | `/dir add ./src,./tests`, `/dir cd ./src` |
| `/editor` | Select supported editor | `/editor` |

### 1.3 Language Settings

| Command | Description | Usage |
|---|---|---|
| `/language` | View/change language settings | `/language` |
| `/language ui [lang]` | Set UI language | `/language ui zh-CN` |
| `/language output [lang]` | Set LLM output language | `/language output Chinese` |

Built-in UI languages: `zh-CN`, `en-US`
Aliases: `zh`, `en`

### 1.4 Tool and Model Management

| Command | Description | Usage |
|---|---|---|
| `/mcp` | List configured MCP servers/tools | `/mcp`, `/mcp desc` |
| `/tools` | Display available tool list | `/tools`, `/tools desc` |
| `/skills` | List/run available skills (experimental) | `/skills`, `/skills <name>` |
| `/approval-mode` | Change approval mode | `/approval-mode <mode>` |
| `/model` | Switch model in current session | `/model` |
| `/extensions` | List active extensions | `/extensions` |
| `/memory` | Manage AI instruction context | `/memory add --project/--global Important Info` |

Approval modes: `plan` (read-only), `default` (manual approval), `auto-edit` (auto-approve edits), `yolo` (auto-approve all)

### 1.5 Information, Settings, and Help

| Command | Description | Usage |
|---|---|---|
| `/help` | Display help | `/help` or `/?` |
| `/about` | Display version info | `/about` |
| `/stats` | Session statistics | `/stats` |
| `/settings` | Open settings editor | `/settings` |
| `/auth` | Change authentication | `/auth` |
| `/copy` | Copy last output to clipboard | `/copy` |
| `/quit` | Exit Copilot Shell | `/quit` or `/exit` |
| `/bash` | Exit Copilot Shell to bash | `/bash` |

---

## 2. @ Commands (File References)

| Format | Description | Example |
|---|---|---|
| `@<file path>` | Inject file content | `@src/main.py Please explain this` |
| `@<directory>` | Read all text files recursively | `@docs/ Summarize this` |

Notes:
- Spaces in paths need backslash escaping: `@My\ Documents/file.txt`
- File paths can be relative or absolute
- `@` file references auto-load `COPILOT.md` from the file's directory and parents
- Multiple files: `@file1.js and @file2.js`

---

## 3. Exclamation Commands (`!`) - Shell

| Format | Description | Example |
|---|---|---|
| `!<command>` | Execute in sub-shell | `!ls -la`, `!git status` |
| Standalone `!` | Toggle shell mode (all input as commands) | `!` (enter) -> commands -> `!` (exit) |

---

## 4. Custom Commands

Save frequently used prompts as shortcut commands.

### Storage Locations

| Type | Location | Priority |
|---|---|---|
| Global | `~/.copilot-shell/commands/` | Low |
| Project | `<project>/.copilot-shell/commands/` | High (overrides global) |

### Naming

File path maps to command name:
- `~/.copilot-shell/commands/test.md` -> `/test`
- `<project>/.copilot-shell/commands/git/commit.md` -> `/git:commit`

### Markdown Format (Recommended)

```markdown
---
description: Optional description (shown in /help)
---

Your prompt content here.
Use {{args}} for parameter injection.
```

### Dynamic Content Injection

| Type | Syntax | Purpose |
|---|---|---|
| File Content | `@{file path}` | Inject static reference files |
| Shell Commands | `!{command}` | Inject execution results (requires confirmation) |

### Example: Git Commit Command

````markdown
---
description: Generate Commit message based on staged changes
---

Please generate a Commit message based on the following diff:

```diff
!{git diff --staged}
```
````

---

## 5. Keyboard Shortcuts

### General

| Shortcut | Description |
|---|---|
| `Esc` | Close dialogs/suggestions |
| `Ctrl+C` | Cancel request / clear input (press twice to exit) |
| `Ctrl+D` | Exit if input empty (press twice to confirm) |
| `Ctrl+L` | Clear screen |
| `Ctrl+O` | Toggle debug console |
| `Ctrl+S` | Allow long responses to print fully |
| `Ctrl+T` | Toggle tool descriptions |
| `Shift+Tab` | Cycle approval modes |

### Input Prompt

| Shortcut | Description |
|---|---|
| `!` | Toggle shell mode (empty input) |
| `?` | Toggle shortcuts display (empty input) |
| `\` + Enter | Insert newline |
| `Tab` | Autocomplete suggestion |
| Up/Down Arrow | Navigate input history |
| `Ctrl+R` | Reverse search history |
| `Ctrl+V` | Paste clipboard (supports images) |
| `Ctrl+X` | Open in external editor |
| `Ctrl+A` | Beginning of line |
| `Ctrl+E` | End of line |
| `Ctrl+K` | Delete to end of line |
| `Ctrl+U` | Delete to beginning of line |
| `Ctrl+Backspace` | Delete word left |

### IDE Integration

| Shortcut | Description |
|---|---|
| `Ctrl+G` | See context from IDE |
