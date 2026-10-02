# Copilot Shell Hooks

Hooks are scripts or programs that copilot-shell executes at specific points in
the agent loop, allowing you to intercept and customize behavior without
modifying the CLI's source code.

## What are Hooks?

Hooks run synchronously as part of the agent loop — when a hook event fires,
copilot-shell waits for all matching hooks to complete before continuing.

With hooks, you can:

- **Inject context**: Inject relevant information (like git history) before the model processes a request
- **Validate actions**: Review tool arguments and block potentially dangerous operations
- **Enforce policies**: Implement security scanners and compliance checks
- **Log interactions**: Track tool usage and model responses for auditing
- **Optimize behavior**: Dynamically filter available tools or adjust model parameters
- **Sandbox commands**: Automatically wrap dangerous shell commands in linux-sandbox for isolated execution

### Getting Started

- **[Writing Hooks Guide](writing-hooks.md)**: A tutorial on creating hooks from scratch
- **[Hooks Reference](reference.md)**: Technical specification of I/O schemas and exit codes

## Core Concepts

### Hook Events

Hooks are triggered by specific events in copilot-shell's lifecycle.

| Event | When It Fires | Impact | Common Use Cases |
|-------|---------------|--------|------------------|
| `SessionStart` | Session begins (startup/resume/clear) | Inject Context | Initialize resources, load context |
| `SessionEnd` | Session ends (exit/clear) | Advisory | Clean up resources, save state |
| `UserPromptSubmit` | After user submits prompt, before planning | Block/Context | Add context, validate input |
| `Stop` | When agent is about to stop | Retry/Halt | Review output, force retry |
| `BeforeModel` | Before sending LLM request | Block/Mock | Modify request, swap model |
| `AfterModel` | After receiving LLM response | Block/Observe | Filter response, log |
| `BeforeToolSelection` | Before LLM selects tools | Filter Tools | Filter available tool set |
| `PreToolUse` | Before tool execution | Block/Rewrite | Validate arguments, block dangerous ops |
| `PostToolUse` | After tool execution | Block/Context | Process results, run tests |
| `PostToolUseFailure` | After tool execution failure | Recovery | Extract original command, sandbox bypass |
| `PreCompact` | Before context compression | Advisory | Save state |
| `Notification` | When system notification occurs | Advisory | Forward desktop alerts |
| `PermissionRequest` | When permission dialog shows | Allow/Deny | Auto-approve or deny |

### Global Mechanics

#### Strict JSON Requirements ("Golden Rule")

Hooks communicate via `stdin` (Input) and `stdout` (Output).

1. **Silence is mandatory**: Scripts **must not** output anything to `stdout` other than the final JSON object
2. **Pollution = failure**: If `stdout` contains non-JSON text, parsing will fail
3. **Debug via stderr**: All logging and debug output goes to `stderr` (e.g., `echo "debug" >&2`)

#### Exit Codes

| Exit Code | Label | Behavioral Impact |
|-----------|-------|-------------------|
| **0** | Success | `stdout` is parsed as JSON |
| **2** | System Block | Operation is aborted; `stderr` used as rejection reason |
| **Other** | Warning | Non-fatal failure; warning shown, continues |

#### Matchers

Use the `matcher` field to filter which specific tools or events trigger your hook:

- **Tool events** (`PreToolUse`, `PostToolUse`): Matchers are **Regular Expressions**
- **Lifecycle events**: Matchers are **Exact Strings**
- **Wildcards**: `"*"` or `""` (empty string) matches all

#### When Multiple Hooks Match

When multiple hooks match the same event:

1. **Plan and deduplicate**: Select hooks by event + matcher; deduplicate based on `name:command`
2. **Execution mode**: Default is **parallel**; if any hook sets `sequential: true`, all run **sequentially**
3. **Sequential chaining**: `PreToolUse` can modify `tool_input`; subsequent hooks see the modified input
4. **Final output merge**: Restrictive outcomes win (`deny`/`block`); reason texts are concatenated

## Configuration

Hooks are configured in `settings.json` with multi-layer merging (highest to lowest priority):

1. **Project settings**: `.copilot-shell/settings.json`
2. **User settings**: `~/.copilot-shell/settings.json`
3. **System settings**: `/etc/copilot-shell/settings.json`
4. **Extensions**: Hooks defined by installed extensions

### Configuration Example

```json
{
  "hooks": {
    "enabled": true,
    "PreToolUse": [
      {
        "matcher": "run_shell_command",
        "sequential": true,
        "hooks": [
          {
            "type": "command",
            "command": "python3 hooks/sandbox-guard.py",
            "name": "sandbox-guard",
            "timeout": 10000,
            "description": "Wraps dangerous commands in sandbox for execution"
          }
        ]
      }
    ]
  }
}
```

### Hook Configuration Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Execution engine; currently only `"command"` supported |
| `command` | string | Yes | Shell command to execute |
| `name` | string | No | Identifies the hook in logs and CLI commands |
| `timeout` | number | No | Execution timeout in milliseconds (default: 60000) |
| `description` | string | No | Brief explanation of the hook's purpose |

### Environment Variables

The following environment variables are available during hook execution:

- `COPILOT_SHELL_PROJECT_DIR`: Absolute path to the project root

## Security and Risks

> **WARNING**: Hooks execute arbitrary code with your user privileges.

**Project-level hooks** are particularly risky when opening untrusted projects.
copilot-shell **fingerprints** project hooks. If a hook's name or command
changes (e.g., via `git pull`), it is treated as a **new, untrusted hook** and
you will be warned before execution.

## Managing Hooks

Use CLI commands to manage hooks:

- **View**: `/hooks panel`
- **Enable/Disable all**: `/hooks enable-all` or `/hooks disable-all`
- **Toggle individual**: `/hooks enable <name>` or `/hooks disable <name>`
