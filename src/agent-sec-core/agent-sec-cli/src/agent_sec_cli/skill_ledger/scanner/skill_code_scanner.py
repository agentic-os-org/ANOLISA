"""Skill directory adapter for the independent code_scanner component."""

import os
from pathlib import Path
from typing import Any

from agent_sec_cli import __version__ as AGENT_SEC_VERSION
from agent_sec_cli.code_scanner.models import (
    Finding,
    Language,
    ScanResult,
    Verdict,
)
from agent_sec_cli.code_scanner.scanner import scan
from agent_sec_cli.skill_ledger.scanner.names import CODE_SCANNER_NAME

SCANNER_NAME = CODE_SCANNER_NAME
SCANNER_VERSION = AGENT_SEC_VERSION

_ERROR_RULE = "code-scanner-error"
_EXCLUDED_DIRS = frozenset(
    {
        ".skill-meta",
        ".git",
        "node_modules",
        "__pycache__",
        ".pytest_cache",
        "dist",
        "build",
    }
)
_MAX_EVIDENCE_ITEMS = 5
_MAX_EVIDENCE_CHARS = 500
_MAX_CODE_FILE_BYTES = 1024 * 1024


def scan_skill_code(skill_dir: str | Path) -> list[dict[str, Any]]:
    """Scan code files in *skill_dir* and return findings-array dicts."""
    root = Path(skill_dir).resolve()
    findings: list[dict[str, Any]] = []

    for path, language in iter_code_files(root):
        findings.extend(_scan_file(root, path, language))

    return findings


def iter_code_files(skill_dir: str | Path) -> list[tuple[Path, Language]]:
    """Return supported code files with their detected language."""
    root = Path(skill_dir).resolve()
    files: list[tuple[Path, Language]] = []

    for current_root, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(current_root)
        rel_root = current.relative_to(root)
        if any(part in _EXCLUDED_DIRS for part in rel_root.parts):
            dirnames[:] = []
            continue

        dirnames[:] = sorted(
            dirname
            for dirname in dirnames
            if dirname not in _EXCLUDED_DIRS and not (current / dirname).is_symlink()
        )

        for filename in sorted(filenames):
            entry = current / filename
            if entry.is_symlink() or not entry.is_file():
                continue

            language = detect_language(entry)
            if language is not None:
                files.append((entry, language))

    return files


def detect_language(path: Path) -> Language | None:
    """Detect the code_scanner language for a Skill file path."""
    suffix = path.suffix.lower()
    if suffix == ".py":
        return Language.PYTHON
    if suffix == ".sh":
        return Language.BASH
    if suffix:
        return None
    return _language_from_shebang(path)


def _language_from_shebang(path: Path) -> Language | None:
    try:
        with path.open("rb") as fh:
            first_line = fh.readline(256)
    except OSError:
        return None

    if not first_line.startswith(b"#!"):
        return None

    shebang = first_line[2:].decode("utf-8", errors="ignore").strip()
    for token in shebang.split():
        name = Path(token).name.lower()
        if name.startswith("python"):
            return Language.PYTHON
        if name in {"sh", "bash", "zsh", "dash"}:
            return Language.BASH

    return None


def _scan_file(root: Path, path: Path, language: Language) -> list[dict[str, Any]]:
    try:
        size = path.stat().st_size
    except OSError as exc:
        return [_error_finding(root, path, language, f"failed to stat file: {exc}")]

    if size > _MAX_CODE_FILE_BYTES:
        return [
            _error_finding(
                root,
                path,
                language,
                f"file too large to scan: {size} bytes > {_MAX_CODE_FILE_BYTES} bytes",
                {"max_file_bytes": _MAX_CODE_FILE_BYTES},
            )
        ]

    try:
        code = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        return [_error_finding(root, path, language, f"failed to read file: {exc}")]

    if not code.strip():
        return []

    try:
        result = scan(code, language)
    except Exception as exc:
        return [
            _error_finding(
                root,
                path,
                language,
                f"code-scanner raised unexpected error: {type(exc).__name__}: {exc}",
            )
        ]
    if not result.ok or result.verdict == Verdict.ERROR:
        return [_error_finding(root, path, language, result.summary)]

    return [
        _finding_to_dict(root, path, result, finding) for finding in result.findings
    ]


def _finding_to_dict(
    root: Path,
    path: Path,
    result: ScanResult,
    finding: Finding,
) -> dict[str, Any]:
    message = finding.desc_zh or finding.desc_en
    return {
        "rule": finding.rule_id,
        "level": finding.severity.value,
        "message": message,
        "file": _relative_path(root, path),
        "metadata": {
            "source": "code-scanner",
            "language": result.language.value,
            "engine_version": result.engine_version,
            "elapsed_ms": result.elapsed_ms,
            "evidence": _truncate_evidence(finding.evidence),
        },
    }


def _error_finding(
    root: Path,
    path: Path,
    language: Language,
    reason: str,
    metadata_extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    metadata: dict[str, Any] = {
        "source": "code-scanner",
        "language": language.value,
        "error": reason,
    }
    if metadata_extra:
        metadata.update(metadata_extra)

    return {
        "rule": _ERROR_RULE,
        "level": "warn",
        "message": "code-scanner could not complete this file scan",
        "file": _relative_path(root, path),
        "metadata": metadata,
    }


def _relative_path(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def _truncate_evidence(evidence: list[str]) -> list[str]:
    truncated: list[str] = []
    for item in evidence[:_MAX_EVIDENCE_ITEMS]:
        text = str(item)
        if len(text) > _MAX_EVIDENCE_CHARS:
            text = text[:_MAX_EVIDENCE_CHARS] + "...<truncated>"
        truncated.append(text)
    return truncated
