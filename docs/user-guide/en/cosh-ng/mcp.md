# Connect an MCP server

[中文版](../../zh/cosh-ng/mcp.md)

MCP servers add tools from a local process or a remote Streamable HTTP service.
Configure them once, connect them from `cosh`, then inspect the names before
asking the Agent to use them.

## Configure a local stdio server

Put server definitions in `~/.copilot-shell/config.toml` or
`/etc/copilot-shell/config.toml`. Project config cannot add an MCP server.

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path/to/workspace"]
startup_timeout_ms = 30000
timeout_ms = 10000
allowed_tools = ["read_file", "list_directory"]
```

The command is started directly, not through the interactive Shell. Add child
environment variables explicitly:

```toml
[mcp.servers.filesystem.env]
SERVICE_TOKEN = "${FILESYSTEM_MCP_TOKEN}"
```

`allowed_tools` may list discovered tool names; omit it to expose all tools or
set `[]` to expose none.

## Configure a remote server

Use `url` instead of `command` for a Streamable HTTP endpoint:

```toml
[mcp.servers.search]
url = "https://mcp.example.com/mcp"
allowed_tools = ["query"]

[mcp.servers.search.oauth]
scopes = ["search"]
```

For a static token, remove the OAuth table and set:

```toml
bearer_token = "${SEARCH_MCP_TOKEN}"
```

Use HTTPS for remote endpoints. HTTP is accepted only for loopback hosts such
as `localhost`, `127.0.0.1`, or `::1`. A server must define exactly one of
`command` and `url`.

## Trust a stdio server

A local stdio server is code that runs at session startup, so cosh starts it
only after you explicitly trust it. A newly configured stdio server is listed
as needing trust and does not auto-start until trusted. Remote HTTP servers
are not gated because they run no local code.

```text
/mcp show filesystem
/mcp trust filesystem
```

`show` prints the command, arguments, environment variable names and a trust
fingerprint without starting the server. `trust` records that fingerprint and
allows startup; in the TUI it previews the configuration first and binds the
trust to the fingerprint you saw. In a terminal, `cosh-core mcp trust
filesystem` asks for a `y/N` confirmation; piped or non-interactive calls are
refused unless you pass `--yes`.

The pin covers the resolved command, arguments, explicit environment values
and `allowed_tools`. Changing any of them — including starting the session
from a different workspace so a relative command resolves elsewhere —
invalidates the pin, so the server stops auto-starting until you trust it
again. Re-run `show` and `trust` after editing the definition.

The running session uses the config loaded at startup. A trust or revocation
is written to disk immediately and applies to future startups; the current
session picks it up on the next snapshot rebuild. If you edited the config or
the rebuild fails, restart `cosh` so the change takes effect in the current
session.

```text
/mcp untrust filesystem
```

`untrust` revokes startup trust by name, even if the server is no longer in the
config, so a removed server cannot revive its old trust. Revocation takes
effect on the next snapshot rebuild.

## Connect and inspect

Start `cosh` in the workspace the server should receive, then run:

```text
/mcp list
/mcp connect filesystem
/mcp inspect filesystem
```

`list` confirms the definition was loaded. `connect` starts or contacts the
server and discovers tools; for a stdio server it requires an existing trust
pin and refuses an untrusted server until you `trust` it. `inspect` and
`refresh` also refuse an untrusted stdio server. `inspect` shows the discovered
and Agent-visible names without printing credentials. MCP tools are exposed as
`mcp__<server>__<tool>` and remain subject to approval.

For OAuth, run the login command in a Shell (the interactive `/mcp login`
command prints this instruction):

```bash
cosh-core mcp login search
```

Finish the browser flow, then connect and inspect the server from `cosh`.

## Refresh or disconnect

```text
/mcp refresh filesystem
/mcp disconnect filesystem
/mcp logout search
```

`refresh` rediscovers tools (it requires an existing trust pin for stdio
servers). `disconnect` disables startup connection and
removes saved OAuth credentials; `connect` enables it again. `logout` removes
OAuth credentials without changing the definition. Changes take effect for the
next Agent task when a task is already running.

## Troubleshoot

| Symptom | Check |
|---|---|
| `/mcp list` is empty | Use system or user config, not project config |
| stdio server does not auto-start | Trust it: `/mcp show <server>` then `/mcp trust <server>` |
| Local server will not start | Check executable, arguments, `env`, and `startup_timeout_ms` |
| Connected server exposes no tools | Check `allowed_tools` (`[]` exposes none) |
| Remote endpoint is rejected | Use HTTPS, or HTTP only on loopback; check token/OAuth settings |
| OAuth login cannot start in `cosh` | Run `cosh-core mcp login <server>` in a Shell, then connect |

MCP output entering Agent context is limited to 64 KiB.
