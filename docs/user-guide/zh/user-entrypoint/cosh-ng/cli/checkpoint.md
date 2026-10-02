# 工作区快照

[English](../../../../en/user-entrypoint/cosh-ng/cli/checkpoint.md)

`cosh-cli checkpoint` 通过 ws-ckpt 守护进程保存、比较、恢复和清理工作区快照。在高风险变更前创建快照，操作失败时即可回滚。

## 前置条件和安全提示

- 必须运行 ws-ckpt 守护进程；socket 不可用时会返回 `CheckpointDaemonUnavailable`。
- 默认 socket 为 `/run/ws-ckpt/ws-ckpt.sock`；使用其他 socket 时传入 `--socket <path>`。
- 快照命令不支持 `--dry-run`。恢复、删除或清理前，请核对工作区和快照 ID。

## 命令

| 命令 | 必需参数 | 用途 |
|---|---|---|
| `cosh-cli checkpoint init` | `--workspace <path>` | 初始化工作区 |
| `cosh-cli checkpoint recover` | `--workspace <path>` | 将工作区恢复为普通目录，或还原中断初始化的备份 |
| `cosh-cli checkpoint create` | `--workspace <path> --id <id>` | 创建快照 |
| `cosh-cli checkpoint list` | 无（`--workspace` 可选） | 列出快照 |
| `cosh-cli checkpoint restore <id>` | `--workspace <path>` | 恢复快照 |
| `cosh-cli checkpoint status` | 无（`--workspace` 可选） | 查看守护进程状态 |
| `cosh-cli checkpoint delete` | `--snapshot <id>` | 删除快照 |
| `cosh-cli checkpoint diff` | `--workspace <path> --from <id> --to <id>` | 比较快照 |
| `cosh-cli checkpoint cleanup` | `--workspace <path>` | 保留限定数量的快照 |

所有命令都使用 `cosh-cli checkpoint` 前缀：

```bash
cosh-cli checkpoint init --workspace /home/agent/project
cosh-cli checkpoint create --workspace /home/agent/project --id before-change --message "safe point"
cosh-cli checkpoint list --workspace /home/agent/project
cosh-cli checkpoint diff --workspace /home/agent/project --from before-change --to after-change
cosh-cli checkpoint restore before-change --workspace /home/agent/project
```

其他可选参数包括：`create` 的 `--pin` 和 `--metadata <json>`，`delete` 的 `--force` 和 `--workspace <path>`，以及 `cleanup` 的 `--keep <count>`。`list` 和 `status` 省略 `--workspace` 时，会查询守护进程记录的所有工作区。

`recover` 由 daemon 判断工作区状态，因此初始化中断后原路径不存在时也可使用。
恢复成功且仍有迁移数据或备份需要检查时，JSON 响应仍为 `ok: true`，并在
`meta.warning` 中保留 daemon 提供的提示和位置；常规成功响应省略该字段。
数据来源与子卷丢失时的处理方式见
[ws-ckpt 恢复说明](../../../runtime/ws-ckpt.md#恢复中断的初始化与悬空注册)。

## 典型回滚流程

创建快照，执行并验证高风险操作；如果操作失败就恢复快照，成功后可在不再需要时清理旧快照。

```bash
cosh-cli checkpoint create --workspace /path/to/workspace --id pre-action --message "safe point"
cosh-cli checkpoint restore pre-action --workspace /path/to/workspace
cosh-cli checkpoint cleanup --workspace /path/to/workspace
```

响应使用标准的 [CoshResponse<T> 封装](../output-format.md)。
