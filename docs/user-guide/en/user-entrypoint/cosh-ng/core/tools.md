# Agent tools

[中文版](../../../../zh/user-entrypoint/cosh-ng/core/tools.md)

The model can use a bounded set of built-in tools. Approval mode, explicit
allow-lists, and Hooks still decide whether a call runs.

## Built-in tools

| Kind | Tools | Typical use |
|---|---|---|
| ReadOnly | `read_file`, `read_many_files`, `grep`, `glob`, `list_directory` | Inspect files and paths |
| FileEdit | `edit`, `write_file`, `save_memory` | Change files or save a memory |
| ShellExec | `shell` | Run a shell command |
| Network | `web_fetch` | Fetch an HTTP resource |
| Other | `skill`, `todo`, `ask_user_question` | Reuse instructions, track work, ask the user |

`cosh_shell_evidence` is available only when Core starts with
`--enable-shell-evidence-tool`. Connected MCP tools use names such as
`mcp__<server>__<tool>`; Extensions may add their own external names.

## Approval choices

| Core mode | ReadOnly | FileEdit | Shell, network, MCP, external |
|---|---|---|---|
| `trust` | Run | Run | Run |
| `auto` | Run | Run | Ask |
| `recommend` | Run | Ask | Ask |

Unknown tool names are denied. Core and the interactive shell use
`recommend`, `auto`, and `trust`; legacy `balanced`, `suggest`, and `strict`
inputs normalize to `recommend`.

## Limit what the model sees

Use `--tools` for exposure and `--allowed-tools` only for exact names that
should bypass approval:

```bash
cosh-core --headless --tools read_file,grep,ask_user_question
cosh-core --headless --tools empty
cosh-core --headless --allowed-tools mcp__search__query
```

Allow-listing `shell`, a network tool, or an external tool grants real
authority. Keep the list as narrow as the task requires.

## Workspace and output limits

File-reading tools are rooted in the workspace captured when Core starts. A
later shell `cd` does not change that boundary, and paths that escape it are
rejected. Search results are bounded and report truncation when they are not
complete. MCP output entering Agent context is limited to 64 KiB.

See [MCP setup](../mcp.md) for external tools and [Extensions](extensions.md)
for extension-provided tools.
