# Service Management

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/service-management.md)

`cosh-cli svc` manages Linux systemd services and returns structured JSON instead of parsing human-readable `systemctl` output. Service commands require systemd; mutating commands normally need root privileges.

## Commands

| Command | Purpose |
|---|---|
| `cosh-cli svc status <name>` | Show service status |
| `cosh-cli svc start <name>` | Start a service |
| `cosh-cli svc stop <name>` | Stop a service |
| `cosh-cli svc restart <name>` | Restart a service |
| `cosh-cli svc enable <name>` | Enable start at boot |
| `cosh-cli svc disable <name>` | Disable start at boot |
| `cosh-cli svc list` | List services |

## Inspect a service

```bash
cosh-cli svc status nginx
cosh-cli svc list
cosh-cli svc list --state running
cosh-cli svc list --state failed
```

`status` and `list` return fields such as active/enabled state, PID, uptime, memory, description, and recent logs when the system provides them. See [Output format](../output-format.md) for the response envelope.

## Change a service

Preview a state change first:

```bash
cosh-cli svc restart nginx --dry-run
cosh-cli svc enable nginx --dry-run
```

The same `--dry-run` flag is available on `start`, `stop`, `restart`, `enable`, and `disable`. Remove it to execute the operation.

## States and errors

The `state` field can be `Running`, `Stopped`, `Failed`, `Activating`, `Deactivating`, or an `Unknown` value supplied by systemd. Common failures are `SvcNotFound`, `SvcStartFailed`, `SvcStopFailed`, `UnsupportedDistro`, and `PermissionDenied`; use `error.hint` for recovery guidance.
