# CLI 自动化参考

[English](../../../en/aw/reference/cli.md)

用 `aw commands --json` 获取命令和参数清单，用具体命令的 `--help` 查询条件约束。

## 输出选择

| 入口 | 输出 |
| --- | --- |
| `install`、`uninstall`、`list --installed`、`doctor`、`recover`、`config init/validate/add-*/remove-*` | 可选局部 `--json` |
| `service status/stop`、`request`、`config show`、`update` | 固定 JSON，不另加 `--json` |
| `inspect` | 必须传 `--json` |
| `run` | 保留原生 Agent 输出；`--` 后参数原样传递 |

管理命令的人类输出支持 `--quiet`、`--verbose` 和 `--color auto|always|never`。
quiet 与 verbose 互斥，三者均与 JSON 冲突。quiet 保留错误和清理警告。

## 管理报告

```bash
aw config validate --config "$HOME/aw/aw.yaml" --json
aw doctor --prefix "$AW_PREFIX" --json
```

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

| 字段或状态 | 消费方式 |
| --- | --- |
| `format: 1` | 报告版本 |
| `status: succeeded` | 退出 0，读取有类型的 `result` |
| `failed` / `cancelled` / `partial` | 退出 1，读取 `error.code`；partial 表示恢复未完成 |
| `mutation.committed` | 包文件及记录是否已跨过持久提交点 |
| `mutation.recovery` | `not_attempted`、`complete` 或 `incomplete` |
| `warnings` | 检查资源路径并完成剩余清理；已提交卸载可能带警告退出 0 |
| `changed: false` | 编辑未改变配置字节 |
| `policy_update_scope: true` | 候选通过离线修改范围检查；随后仍须执行 `aw update` |

语法错误在 stderr 输出文本，stdout 为空，退出 1。I/O 失败可能发生在文件发布之后，重试前先查询状态。
`config show` 返回完整私有配置；管理报告的固定错误摘要不适用于原生输出或协议流。

## 卸载与恢复

脚本使用 `aw uninstall --prefix ABS_PREFIX --yes`，system scope 同时需要 `sudo` 和 `--scope system`。
交互卸载只接受 `y` 或 `yes`；JSON 和无终端调用必须提供 `--yes`。`--dry-run` 只预览。
包错误 `needs_recovery`、`package_in_use`、`plan_blocked` 及恢复步骤见[包管理](packages.md)。

## 命令发现

`aw commands --json` 返回 `format`、`name`、`version` 和 `commands`，每项列出命令路径和可见参数。
`required` 仅标记无条件必需参数。补全使用同一声明，支持 bash、zsh、fish、powershell、elvish。
帮助和发现命令离线运行。版本 `aw -V` 为 CLI crate 版本，Preview 包标识由安装记录提供。
