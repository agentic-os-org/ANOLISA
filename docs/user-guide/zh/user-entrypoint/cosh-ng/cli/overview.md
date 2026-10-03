# 管理系统操作

[English](../../../../en/user-entrypoint/cosh-ng/cli/overview.md)

`cosh-cli` 为脚本和 Agent 提供统一的 JSON 接口，用于软件包、服务、工作区快照和审计操作。每条命令都会向 stdout 写入一个 `CoshResponse<T>` 响应，成功退出码为 `0`，失败退出码为 `1`。

## 命令域

| 命令域 | 操作 |
|---|---|
| `pkg` | `install`、`remove`、`search`、`list` |
| `svc` | `status`、`start`、`stop`、`restart`、`enable`、`disable`、`list` |
| `checkpoint` | `init`、`recover`、`create`、`list`、`restore`、`status`、`delete`、`diff`、`cleanup` |
| `audit` | `check`、`log`、`status`、`events`、`trace`、`export`、`prune`、`policy` |

使用 `cosh-cli <domain> --help` 和 `cosh-cli <domain> <action> --help` 查看准确参数和默认值。

## 安全的起始命令

只读示例：

```bash
cosh-cli pkg search 'web*'
cosh-cli pkg list --installed
cosh-cli svc status nginx
cosh-cli svc list --state running
cosh-cli audit status
```

执行变更前先预览软件包和服务操作：

```bash
cosh-cli pkg install nginx --dry-run
cosh-cli svc restart nginx --dry-run
```

`--dry-run` 属于具体操作。软件包的 `install`、`remove`，以及服务的 `start`、`stop`、`restart`、`enable`、`disable` 支持该参数；快照变更不支持，版本 1 的 `audit prune` 只接受 `--dry-run`。

软件包和服务变更通常需要 root 权限。服务操作依赖 Linux systemd。快照操作需要运行中的 `ws-ckpt` 守护进程，要求提供工作区路径的命令还要求该路径存在。

## 在脚本或智能体中使用

1. 把 stdout 解析为一个 JSON 值。
2. 检查 `ok`，不要根据文字输出推断成功与否。
3. 失败时使用 `error.recoverable` 判断是否适合重试，并根据 `error.hint` 决定下一步。
4. 判断变更是否已执行前，先确认 `meta.dry_run`。
5. 单独保留 stderr；stdout 是自动化接口。

请先阅读[输出格式](../output-format.md)，再按需查看[软件包管理](package-management.md)、[服务管理](service-management.md)、[工作区快照](checkpoint.md)和[安全审计](audit.md)。
