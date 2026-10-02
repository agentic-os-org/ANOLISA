# Extensions

[中文版](../../../../zh/user-entrypoint/cosh-ng/core/extensions.md)

An Extension packages reusable capabilities such as Skills, Hooks, MCP
servers, settings, context, or Agent definitions. Install only Extensions you
trust because they can add executable commands and external tools.

## Install or link an Extension

Run these commands at the `cosh` prompt:

```text
/extensions list
/extensions info <name>
/extensions doctor [name]

/extensions install ./extension
/extensions install https://example.com/extension.git --ref main
/extensions link ./extension
/extensions update <name>
/extensions update --all
/extensions uninstall <name>
```

`install` copies a package into the managed user store. `link` keeps using the
local directory, which is useful while developing. HTTPS Git sources may use
`--ref`. Run `/extensions help` for the installed version's syntax.

## Review and activate changes

Operations that add or change executable capabilities may wait for consent:

```text
/extensions operation <operation-id>
/extensions consent <operation-id>
/extensions cancel <operation-id>
```

Inspect the source and capability diff before consenting. Use
`/extensions enable <name>`, `/extensions disable <name>`, and
`/extensions reload` to control an installed package. If the same Extension is
found in system and user stores, choose one explicitly:

```text
/extensions select-source <name> user
/extensions select-source <name> system
```

## Extension settings

```text
/extensions settings list <name> [--scope user|workspace]
/extensions settings get <name> <key> [--scope user|workspace]
/extensions settings set <name> <key> <value> --scope user
/extensions settings unset <name> <key> --scope workspace
```

Sensitive settings use the operating-system secret store and display as
`[redacted]`; they cannot use workspace scope. Workspace settings require a
trusted project.

## Create a scaffold

Extension authors can create a starter package and validate it:

```text
/extensions new <path> --template minimal
/extensions doctor <name>
```

Templates include `minimal`, `skill`, `hook`, `mcp`, `context`, and `agent`.
Extension Hooks and tools follow the same approval rules as configured Hooks
and MCP servers.
