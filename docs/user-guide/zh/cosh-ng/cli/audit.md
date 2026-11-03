# 安全审计

[English](../../../en/cosh-ng/cli/audit.md)

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

审计默认在每用户范围内为 active 和 closed v1 segment 合计提供 100 MiB 预算。
写入准入通过共享锁计入缓冲字节，持续写入与多个 writer 仍受同一个上限约束。

生命周期证据损坏、未知或超过有界扫描限制时，retention 返回 `Blocked`，保留
证据，不刷新上次成功回收时间，并允许重试。Session start/end 事件只按同一个
segment 内的实际行序配对。现有事件缺少 invocation identity，因此跨 segment
未配对的 Session 证据会保守保留；长会话跨越轮转后可能无法自动回收。

Core 会话文件使用独立的每用户 100 MiB 预算，包含 staging。原子替换会在写入前
同时计算旧文件和新 staging 文件；峰值空间不足时保留旧文件并拒绝提交。
Core/Shell 诊断日志共享 20 MiB，并保留今天及前两个 UTC 日期。存储所有权检查
会在写入或清理前拒绝不安全条目。

## 策略命令

```bash
cosh-cli audit policy show
cosh-cli audit policy list
cosh-cli audit policy validate ./audit.toml
cosh-cli audit policy explain "cat /etc/os-release"
```

策略加载器也兼容旧的 `cosh-cli audit check --action ...` 写法。策略位置、审计设置和存储覆盖项见[配置](../configuration.md)。系统审计设置优先于用户设置；项目中的审计表会被忽略。
