"""Detector orchestration for the PII checker."""

import time
from collections import Counter
from collections.abc import Sequence

from agent_sec_cli.pii_checker.detectors.base import PiiCandidate, PiiDetector
from agent_sec_cli.pii_checker.detectors.custom import CustomPiiDetector
from agent_sec_cli.pii_checker.detectors.regex import RegexPiiDetector
from agent_sec_cli.pii_checker.models import (
    PiiFinding,
    PiiScanResult,
    PiiSeverity,
    Verdict,
)
from agent_sec_cli.pii_checker.redactor import redact_text, redact_value

DEFAULT_MAX_BYTES = 1_048_576
LOW_CONFIDENCE_THRESHOLD = 0.5
ALLOWED_SOURCES = {
    "user_input",
    "model_input",
    "tool_input",
    "tool_output",
    "model_output",
    "observability",
    "manual",
    "unknown",
}


def _decode_utf8_prefix(data: bytes) -> str:
    """Decode bytes after backing off a partial UTF-8 character at the end."""
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        if exc.reason != "unexpected end of data":
            raise
        return data[: exc.start].decode("utf-8")


def _limit_text(text: str, max_bytes: int | None) -> tuple[str, bool, int]:
    """Limit text by encoded byte length when a byte limit is configured."""
    encoded = text.encode("utf-8")
    if max_bytes is None:
        return text, False, len(encoded)
    if len(encoded) <= max_bytes:
        return text, False, len(encoded)
    trimmed = _decode_utf8_prefix(encoded[:max_bytes])
    return trimmed, True, max_bytes


def _aggregate_verdict(findings: list[PiiFinding]) -> str:
    """Aggregate findings into pass/warn/deny."""
    if any(finding.severity == PiiSeverity.DENY.value for finding in findings):
        return Verdict.DENY.value
    if findings:
        return Verdict.WARN.value
    return Verdict.PASS.value


class PiiScanner:
    """PII scanner that orchestrates one or more detector implementations."""

    def __init__(self, detectors: Sequence[PiiDetector] | None = None) -> None:
        """Create a scanner with built-in regex detection unless overridden."""
        self._custom_detector: CustomPiiDetector | None = None
        if detectors is not None:
            self._detectors = list(detectors)
        else:
            self._custom_detector = CustomPiiDetector()
            self._detectors = [RegexPiiDetector(), self._custom_detector]

    def scan(
        self,
        text: str,
        *,
        source: str = "unknown",
        include_low_confidence: bool = False,
        raw_evidence: bool = False,
        redact_output: bool = False,
        max_bytes: int | None = None,
    ) -> PiiScanResult:
        """Scan text and return a fixed-schema result."""
        started = time.perf_counter()
        normalized_source = source if source in ALLOWED_SOURCES else "unknown"
        if max_bytes is not None and max_bytes <= 0:
            raise ValueError("max_bytes must be greater than zero")
        limited_text, truncated, bytes_scanned = _limit_text(text, max_bytes)

        candidates = self._detect(limited_text)
        findings = self._build_findings(
            candidates,
            include_low_confidence=include_low_confidence,
            raw_evidence=raw_evidence,
        )
        verdict = _aggregate_verdict(findings)
        summary = self._build_summary(
            findings,
            source=normalized_source,
            bytes_scanned=bytes_scanned,
            truncated=truncated,
        )
        if self._custom_detector is not None:
            summary["custom_rules"] = self._custom_detector.summary()
        elapsed_ms = int((time.perf_counter() - started) * 1000)

        return PiiScanResult(
            ok=True,
            verdict=verdict,
            summary=summary,
            findings=findings,
            elapsed_ms=elapsed_ms,
            include_raw_evidence=raw_evidence,
            redacted_text=(
                redact_text(limited_text, findings) if redact_output else None
            ),
        )

    def _detect(self, text: str) -> list[PiiCandidate]:
        """Run configured detectors and return deduplicated raw candidates."""
        candidates: list[PiiCandidate] = []
        for detector in self._detectors:
            detector_name = getattr(detector, "name", "unknown")
            detector_engine = getattr(detector, "engine", detector_name)
            for candidate in detector.detect(text):
                if candidate.detector != "unknown" and candidate.engine != "unknown":
                    candidates.append(candidate)
                    continue
                candidates.append(
                    candidate.model_copy(
                        update={
                            "detector": (
                                detector_name
                                if candidate.detector == "unknown"
                                else candidate.detector
                            ),
                            "engine": (
                                detector_engine
                                if candidate.engine == "unknown"
                                else candidate.engine
                            ),
                        }
                    )
                )
        return self._dedupe(candidates)

    def _dedupe(self, candidates: list[PiiCandidate]) -> list[PiiCandidate]:
        """Drop exact type-and-span duplicates while preserving overlaps."""
        ordered = sorted(
            candidates,
            key=lambda item: (
                item.severity != PiiSeverity.DENY.value,
                -item.confidence,
                item.span[0],
                -(item.span[1] - item.span[0]),
                item.pii_type,
            ),
        )
        kept: list[PiiCandidate] = []
        seen: set[tuple[str, tuple[int, int]]] = set()
        for candidate in ordered:
            identity = (candidate.pii_type, candidate.span)
            if identity in seen:
                continue
            seen.add(identity)
            kept.append(candidate)
        return sorted(
            kept,
            key=lambda item: (item.span[0], item.span[1], item.pii_type),
        )

    def _build_findings(
        self,
        candidates: list[PiiCandidate],
        *,
        include_low_confidence: bool,
        raw_evidence: bool,
    ) -> list[PiiFinding]:
        """Convert candidates to public findings."""
        findings: list[PiiFinding] = []
        for candidate in candidates:
            if (
                not include_low_confidence
                and candidate.confidence < LOW_CONFIDENCE_THRESHOLD
            ):
                continue
            metadata = dict(candidate.metadata)
            metadata.setdefault("detector", candidate.detector)
            metadata.setdefault("engine", candidate.engine)
            findings.append(
                PiiFinding(
                    type=candidate.pii_type,
                    category=candidate.category,
                    severity=candidate.severity,
                    confidence=candidate.confidence,
                    evidence_redacted=redact_value(
                        candidate.value,
                        candidate.pii_type,
                        category=candidate.category,
                    ),
                    span=candidate.span,
                    metadata=metadata,
                    raw_evidence=candidate.value if raw_evidence else None,
                )
            )
        return findings

    def _build_summary(
        self,
        findings: list[PiiFinding],
        *,
        source: str,
        bytes_scanned: int,
        truncated: bool,
    ) -> dict[str, object]:
        """Build aggregate summary data."""
        by_type = Counter(finding.type for finding in findings)
        by_category = Counter(finding.category for finding in findings)
        by_severity = Counter(finding.severity for finding in findings)
        return {
            "total": len(findings),
            "by_type": dict(sorted(by_type.items())),
            "by_category": dict(sorted(by_category.items())),
            "by_severity": dict(sorted(by_severity.items())),
            "source": source,
            "bytes_scanned": bytes_scanned,
            "truncated": truncated,
        }


def scan_text(
    text: str,
    *,
    detectors: Sequence[PiiDetector] | None = None,
    source: str = "unknown",
    include_low_confidence: bool = False,
    raw_evidence: bool = False,
    redact_output: bool = False,
    max_bytes: int | None = None,
) -> PiiScanResult:
    """Convenience function for one-off scans."""
    return PiiScanner(detectors=detectors).scan(
        text,
        source=source,
        include_low_confidence=include_low_confidence,
        raw_evidence=raw_evidence,
        redact_output=redact_output,
        max_bytes=max_bytes,
    )
