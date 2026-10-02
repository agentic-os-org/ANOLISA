# 命令行输出格式

[English](../../../en/user-entrypoint/cosh-ng/output-format.md)

每个已解析的`cosh-cli`操作都会返回一个JSON信封。先解析信封，再处理操作对应的`data`或`error`对象。

## 成功响应

```json
{
  "ok": true,
  "data": { "packages": [] },
  "meta": {
    "subsystem": "pkg",
    "duration_ms": 342,
    "distro": "alinux",
    "dry_run": false
  }
}
```

## 失败响应

```json
{
  "ok": false,
  "error": {
    "code": "PkgNotFound",
    "message": "package 'nginx-extra' not found",
    "recoverable": false,
    "hint": "Try 'cosh pkg search nginx' to check availability",
    "subsystem": "pkg"
  },
  "meta": {
    "subsystem": "pkg",
    "duration_ms": 120,
    "distro": "ubuntu",
    "dry_run": false
  }
}
```

## 字段

| 字段 | 含义 |
|---|---|
| `ok` | 成功为`true`，失败为`false`。 |
| `data` | 操作结果，成功时存在。 |
| `error` | 失败详情：`code`、`message`、`recoverable`、可选的`hint`和`details`，以及`subsystem`。 |
| `meta.subsystem` | `pkg`、`svc`、`checkpoint`或`audit`。 |
| `meta.duration_ms` | 操作耗时，单位为毫秒。 |
| `meta.distro` | 可用时返回检测到的平台ID。 |
| `meta.dry_run` | `true`表示预览，实际操作未执行。 |
| `meta.warning` | 随结果返回的可选警告。 |

错误码是稳定字符串，例如`PkgNotFound`、`UnsupportedDistro`、`SvcNotFound`、`CheckpointNotFound`、`AuditDenied`、`Timeout`和`PermissionDenied`；请使用`error.code`和`error.hint`，不要解析错误消息文本。

## 退出码和Agent处理

- 退出码`0`表示`ok: true`；退出码`1`表示`ok: false`。
- 失败时先检查`error.recoverable`再决定是否重试，并在有`error.hint`时展示它。
- `meta.dry_run`为`true`时，只报告预览结果，不要声称主机已发生变化。
