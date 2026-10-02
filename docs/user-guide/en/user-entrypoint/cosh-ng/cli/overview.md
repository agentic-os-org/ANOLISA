# Manage System Operations

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/overview.md)

`cosh-cli` gives scripts and Agents one JSON interface for package, service, workspace checkpoint, and audit operations. Every command writes one `CoshResponse<T>` value to stdout and exits with `0` on success or `1` on failure.

## Command domains

| Domain | Actions |
|---|---|
| `pkg` | `install`, `remove`, `search`, `list` |
| `svc` | `status`, `start`, `stop`, `restart`, `enable`, `disable`, `list` |
| `checkpoint` | `init`, `recover`, `create`, `list`, `restore`, `status`, `delete`, `diff`, `cleanup` |
| `audit` | `check`, `log`, `status`, `events`, `trace`, `export`, `prune`, `policy` |

Use `cosh-cli <domain> --help` and `cosh-cli <domain> <action> --help` for the exact arguments and defaults.

## Safe first commands

Read-only examples:

```bash
cosh-cli pkg search 'web*'
cosh-cli pkg list --installed
cosh-cli svc status nginx
cosh-cli svc list --state running
cosh-cli audit status
```

Preview package and service changes before executing them:

```bash
cosh-cli pkg install nginx --dry-run
cosh-cli svc restart nginx --dry-run
```

`--dry-run` belongs to the action. Package `install` and `remove`, and service `start`, `stop`, `restart`, `enable`, and `disable` support it. Checkpoint mutations do not; `audit prune` accepts only `--dry-run` in version 1.

Package and service mutations normally need root privileges. Service operations require Linux systemd. Checkpoint operations require a running `ws-ckpt` daemon and an existing workspace for commands whose `--workspace` is required.

## Use from scripts and Agents

1. Parse stdout as one JSON value.
2. Check `ok`; do not infer success from text output.
3. On failure, use `error.recoverable` to decide whether a retry is useful and `error.hint` for the next action.
4. Check `meta.dry_run` before assuming a mutation happened.
5. Keep stderr separate from stdout; stdout is the automation contract.

See [Output format](../output-format.md) for the envelope and [Package management](package-management.md), [Service management](service-management.md), [Workspace checkpoints](checkpoint.md), and [Security audit](audit.md) for each domain.
