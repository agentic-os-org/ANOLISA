"""Detector interfaces for the PII checker."""

from typing import Protocol

from pydantic import BaseModel, Field


class PiiCandidate(BaseModel):
    """Raw detector output before scanner-level filtering and redaction."""

    pii_type: str
    category: str
    severity: str
    confidence: float
    value: str
    span: tuple[int, int]
    metadata: dict[str, object] = Field(default_factory=dict)
    detector: str = "unknown"
    engine: str = "unknown"


class PiiDetector(Protocol):
    """Protocol for regex, rules, or model-backed PII detectors."""

    name: str
    engine: str

    def detect(self, text: str) -> list[PiiCandidate]:
        """Return raw PII candidates for *text*."""
        pass
