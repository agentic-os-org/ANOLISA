# Using Hooks

Hooks are Copilot Shell's interception mechanism. They execute custom scripts
before and after tool calls and agent processing, enabling security enforcement,
automated approval, context injection, and more.

## Quick Start

List all registered hooks:

```
/hooks list
```

## Hook Events

Copilot Shell supports the following hook events:

| Event | Trigger | Typical Use |
|-------|---------|-------------|
| `SessionStart` | Session begins (start/resume/clear) | Initialize environment, load context |
| `SessionEnd` | Session ends (exit/clear) | Clean up resources, save state |
| `UserPromptSubmit` | After user submits prompt, before planning | Inject context, validate input, block turn |
| `Stop` | When agent is about to stop | Review output, force retry |
| `BeforeModel` | Before sending LLM request | Switch model, modify parameters, mock response |
| `AfterModel` | After receiving LLM response | Filter response, log |
| `BeforeToolSelection` | Before LLM selects tools | Filter available tool set |
| `PreToolUse` | Before tool execution | Intercept dangerous commands, modify parameters, security audit |
| `PostToolUse` | After tool execution | Process results, log, hide sensitive output |
| `PostToolUseFailure` | After tool execution failure | Error recovery, sandbox bypass |
| `PreCompact` | Before context compression | Save state, notify user |
| `Notification` | When system notification occurs | Forward desktop alerts |
| `PermissionRequest` | When permission dialog shows | Auto-approve or deny permissions |

## Managing Hooks

### View Registered Hooks

```
/hooks list
```

Displays all hooks with their name, source, and status (enabled/disabled).

### Enable or Disable Specific Hooks

Disable via configuration file:

```json
{
  "hooksConfig": {
    "disabled": ["sandbox-guard"]
  }
}
```

### Disable Hooks System Globally

```json
{
  "hooksConfig": {
    "enabled": false
  }
}
```

## Hook Configuration Format

Configure custom hooks in `settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "run_shell_command",
        "sequential": true,
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/my-hook.sh",
            "name": "my-hook",
            "timeout": 10000
          }
        ]
      }
    ]
  }
}
```

### Configuration Fields

Each event contains an array of matcher groups. Each matcher group has:

| Field | Type | Description |
|-------|------|-------------|
| `matcher` | string | Tool name regex to match; empty or `"*"` matches all |
| `sequential` | boolean | Execute sequentially (default: parallel) |
| `hooks` | array | Hook list for this matcher group |

Each hook object:

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Execution engine; currently only `"command"` |
| `command` | string | Hook script path or command |
| `name` | string | Hook name (used in logs and management commands) |
| `timeout` | number | Timeout in milliseconds (default: 60000) |

## Hook Source Priority

When multiple sources define hooks for the same event, execution priority is:

1. **User** — Hooks defined in user settings
2. **Extension** — Hooks injected by extensions
3. **Remote** — Remotely loaded hooks

## Hook Input/Output

Hook scripts receive JSON input via stdin and return JSON output via stdout.

### PreToolUse Input Example

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "run_shell_command",
  "tool_input": {
    "command": "rm -rf /tmp/test"
  },
  "session_id": "abc123",
  "cwd": "/home/user/project",
  "timestamp": "2025-01-01T00:00:00Z"
}
```

### PreToolUse Output Examples

Deny execution:

```json
{
  "decision": "deny",
  "reason": "Dangerous command intercepted"
}
```

Allow execution with modified parameters:

```json
{
  "hookSpecificOutput": {
    "tool_input": {
      "command": "linux-sandbox -- rm -rf /tmp/test"
    }
  }
}
```

## Related Documentation

- [Hook Development Guide](../../../../developer-guide/en/copilot-shell/hooks/index.md)
- [Hook API Reference](../../../../developer-guide/en/copilot-shell/hooks/reference.md)
- [Writing Custom Hooks](../../../../developer-guide/en/copilot-shell/hooks/writing-hooks.md)
- [AgentSecCore Internal Commands](../../agent-security/agent-sec-core/internal-commands.md) — contract of the `agent-sec-cli log-sandbox` audit command that `sandbox-guard` spawns
