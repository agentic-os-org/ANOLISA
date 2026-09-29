# Architecture Overview

Copilot Shell is a terminal-based AI programming assistant written in TypeScript,
organized as an npm monorepo.

## Repository Structure

```
deprecated/copilot-shell/
├── packages/
│   ├── cli/          # CLI entry point and TUI layer
│   ├── core/         # Core engine (models, tools, session management)
│   └── test-utils/   # Test helper utilities
├── scripts/          # Build and release scripts
├── integration-tests/ # End-to-end integration tests
├── hooks/            # Built-in hook scripts
└── eslint-rules/     # Custom ESLint rules
```

## Package Responsibilities

### `@copilot-shell/cli`

The CLI entry layer, responsible for:

- Parsing CLI arguments (`yargs`)
- Rendering interactive TUI (`ink` + React)
- Slash command registration and dispatch
- User input/output stream handling
- Extension and skill discovery and loading

### `@copilot-shell/core`

The core engine, responsible for:

- **Model Adaptation**: Unified OpenAI / Alibaba Cloud DashScope and other backends
- **Tool System**: Tool definitions, permission management, execution scheduling
- **Session Management**: Conversation history, context compression (Compact), checkpoints
- **Hook Runtime**: Event triggering, script execution, result aggregation
- **MCP Client**: stdio / SSE transport protocols
- **Configuration System**: Multi-layer config merging (System > User > Project > Defaults)
- **Security**: Sandbox integration, tool approval policies
- **Observability**: OpenTelemetry metrics / traces / logs

### `@copilot-shell/test-utils`

Shared testing utilities providing mock models, mock MCP servers, and other
test infrastructure.

## Key Design Decisions

### Layered Configuration

Configuration uses a four-layer priority system:

```
System Settings (/etc/copilot-shell/settings.json)       ← Admin-enforced (highest)
  ↓
Project-level (.copilot-shell/settings.json)
  ↓
User-level (~/.copilot-shell/settings.json)
  ↓
System Defaults (/etc/copilot-shell/system-defaults.json) ← Lowest
```

Array fields use a **replace** strategy; object fields use **shallow merge**.

### Agent Loop

Core loop flow:

```
User Input → UserPromptSubmit hooks
  → BeforeModel hooks → LLM Request → AfterModel hooks
  → BeforeToolSelection hooks → Tool Selection
  → PreToolUse hooks → Tool Execution → PostToolUse hooks
  → Stop hooks → Output
```

Each stage has corresponding hook events, allowing external scripts to
intercept the control flow.

### Model Adaptation Layer

Adapts multiple model backends through a unified `ModelProvider` interface:

- Request/response format standardization
- Unified streaming output handling
- Token counting and usage statistics
- Automatic authentication token refresh

### Tool Permission Model

Four-level approval modes:

| Mode | Behavior |
|------|----------|
| `plan` | All tools require confirmation |
| `default` | Only file modifications and shell require confirmation |
| `auto-edit` | Only shell requires confirmation |
| `yolo` | All auto-approved |

Allowlist (`allowedTools`) and exclude list (`excludeTools`) provide
fine-grained control.

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Runtime | Node.js ≥ 20 |
| Language | TypeScript (ESM) |
| Build | esbuild |
| TUI | ink (React) |
| Testing | vitest |
| Formatting | Prettier |
| Linting | ESLint |
| Package Management | npm workspaces |

## Directory Conventions

| Path | Purpose |
|------|---------|
| `~/.copilot-shell/` | User data directory (config, sessions, skills) |
| `.copilot-shell/` | Project-level configuration directory |
| `/etc/copilot-shell/` | System-level configuration |
| `~/.copilot-shell/extensions/` | Installed extensions |
| `~/.copilot-shell/skills/` | User-level skills |
