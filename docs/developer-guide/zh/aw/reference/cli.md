# CLI 自动化参考

[English](../../../en/aw/reference/cli.md)

完整子命令与参数枚举见用户手册的 [CLI 命令参考](../../../../user-guide/zh/aw/cli-reference.md)。
本页说明脚本与自动化使用的输出约定。

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
帮助和发现命令离线运行。`aw -V` 报告 CLI crate 版本。

## 独立实例 JSON

`aw instance` 使用 `aw-instance/v1` 信封，区分 `result.state`、操作错误和准备审计键。完整字段、确认期限与重复请求规则见[独立实例接入](../deployment/independent-instances.md)。

## 公共审计导出

`aw audit-export` 从已有的同用户服务状态目录读取一页，服务可以已经停止。
使用服务配置的状态路径，勿另建空目录替代。Journal 目录须属于有效用户且权限为 0700，
源描述符和链文件权限为 0600。请求文件须为普通文件，最多 16 KiB。
此命令不接受控制操作。

```bash
AW_STATE=/absolute/private/state
AW_QUERY="$HOME/aw-audit-query.json"
cat > "$AW_QUERY" <<'JSON'
{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":64}}
JSON
aw audit-export --state-dir "$AW_STATE" --request "$AW_QUERY"
```

成功响应包含 `error: null`、持久 `source_id` 与 `result.keys`。
将 `result.next_cursor` 放入下次枚举请求，直到它为 null。
`result.changed` 报告键集合变化，续页仍向后推进。游标保留首轮的 `through` 上限；
下一轮收集更晚及新插入的前部键。保留单链进度，并限制消费者循环。
对每个键，以 `{"type":"read","chain_id":"KEY","cursor":null,"limit":64}`
作为 `operation`，保留同样的 `api_version` 外层。用返回的键替换 `KEY`。
后续单链请求使用响应中的 `result.cursor`。

单链响应包含 ExecutionEvent、公共 AuditRecord、捕获的链尾、`caught_up`、`terminal`
及固定限制。页面按源位置计数，因此没有公共记录的页面也可能推进游标。
仅 `caught_up && terminal` 表示该链读完；开放链须重访，键枚举须定期从头开始。
记录和游标一起原子保存，以生产者身份和 UID 去重，重放时比较对象摘要。
历史生产者代际保持不变，响应不包含私有 Journal 记录。Schema、界限、损坏及历史缺失处理
见[导出合同](../../../../../src/aw/docs/design/zh/runtime/audit-export.md)。

协议失败返回 `result: null`、稳定 `error` 错误码，退出码为 1。
命令语法及请求文件 I/O 错误使用 stderr。取消没有写入副作用。
单次命令退出后无需停止资源；请求文件不再需要时删除 `"$AW_QUERY"`。
