"""Data models for the PII checker."""

from enum import StrEnum
from typing import Any

from pydantic import BaseModel, Field


class Verdict(StrEnum):
    """Aggregated PII scan verdict."""

    PASS = "pass"
    WARN = "warn"
    DENY = "deny"
    ERROR = "error"


class PiiSeverity(StrEnum):
    """Finding severity used by policy consumers."""

    WARN = "warn"
    DENY = "deny"


class PiiCategory(StrEnum):
    """High-level finding category."""

    PERSONAL_DATA = "personal_data"
    CREDENTIAL = "credential"
    CUSTOM = "custom"


class PiiFinding(BaseModel):
    """Single PII or credential finding."""

    type: str
    category: str
    severity: str
    confidence: float
    evidence_redacted: str
    span: tuple[int, int]
    metadata: dict[str, Any] = Field(default_factory=dict)
    raw_evidence: str | None = None

    def to_dict(self, *, include_raw_evidence: bool = False) -> dict[str, Any]:
        """Return the fixed finding schema."""
        data: dict[str, Any] = {
            "type": self.type,
            "category": self.category,
            "severity": self.severity,
            "confidence": round(self.confidence, 3),
            "evidence_redacted": self.evidence_redacted,
            "span": {"start": self.span[0], "end": self.span[1]},
            "metadata": dict(self.metadata),
        }
        if include_raw_evidence and self.raw_evidence is not None:
            data["raw_evidence"] = self.raw_evidence
        return data


class PiiScanResult(BaseModel):
    """Structured PII scan result."""

    ok: bool
    verdict: str
    summary: dict[str, Any]
    findings: list[PiiFinding]
    elapsed_ms: int
    include_raw_evidence: bool = False
    redacted_text: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Return the fixed public output schema."""
        data: dict[str, Any] = {
            "ok": self.ok,
            "verdict": self.verdict,
            "summary": dict(self.summary),
            "findings": [
                finding.to_dict(include_raw_evidence=self.include_raw_evidence)
                for finding in self.findings
            ],
            "elapsed_ms": self.elapsed_ms,
        }
        if self.redacted_text is not None:
            data["redacted_text"] = self.redacted_text
        return data
