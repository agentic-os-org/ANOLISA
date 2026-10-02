# Package Management

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/package-management.md)

`cosh-cli pkg` provides structured package operations. It routes to dnf, apt, zypper, or Homebrew according to the detected platform and returns the common JSON envelope.

## Commands

| Command | Purpose |
|---|---|
| `cosh-cli pkg install <package>` | Install a package |
| `cosh-cli pkg remove <package>` | Remove a package |
| `cosh-cli pkg search <query>` | Search package names |
| `cosh-cli pkg list --installed` | List installed packages |

## Install or remove

Preview a change before executing it:

```bash
cosh-cli pkg install nginx --dry-run
cosh-cli pkg remove nginx --dry-run
```

Run without `--dry-run` to apply the change. Package operations normally need root privileges. An install that finds the package already present still returns success and marks `already_installed` in the response.

`--dry-run` previews the operation without installing or removing anything, downloading packages, or writing to the package database. What it verifies depends on the backend. On dnf, apt, and zypper it confirms that dependency resolution succeeds against the current metadata; it does not cover failures that only surface during the real transaction, such as download or signature errors, file conflicts, failing scriptlets, or state changed by a concurrent package operation. Those backends read repository metadata to resolve, and fetch it from the network when the local copy is missing or expired. Homebrew has no simulation mode, so its `--dry-run` only confirms that the formula exists (install) or is installed (remove) — it does not resolve dependencies or conflicts at all.

## Search

The query is passed as one argument and uses a portable package-name pattern. The accepted pattern characters are package-name characters plus `*`, `?`, `[` and `]`:

```bash
cosh-cli pkg search 'libssl*'
cosh-cli pkg search 'python3-?'
cosh-cli pkg search 'lib[0-9]*'
```

Backend-specific regular expressions, shell metacharacters, an empty query, and a query beginning with `-` are rejected. `cosh-cli` keeps whole-package-name matching consistent across supported backends.

## List and errors

`list --installed` returns package names and versions. Search results also report whether each package is installed; a search result may omit its version, and a package listing may omit architecture or repository when the backend does not provide them.

Common failures are `PkgNotFound`, `PkgBackendError`, `UnsupportedDistro`, and `PermissionDenied`. Use `error.hint` in the response for the suggested next step. See [Supported platforms](../supported-distros.md) for routing details and [Output format](../output-format.md) for the response envelope.
