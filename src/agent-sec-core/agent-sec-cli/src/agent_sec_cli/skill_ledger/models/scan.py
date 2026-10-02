"""ScanEntry model and scanStatus aggregation logic."""

from typing import Any

from agent_sec_cli.skill_ledger.utils import utc_now_iso
from pydantic import BaseModel, Field


class ScanEntry(BaseModel):
    """A single scanner's result within a manifest's ``scans[]`` array.

    Attributes:
        scanner:    Scanner identifier (e.g. ``"skill-vetter"``).
        version:    Scanner version for reproducibility.
        status:     Individual result: ``pass`` | ``warn`` | ``deny``.
        findings:   Scanner-specific structured findings.
        scannedAt:  ISO-8601 timestamp of when the scan was performed.
    """

    scanner: str = "skill-vetter"
    version: str = "0.1.0"
    status: str = "pass"
    findings: list[dict[str, Any]] = Field(default_factory=list)
    scannedAt: str = Field(default_factory=utc_now_iso)


# Severity ordering: higher index = more severe.
_SEVERITY_ORDER: dict[str, int] = {
    "none": 0,
    "pass": 1,
    "warn": 2,
    "deny": 3,
}


def aggregate_scan_status(scans: list[ScanEntry]) -> str:
    """Compute the aggregate ``scanStatus`` from a list of scan entries.

    Returns the **most severe** status across all entries.
    An empty list yields ``"none"``.
    """
    if not scans:
        return "none"
    worst = max(scans, key=lambda s: _SEVERITY_ORDER.get(s.status, 0))
    return worst.status
