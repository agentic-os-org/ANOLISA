# Extension Management

Extensions are Copilot Shell's capability extension mechanism. External
components (such as agent-sec-core, tokenless) integrate into Copilot Shell
through declarative configuration without modifying the core code.

## View Loaded Extensions

```
/extensions
```

This command lists all discovered and loaded extensions in the current session.

## Extension Loading Paths

Copilot Shell searches for extensions in the following order:

1. **System-level directory**: `/usr/share/copilot-shell/extensions/`
2. **User-level directory**: `~/.copilot-shell/extensions/`
3. **Project-level directory**: `.copilot-shell/extensions/`
4. **CLI argument**: `--extensions` flag

Each extension directory should contain a `cosh-extension.json` declaration file.

## Extension Declaration Format

Extensions declare their capabilities via a `cosh-extension.json` file:

```json
{
  "name": "my-extension",
  "version": "1.0.0",
  "hooks": {
    "PreToolUse": [
      {
        "command": "${EXTENSION_DIR}/hooks/pre-tool.sh",
        "matcher": "Shell"
      }
    ]
  },
  "tools": [
    {
      "name": "my-custom-tool",
      "command": "${EXTENSION_DIR}/tools/my-tool.sh"
    }
  ]
}
```

### Variable Substitution

The following variables are supported in extension configuration:

| Variable | Meaning |
|----------|---------|
| `${EXTENSION_DIR}` | Absolute path of the current extension directory |

## Known Extensions

The following ANOLISA ecosystem components integrate via the extension mechanism:

| Extension | Function |
|-----------|----------|
| `agent-sec-core` | Security sandbox, command auditing, hooks injection |
| `tokenless` | LLM token compression optimization |

These extensions are automatically deployed to the system-level extension
directory when ANOLISA is installed.

## Enabling and Disabling Extensions

### Via CLI Arguments

```bash
# Load only specified extensions
cosh --extensions my-extension,another-extension

# List loaded extensions then exit
cosh --list-extensions
```

### Via Configuration File

```json
{
  "extensions": ["my-extension"]
}
```

## Extensions and Hooks

Extensions can register hooks into Copilot Shell's event system. Hooks
registered by extensions execute after user-defined hooks in priority:

1. User hooks (user settings)
2. Extension hooks (extension-injected)
3. Remote hooks (remotely loaded)

## Related Documentation

- [Hook Development Guide](../../../../developer-guide/en/copilot-shell/hooks/index.md) — Learn how extensions register hooks
