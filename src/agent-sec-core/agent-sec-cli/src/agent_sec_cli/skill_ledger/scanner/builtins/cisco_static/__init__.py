"""Cisco static-only skill scanner adapter."""

from agent_sec_cli.skill_ledger.scanner.builtins.cisco_static.scanner import (
    SCANNER_NAME,
    SCANNER_VERSION,
    scan_skill,
)

__all__ = ["SCANNER_NAME", "SCANNER_VERSION", "scan_skill"]
