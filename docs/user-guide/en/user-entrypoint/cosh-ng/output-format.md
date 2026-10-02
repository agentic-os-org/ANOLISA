# CLI Output Format

[中文版](../../../zh/user-entrypoint/cosh-ng/output-format.md)

Each parsed `cosh-cli` action returns a JSON envelope. Parse the envelope first, then handle the operation-specific `data` or `error` object.

## Success

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

## Failure

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

## Fields

| Field | Meaning |
|---|---|
| `ok` | `true` for success, `false` for failure. |
| `data` | Operation result; present on success. |
| `error` | Failure details: `code`, `message`, `recoverable`, optional `hint` and `details`, and `subsystem`. |
| `meta.subsystem` | `pkg`, `svc`, `checkpoint`, or `audit`. |
| `meta.duration_ms` | Elapsed operation time in milliseconds. |
| `meta.distro` | Detected platform ID when available. |
| `meta.dry_run` | `true` means the operation was previewed, not applied. |
| `meta.warning` | Optional warning accompanying the result. |

Error codes are stable strings such as `PkgNotFound`, `UnsupportedDistro`, `SvcNotFound`, `CheckpointNotFound`, `AuditDenied`, `Timeout`, and `PermissionDenied`; use `error.code` and `error.hint` instead of parsing the message.

## Exit codes and Agent handling

- Exit code `0` means `ok: true`; exit code `1` means `ok: false`.
- For a failure, inspect `error.recoverable` before retrying and show `error.hint` when present.
- When `meta.dry_run` is `true`, report the preview without claiming that the host changed.
