# Tokenless Plugin for Codex

Command-output reduction and environment error detection plugin for
[Codex](https://github.com/openai/codex). It rewrites supported shell commands
through RTK before execution and classifies environment errors with actionable
fix hints.

## Features

| Feature | Description |
|---------|-------------|
| **Command Rewriting** | Rewrites supported shell commands through RTK so verbose output is reduced at its source |
| **Environment Error Detection** | Classifies tool failures as dependency/permission/file/network/package issues; injects fix hints to prevent retry loops |
| **Statistics Tracking** | Attributes RTK rewrites to the Codex session for auditing and optimization |

## How It Works

The plugin registers four hooks with Codex:

1. **`SessionStart`** — verifies the `tokenless` CLI is installed and functional (non-blocking)
2. **`PreToolUse` (tool-ready, hard-disabled)** — remains registered but is a
   silent pass-through. `tokenless env-check` returns `UNKNOWN` with
   `enabled:false`, so the hook emits no context, performs no repair, and never
   blocks a tool. Re-enabling the retained legacy pipeline requires a source
   change and a new release.
3. **`PreToolUse` (rewrite)** — runs before shell command execution:
   - Rewrites commands via `rtk rewrite` for token optimization
   - Only applies to Bash/Shell/terminal/programmatic tools
4. **`PostToolUse`** — runs after every tool execution:
   - Skips content-reading and task-management tools
   - Classifies environment errors and injects fix hints

> **Codex protocol constraint**: `PostToolUse` rejects output suppression and
> replacement. Injecting compressed content through `additionalContext` would
> leave the original in place and increase the prompt. The plugin therefore
> reserves `additionalContext` for actionable environment diagnostics. Actual
> first-pass savings come from RTK rewriting supported commands before they run.

## Installation

### Prerequisites

- Rust toolchain ≥ 1.89.0
- The `anolisa` source tree (this repository)

### Install

```bash
cd src/tokenless/adapters/tokenless/codex
./scripts/install.sh
```

This builds the `tokenless` Rust CLI in release mode and installs it to
`~/.local/bin/`. Add `~/.local/bin` to your `PATH` if not already present.

### Configure Codex

Add the plugin to your Codex `config.toml`:

```toml
[plugins]
tokenless = { enabled = true }
```

Or install from the marketplace (once published).

### Verify

```bash
./scripts/detect.sh
# {"installed": true, "version": "1.0.0", "path": "/home/user/.local/bin/tokenless"}
```

## Hook Output Format

For a classified environment failure, the plugin injects `additionalContext`
in the following format:

```
[tokenless:env:ENV_DEPENDENCY_MISSING] Missing dependency detected. ...
Do NOT retry the same command — fix the environment first.
```

## Configuration

Configuration is managed through the `tokenless` CLI's environment and config file:

| Variable | Default | Description |
|----------|---------|-------------|
| `TOKENLESS_AGENT_ID` | `codex` | Agent identifier for statistics |
| `TOKENLESS_BIN` | (auto-detected) | Path to tokenless binary |
| `TOKENLESS_STATS_ENABLED` | `1` | Enable statistics recording |
| `TOKENLESS_DATA_DIR` | `~/.tokenless` | Directory for `stats.db` and `stash.db` |
| `TOKENLESS_STATS_DB` | `~/.tokenless/stats.db` | Statistics database path |
| `TOKENLESS_STASH_DB` | `~/.tokenless/stash.db` | Reversible stash database path |

`TOKENLESS_STATS_DB` and `TOKENLESS_STASH_DB` take precedence over
`TOKENLESS_DATA_DIR`, which may be any accessible absolute non-root directory.
Custom database files must remain under the real user home or selected data
directory.

### View Statistics

```bash
tokenless stats summary
tokenless stats list --limit 20
tokenless stats show <id>
```

## Savings Path

```
Shell command
    │
    ├─ PreToolUse: rtk rewrite
    │   └─ Replaces the command with an output-reducing equivalent
    │
    ├─ Tool execution
    │   └─ Codex receives the already-reduced output
    │
    └─ PostToolUse: environment diagnostics only
```

## Architecture

```
codex-plugin-tokenless/
├── plugin.json.in           # Codex plugin manifest (version-stamped by Makefile)
├── hooks/
│   └── hooks.json           # Hook definitions (SessionStart, PreToolUse, PostToolUse)
├── scripts/
│   ├── response-diagnostics # PostToolUse: environment error detection
│   ├── rewrite-hook         # PreToolUse: RTK command rewriting
│   ├── tool-ready           # PreToolUse: registered hard-disabled pass-through
│   ├── check-tokenless      # SessionStart: version/availability check
│   ├── install.sh           # Build and install tokenless CLI + register plugin
│   ├── detect.sh            # Detect tokenless availability
│   └── uninstall.sh         # Cleanup plugin registration + marketplace
└── README.md
```

## Related

- [Tokenless Rust CLI](../../crates/tokenless-cli/) — core compression engine
- [OpenClaw Plugin](../openclaw/) — same compression for OpenClaw
- [Hermes Plugin](../hermes/) — same compression for Hermes

## License

Same as the ANOLISA project.
