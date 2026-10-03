# Integrate another frontend

[中文版](../../../../zh/user-entrypoint/cosh-ng/core/overview.md)

`cosh-core` runs an Agent without the interactive terminal UI. Start `cosh` for
normal terminal use; call `cosh-core` directly when another frontend needs a
JSONL process, a one-shot prompt, or session management.

## Start a core process

```bash
# One prompt, then exit
cosh-core --headless "Inspect disk usage; do not modify anything"

# Long-running JSONL process
cosh-core --headless

# Resume or compact a saved conversation
cosh-core --headless --resume <session-id>
cosh-core --headless --resume <session-id> --compact

# Handle one provider-free registry request from stdin
cosh-core --registry
```

When stdin is not a TTY, `cosh-core` selects headless mode automatically. In
headless and registry modes, stdout is JSONL protocol output; logs go to the
configured log file or stderr.

## Options used by integrations

| Option | Use |
|---|---|
| `--model <name>` | Override the configured model for this process |
| `--approval-mode <mode>` | Select `recommend`, `auto`, or `trust` |
| `--allowed-tools <names>` | Let exact tool names bypass approval |
| `--tools <selection>` | Expose `default`, `empty`, or a comma-separated subset |
| `--bare` | Ignore project config, Hooks, Skills, Extensions, and persistence |
| `--resume <id>` | Select a saved conversation for the current workspace |
| `--compact` | Compact the selected conversation and exit |
| `--enable-shell-evidence-tool` | Expose bounded terminal evidence to cosh-shell |

`--tools` controls what the model can see. `--allowed-tools` changes the
approval boundary; allow-listing a tool can grant real execution authority.

## Connect a frontend

1. Start `cosh-core --headless` and keep stdin/stdout open.
2. Send a `control_request` with `subtype: "initialize"`, then send `user`
   messages as JSON objects, one per line.
3. Read streamed output and answer Core `control_request` messages with the
   same request ID. A client must handle tool approval, user questions, and
   authentication when they occur.
4. Send `subtype: "shutdown"` when the frontend is done.

See [Headless mode](headless-mode.md) for message examples and
[the IPC protocol reference](../../../../../developer-guide/en/cosh-ng/ipc-protocol.md)
for the complete schema. Configure credentials in [Providers](providers.md),
and see [Configuration](../configuration.md) for workspace and persistence
settings.
