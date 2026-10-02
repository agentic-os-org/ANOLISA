# 安全审计

[English](../../../../en/user-entrypoint/cosh-ng/cli/audit.md)

`cosh-cli audit` 检查操作是否获准，并读取用于排障的脱敏审计事件。它支持策略检查、分页查询、关联追踪、事故导出和保留计划预览。所有命令都返回标准的 `CoshResponse<T>` JSON 响应封装。

## 命令

| 命令 | 用途 |
|---|---|
| `cosh-cli audit check` | 根据当前策略评估操作 |
| `cosh-cli audit log` | 读取某个会话的策略决策事件 |
| `cosh-cli audit status` | 查看审计存储和读取健康状态 |
| `cosh-cli audit events` | 分页查询审计事件 |
| `cosh-cli audit trace <id>` | 根据 ID 或关联身份追踪事件 |
| `cosh-cli audit export --output <dir>` | 写出脱敏事故包 |
| `cosh-cli audit prune --dry-run` | 预览保留计划候选项 |
| `cosh-cli audit policy ...` | 查看或校验策略文件 |

使用 `cosh-cli audit --help` 或具体操作的 `--help` 查看完整参数。

## 检查策略决策

可以传入原始操作字符串，也可以使用结构化字段：

```bash
cosh-cli audit check --action-string "pkg install nginx"
cosh-cli audit check --subsystem pkg --operation install --target nginx
cosh-cli audit log --session abc123 --since 2h --limit 50
```

`--action` 仍是 `--action-string` 的别名。结构化检查必须提供 `--subsystem` 和 `--operation`；`--target` 以及成对的 `--arg-key`、`--arg-value` 参数可选。

## 查询和导出事件

```bash
cosh-cli audit status
cosh-cli audit events --since 2h --event approval.requested,approval.resolved --limit 100
cosh-cli audit trace 7fa4c0b0-0000-4000-8000-000000000001
cosh-cli audit export --since 2h --identity session-123 --output ./audit-incident
cosh-cli audit prune --dry-run
```

`--since` 可以使用 `30s`、`5m`、`2h` 或 `1d` 等时长，也可以使用 RFC 3339 时间戳；`--until` 使用 RFC 3339 时间戳。`events` 和 `export` 还支持重复或逗号分隔的 `--event`、`--component`、`--outcome` 过滤，以及 `--identity` 和 `--schema v1|legacy_v0`；`events` 和 `trace` 支持用不透明的 `--cursor` 获取下一页。

在 `cosh-shell` 中，`/audit status`、`/audit trace current` 和 `/audit export current <dir>` 是相同操作的有界入口。

导出目录包含 `events.jsonl`、`summary.json`、`manifest.json` 和 `SHA256SUMS`。导出内容会脱敏并以原子方式发布；`--force` 只能替换带有有效 cosh 审计清单的目录。版本 1 只支持预览保留计划，因此 `audit prune` 必须带 `--dry-run`，不会删除数据。

## 策略命令

```bash
cosh-cli audit policy show
cosh-cli audit policy list
cosh-cli audit policy validate ./audit.toml
cosh-cli audit policy explain "cat /etc/os-release"
```

策略加载器也兼容旧的 `cosh-cli audit check --action ...` 写法。策略位置、审计设置和存储覆盖项见[配置](../configuration.md)。系统审计设置优先于用户设置；项目中的审计表会被忽略。
