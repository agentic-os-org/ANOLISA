# Tokenless Plugin for Codex

Command-output reduction plugin for
[Codex](https://github.com/openai/codex). It rewrites supported shell commands
through RTK before execution (declaring `post_tool_replacement: "none"` to Core).
Legacy Tool Ready pre-checks and PostTool `[tokenless:env]` diagnostics remain
registered as silent pass-throughs.

## Features

| Feature | Description |
|---------|-------------|
| **Command Rewriting** | Rewrites supported shell commands through RTK so verbose output is reduced at its source |
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
   - Rewrites commands through Tokenless PreTool (`tokenless compress`)
   - Only applies to Bash/Shell/terminal/programmatic tools
4. **`PostToolUse` (response-diagnostics, retired)** — remains registered as a
   silent pass-through (`exit 0` with empty stdout); `[tokenless:env]`
   environment-error attribution has been retired.

> **Codex protocol constraint**: `PostToolUse` rejects output suppression and
> replacement. Injecting compressed content through `additionalContext` would
> leave the original in place and increase the prompt. Actual first-pass
> savings come from RTK rewriting supported commands before they run.

## Installation

### Prerequisites

- Rust toolchain ≥ 1.91.0
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
# {"installed": true, "status": "installable", "version": "1.0.0", "path": "/home/user/.local/bin/tokenless"}
```

`installed` describes the tokenless binary. `status` describes this adapter and
is what `tokenless init` reads:

- `installable` — a Codex CLI can be resolved, so `install.sh` will register the
  plugin. `detect.sh` never asks Codex whether the plugin is *already*
  registered, so this is the most it can claim; `install.sh` is idempotent, so
  re-running it against an already registered host is safe.
- `missing_prereqs` — no Codex CLI can be resolved (`CODEX_BIN` pinned to
  something unusable, or no `codex` on `PATH`). `install.sh` would print
  "skipping plugin registration" and exit 0 having changed nothing, so the
  adapter is not offered for installation.

The exit code stays 0 in both cases: plugin activation is controlled by
capabilities, not by this probe.

## Hook Output Format

Both `PreToolUse` (`tool-ready`) and `PostToolUse` (`response-diagnostics`) exit
with empty stdout (`exit 0`); only `PreToolUse` (`rewrite-hook`) emits updated
command arguments.

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
    ├─ PreToolUse: tokenless PreTool
    │   └─ Replaces the command with an output-reducing equivalent
    │
    ├─ Tool execution
    │   └─ Codex receives the already-reduced output
    │
    └─ PostToolUse: silent pass-through (diagnostics retired)
```

## Architecture

```
codex-plugin-tokenless/
├── plugin.json.in           # Codex plugin manifest (version-stamped by Makefile)
├── hooks/
│   └── hooks.json           # Hook definitions (SessionStart, PreToolUse, PostToolUse)
├── scripts/
│   ├── response-diagnostics # PostToolUse: retired silent pass-through
│   ├── rewrite-hook         # PreToolUse: RTK command rewriting
│   ├── tool-ready           # PreToolUse: registered hard-disabled pass-through
│   ├── check-tokenless      # SessionStart: version/availability check
│   ├── install.sh           # Build and install tokenless CLI + register plugin
│   ├── detect.sh            # Detect tokenless availability
│   └── uninstall.sh         # Cleanup plugin registration + marketplace
└── README.md
```

## Related

- [Tokenless Rust CLI](../../../crates/tokenless-cli/) — core compression engine
- [OpenClaw Plugin](../openclaw/) — same compression for OpenClaw
- [Hermes Plugin](../hermes/) — same compression for Hermes

## License

Same as the ANOLISA project.
