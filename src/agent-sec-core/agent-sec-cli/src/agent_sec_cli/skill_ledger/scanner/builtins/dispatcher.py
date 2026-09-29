"""Dispatcher for built-in skill-ledger scanners."""

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.scanner.builtins.cisco_static.scanner import (
    SCANNER_NAME,
    SCANNER_VERSION,
    scan_skill,
)
from agent_sec_cli.skill_ledger.scanner.names import validate_scanner_name


@dataclass(frozen=True)
class BuiltinScanResult:
    """Result returned by a built-in scanner adapter."""

    scanner: str
    version: str
    findings: list[dict[str, Any]]


class BuiltinScannerError(RuntimeError):
    """Raised when a built-in scanner cannot complete a scan."""


def run_builtin_scanner(
    scanner_name: str,
    skill_dir: str | Path,
    options: dict[str, Any] | None = None,
) -> BuiltinScanResult:
    """Run a built-in scanner by registry name."""
    canonical_name = validate_scanner_name(scanner_name)
    if canonical_name == SCANNER_NAME:
        try:
            findings = scan_skill(skill_dir, options=options)
        except Exception as exc:
            raise BuiltinScannerError(
                f"Built-in scanner {canonical_name!r} failed to initialize or run: {exc}"
            ) from exc
        return BuiltinScanResult(
            scanner=SCANNER_NAME,
            version=SCANNER_VERSION,
            findings=findings,
        )
    raise ValueError(f"Unknown built-in scanner: {scanner_name}")
