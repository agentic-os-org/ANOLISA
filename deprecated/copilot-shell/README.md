# Copilot Shell

[中文版](README_zh.md)

An AI-powered terminal assistant for code understanding, task automation, and system management. Copilot Shell is a legacy component of [ANOLISA](../../README.md), built on top of upstream [Qwen Code](https://github.com/QwenLM/qwen-code) v0.9.0.

Deprecated for new development; use [cosh-ng](../../src/cosh-ng/README.md) instead. Existing commands, installation, builds, and release identities remain supported during this transition. Read the [root AGENTS.md](../../AGENTS.md) before making changes.

## Features

- **Natural Language Coding** — Describe changes in plain language to modify code, implement features, or fix bugs.
- **Code Analysis & Navigation** — Understand entire project structures and answer code-related questions.
- **Multi-Tool Orchestration** — Integrates file, shell, search, web, LSP, and MCP tools in a single session.
- **Interactive Shell** — `/bash` command to drop into an interactive shell; type `exit` to return.
- **Skill System** — Local + remote skill discovery with priority-based fallback (Project > User > Extension > Remote).
- **Hooks System** — PreToolUse events for intercepting tool calls before execution.
- **Git Workflow Automation** — Automate commits, branch creation, conflict resolution, and release notes.
- **Multi-Provider Support** — Aliyun Authentication (ECS RAM Role web auth or AK/SK), Qwen OAuth, Custom Provider (DashScope, DeepSeek, Kimi, GLM, MiniMax, or any OpenAI-compatible endpoint).
- **PTY Mode** — Full pseudo-terminal support including `sudo` commands.
- **Extensible** — Extend capabilities via MCP servers and custom skills.

## Quick Start

### Install from RPM

```bash
sudo yum install copilot-shell
```

### Install from Source

```bash
cd deprecated/copilot-shell
make build
```

### Run

```bash
make start

# Interactive mode
cosh

# Or use alternative aliases
co
copilot
```

### Authenticate

```bash
# Aliyun Authentication (default)
# - On ECS: auto-detects environment, opens browser or shows QR code for web auth
# - Elsewhere: enter AK/SK directly
cosh    # follow the on-screen prompts
```

> ** Note: ** Supports reusing openclaw model configurations.

## Architecture

Copilot Shell uses a monorepo layout with npm workspaces:

| Package               | Description                                                                |
| --------------------- | -------------------------------------------------------------------------- |
| `packages/cli`        | Terminal UI layer — input handling, command parsing, Ink/React rendering   |
| `packages/core`       | Backend core — AI model communication, prompt building, tool orchestration |
| `packages/test-utils` | Shared test utilities                                                      |

## Development

### Prerequisites

- Node.js >= 20.0.0
- npm (included with Node.js)

### Commands

```bash
make install          # Install dependencies
make build            # Build all packages
make test             # Run unit tests
make lint             # Lint with ESLint
make format           # Format with Prettier
```

> **Note:** `make install` (i.e. `npm install`) automatically sets up husky pre-commit hooks. Staged files will be checked with Prettier and ESLint before each commit. Use `git commit --no-verify` to bypass if needed.

### RPM Packaging

```bash
make rpm
```

## Configuration

Copilot Shell uses a layered configuration system (highest priority first):

1. Command-line arguments
2. Environment variables
3. Project settings (`.copilot-shell/settings.json`)
4. User settings (`~/.copilot-shell/settings.json`)
5. System settings
6. Defaults

## Documentation

- [Quick Start](../../docs/user-guide/en/user-entrypoint/copilot-shell/QUICKSTART.md)
- [Contributing](CONTRIBUTING.md)

## License

Apache License 2.0 — see [LICENSE](../../LICENSE) for details.
