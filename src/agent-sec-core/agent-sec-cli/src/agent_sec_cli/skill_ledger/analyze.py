"""Read-only orchestration for Skill content scanners."""

import os
import stat
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli import __version__ as AGENT_SEC_VERSION
from agent_sec_cli.skill_ledger.scanner.builtins.dispatcher import (
    run_builtin_scanner,
)
from agent_sec_cli.skill_ledger.scanner.names import (
    CODE_SCANNER_NAME,
    STATIC_SCANNER_NAME,
)
from agent_sec_cli.skill_ledger.scanner.skill_code_scanner import (
    SCANNER_VERSION as CODE_SCANNER_VERSION,
)
from agent_sec_cli.skill_ledger.scanner.skill_code_scanner import (
    scan_skill_code,
)

SCHEMA_VERSION = "1"
MAX_FILES = 2_000
MAX_TOTAL_BYTES = 50 * 1024 * 1024
MAX_DIRECTORY_DEPTH = 32

_SKIPPED_DIRS = frozenset(
    {
        ".git",
        ".skill-meta",
        ".pytest_cache",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
    }
)
_CODE_ERROR_RULES = frozenset({"code-scanner-error"})
_STATIC_ERROR_RULES = frozenset(
    {
        "file-decode-error",
        "file-read-error",
        "large-file-skipped",
        "scanner-rule-error",
    }
)
_STATUS_ORDER = {"pass": 0, "warn": 1, "deny": 2}


def analyze_skill(skill_dir: str | Path) -> tuple[dict[str, Any], int]:
    """Analyze current Skill content without updating Skill Ledger state."""
    raw_root = Path(skill_dir).expanduser()
    input_error = _validate_root(raw_root)
    if input_error is not None:
        return _error_payload(input_error), 2

    root = raw_root.resolve()
    coverage_errors = _inspect_tree(root)
    if coverage_errors:
        return _coverage_payload(coverage_errors), 1

    scanners = [
        _run_code_scanner(root),
        _run_static_scanner(root),
    ]
    coverage_complete = all(bool(scanner["coverage_complete"]) for scanner in scanners)
    status = _aggregate_status(scanners) if coverage_complete else "error"
    payload = {
        "schema_version": SCHEMA_VERSION,
        "engine_version": AGENT_SEC_VERSION,
        "status": status,
        "coverage_complete": coverage_complete,
        "scanners": scanners,
        "errors": [],
    }
    return _sanitize_payload(payload, root), 0 if coverage_complete else 1


def _validate_root(root: Path) -> dict[str, Any] | None:
    if root.is_symlink():
        return _error("root-symlink", "Skill root must not be a symbolic link.")
    if not root.exists():
        return _error("root-not-found", "Skill root does not exist.")
    if not root.is_dir():
        return _error("root-not-directory", "Skill root is not a directory.")
    if not (root / "SKILL.md").is_file() or (root / "SKILL.md").is_symlink():
        return _error(
            "skill-manifest-missing",
            "Skill root must contain a regular SKILL.md file.",
            file="SKILL.md",
        )
    return None


def _inspect_tree(root: Path) -> list[dict[str, Any]]:
    errors: list[dict[str, Any]] = []
    file_count = 0
    total_bytes = 0

    def on_error(exc: OSError) -> None:
        errors.append(
            _error("directory-read-error", "Skill directory is not readable.")
        )

    for current_root, dirnames, filenames in os.walk(
        root, followlinks=False, onerror=on_error
    ):
        current = Path(current_root)
        relative_dir = current.relative_to(root)
        depth = len(relative_dir.parts)
        if depth > MAX_DIRECTORY_DEPTH:
            errors.append(
                _error(
                    "directory-depth-limit",
                    f"Skill directory depth exceeds {MAX_DIRECTORY_DEPTH}.",
                    file=relative_dir.as_posix(),
                    metadata={"max_directory_depth": MAX_DIRECTORY_DEPTH},
                )
            )
            dirnames[:] = []
            continue

        dirnames[:] = sorted(
            name
            for name in dirnames
            if name not in _SKIPPED_DIRS and not (current / name).is_symlink()
        )
        for filename in sorted(filenames):
            path = current / filename
            rel_path = path.relative_to(root).as_posix()
            try:
                stat_result = path.lstat()
            except OSError:
                errors.append(
                    _error(
                        "file-stat-error",
                        "Skill file metadata could not be read.",
                        file=rel_path,
                    )
                )
                continue
            mode = stat_result.st_mode
            if stat.S_ISLNK(mode):
                continue
            if not stat.S_ISREG(mode):
                errors.append(
                    _error(
                        "unsupported-file-type",
                        "Skill contains a non-regular file that cannot be scanned.",
                        file=rel_path,
                    )
                )
                continue
            file_count += 1
            total_bytes += stat_result.st_size

    if file_count > MAX_FILES:
        errors.append(
            _error(
                "file-count-limit",
                f"Skill contains more than {MAX_FILES} regular files.",
                metadata={"max_files": MAX_FILES, "file_count": file_count},
            )
        )
    if total_bytes > MAX_TOTAL_BYTES:
        errors.append(
            _error(
                "total-size-limit",
                f"Skill content exceeds {MAX_TOTAL_BYTES} bytes.",
                metadata={
                    "max_total_bytes": MAX_TOTAL_BYTES,
                    "total_bytes": total_bytes,
                },
            )
        )
    return _sort_errors(errors)


def _run_code_scanner(root: Path) -> dict[str, Any]:
    try:
        raw_findings = scan_skill_code(root)
    except Exception:
        return _scanner_error(
            CODE_SCANNER_NAME,
            CODE_SCANNER_VERSION,
            _error("scanner-error", "code-scanner failed to complete."),
        )
    return _build_scanner_result(
        CODE_SCANNER_NAME,
        CODE_SCANNER_VERSION,
        raw_findings,
        _CODE_ERROR_RULES,
    )


def _run_static_scanner(root: Path) -> dict[str, Any]:
    try:
        result = run_builtin_scanner(STATIC_SCANNER_NAME, root)
    except Exception:
        return _scanner_error(
            STATIC_SCANNER_NAME,
            "unknown",
            _error("scanner-error", "static-scanner failed to complete."),
        )
    return _build_scanner_result(
        result.scanner,
        result.version,
        result.findings,
        _STATIC_ERROR_RULES,
    )


def _build_scanner_result(
    name: str,
    version: str,
    raw_findings: list[dict[str, Any]],
    error_rules: frozenset[str],
) -> dict[str, Any]:
    findings: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []
    for finding in raw_findings:
        if str(finding.get("rule", "")) in error_rules:
            errors.append(_finding_error(finding))
        else:
            findings.append(finding)
    coverage_complete = not errors
    return {
        "name": name,
        "version": version,
        "status": _finding_status(findings) if coverage_complete else "error",
        "coverage_complete": coverage_complete,
        "findings": _sort_findings(findings),
        "errors": _sort_errors(errors),
    }


def _scanner_error(name: str, version: str, error: dict[str, Any]) -> dict[str, Any]:
    return {
        "name": name,
        "version": version,
        "status": "error",
        "coverage_complete": False,
        "findings": [],
        "errors": [error],
    }


def _finding_error(finding: dict[str, Any]) -> dict[str, Any]:
    metadata = dict(finding.get("metadata") or {})
    metadata.pop("error", None)
    return _error(
        str(finding.get("rule", "scanner-error")),
        str(finding.get("message", "Scanner could not complete coverage.")),
        file=str(finding["file"]) if finding.get("file") else None,
        metadata=metadata,
    )


def _finding_status(findings: list[dict[str, Any]]) -> str:
    levels = [str(finding.get("level", "pass")).lower() for finding in findings]
    if "deny" in levels:
        return "deny"
    if "warn" in levels:
        return "warn"
    return "pass"


def _aggregate_status(scanners: list[dict[str, Any]]) -> str:
    return max(
        (str(scanner["status"]) for scanner in scanners),
        key=lambda status: _STATUS_ORDER.get(status, 0),
        default="pass",
    )


def _coverage_payload(errors: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "engine_version": AGENT_SEC_VERSION,
        "status": "error",
        "coverage_complete": False,
        "scanners": [],
        "errors": errors,
    }


def _error_payload(error: dict[str, Any]) -> dict[str, Any]:
    return _coverage_payload([error])


def _error(
    code: str,
    message: str,
    *,
    file: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    item: dict[str, Any] = {"code": code, "message": message}
    if file:
        item["file"] = file
    if metadata:
        item["metadata"] = metadata
    return item


def _sort_findings(findings: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return sorted(
        findings,
        key=lambda item: (
            str(item.get("file", "")),
            int(item.get("line") or 0),
            str(item.get("rule", "")),
        ),
    )


def _sort_errors(errors: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return sorted(
        errors,
        key=lambda item: (str(item.get("file", "")), str(item.get("code", ""))),
    )


def _sanitize_payload(payload: dict[str, Any], root: Path) -> dict[str, Any]:
    def is_safe_redaction_root(value: str) -> bool:
        path = Path(value)
        return path.is_absolute() and path != Path(path.anchor)

    sensitive_roots = {str(root), str(Path.home()), tempfile.gettempdir()}
    sensitive_roots.update(
        value
        for name in (
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "TEMP",
            "TMP",
        )
        if (value := os.environ.get(name))
    )
    sensitive_roots = {
        value for value in sensitive_roots if is_safe_redaction_root(value)
    }

    def sanitize(value: Any) -> Any:
        if isinstance(value, dict):
            return {str(key): sanitize(item) for key, item in value.items()}
        if isinstance(value, list):
            return [sanitize(item) for item in value]
        if isinstance(value, str):
            cleaned = value
            for sensitive in sorted(sensitive_roots, key=len, reverse=True):
                if sensitive:
                    cleaned = cleaned.replace(sensitive, "<redacted>")
            return cleaned
        return value

    return sanitize(payload)
