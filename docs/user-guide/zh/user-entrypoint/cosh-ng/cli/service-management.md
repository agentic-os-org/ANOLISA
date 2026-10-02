# 服务管理

[English](../../../../en/user-entrypoint/cosh-ng/cli/service-management.md)

`cosh-cli svc` 管理 Linux systemd 服务，返回结构化 JSON，不需要解析 `systemctl` 的人类可读文本。服务命令依赖 systemd，变更操作通常需要 root 权限。

## 命令

| 命令 | 用途 |
|---|---|
| `cosh-cli svc status <name>` | 查看服务状态 |
| `cosh-cli svc start <name>` | 启动服务 |
| `cosh-cli svc stop <name>` | 停止服务 |
| `cosh-cli svc restart <name>` | 重启服务 |
| `cosh-cli svc enable <name>` | 启用开机启动 |
| `cosh-cli svc disable <name>` | 禁用开机启动 |
| `cosh-cli svc list` | 列出服务 |

## 查看服务

```bash
cosh-cli svc status nginx
cosh-cli svc list
cosh-cli svc list --state running
cosh-cli svc list --state failed
```

`status` 和 `list` 会在系统能够提供时返回 active/enabled 状态、PID、运行时长、内存、描述和最近日志等字段。响应封装见[输出格式](../output-format.md)。

## 修改服务

先预览状态变更：

```bash
cosh-cli svc restart nginx --dry-run
cosh-cli svc enable nginx --dry-run
```

`start`、`stop`、`restart`、`enable` 和 `disable` 都支持 `--dry-run`；去掉该参数才会执行操作。

## 状态和错误

`state` 字段可以是 `Running`、`Stopped`、`Failed`、`Activating`、`Deactivating`，也可能是 systemd 提供的 `Unknown` 值。常见错误包括 `SvcNotFound`、`SvcStartFailed`、`SvcStopFailed`、`UnsupportedDistro` 和 `PermissionDenied`；恢复建议见响应中的 `error.hint`。
