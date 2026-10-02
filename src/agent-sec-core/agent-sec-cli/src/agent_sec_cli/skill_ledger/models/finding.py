"""NormalizedFinding — universal contract for scanner outputs.

All scanner results are normalised into this structure before being
stored in ``ScanEntry.findings``.  See design doc §2 *NormalizedFinding*.
"""

from typing import Any

from pydantic import BaseModel, Field, field_validator

# Allowed severity levels (aligned with scanStatus aggregation).
VALID_LEVELS = frozenset({"deny", "warn", "pass"})


class NormalizedFinding(BaseModel):
    """A single finding produced by any scanner, in canonical form.

    Attributes:
        rule:     Rule / check identifier (e.g. ``"dangerous-exec"``).
        level:    ``"deny"`` | ``"warn"`` | ``"pass"``.
        message:  Human-readable description of the finding.
        file:     (Optional) Affected file path relative to skill_dir.
        line:     (Optional) Line number within *file*.
        metadata: (Optional) Scanner-specific extra data.
    """

    rule: str
    level: str  # "deny" | "warn" | "pass"
    message: str
    file: str | None = None
    line: int | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)

    @field_validator("level")
    @classmethod
    def validate_level(cls, v: str) -> str:
        """Normalise and validate the severity level."""
        v = str(v).lower()
        if v not in VALID_LEVELS:
            raise ValueError(
                f"Invalid finding level {v!r}; expected one of {sorted(VALID_LEVELS)}"
            )
        return v

    def to_findings_dict(self) -> dict[str, Any]:
        """Return a plain dict suitable for ``ScanEntry.findings``."""
        d: dict[str, Any] = {
            "rule": self.rule,
            "level": self.level,
            "message": self.message,
        }
        if self.file is not None:
            d["file"] = self.file
        if self.line is not None:
            d["line"] = self.line
        if self.metadata:
            d["metadata"] = self.metadata
        return d
