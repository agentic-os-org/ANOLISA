"""SignedManifest Pydantic model with canonical JSON and manifestHash computation."""

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.models.scan import ScanEntry
from agent_sec_cli.skill_ledger.utils import utc_now_iso
from pydantic import BaseModel, Field, model_validator


class ManifestSignature(BaseModel):
    """Digital signature block embedded in a SignedManifest."""

    algorithm: str = "ed25519"
    value: str = ""  # base64-encoded signature
    keyFingerprint: str = ""  # "sha256:<hex>"


class UserDecision(BaseModel):
    """User decision bound to this specific signed manifest version."""

    action: str
    targetVersionId: str | None = None
    reason: str | None = None

    @model_validator(mode="after")
    def _validate_action(self) -> "UserDecision":
        allowed = {"allow", "always_allow", "block", "rollback"}
        if self.action not in allowed:
            raise ValueError(
                f"userDecision.action must be one of {sorted(allowed)}, got {self.action!r}"
            )
        if self.action == "rollback" and not self.targetVersionId:
            raise ValueError("userDecision.targetVersionId is required for rollback")
        if self.action != "rollback" and self.targetVersionId is not None:
            raise ValueError(
                "userDecision.targetVersionId is only allowed for rollback"
            )
        return self


class SignedManifest(BaseModel):
    """The canonical signed manifest stored in ``.skill-meta/``.

    See the design doc §1 *SignedManifest 结构* for field semantics.
    """

    version: int = 1
    versionId: str = "v000001"
    previousVersionId: str | None = None

    skillName: str = ""

    fileHashes: dict[str, str] = Field(default_factory=dict)

    scans: list[ScanEntry] = Field(default_factory=list)
    scanStatus: str = "none"  # none | pass | warn | deny

    userDecision: UserDecision | None = None

    policy: str = "warning"  # warning | allow | block

    createdAt: str = Field(default_factory=utc_now_iso)
    updatedAt: str = Field(default_factory=utc_now_iso)

    # ── Anti-tamper fields ──────────────────────────────────────
    manifestHash: str = ""

    previousManifestSignature: str | None = None

    signature: ManifestSignature | None = None

    # ------------------------------------------------------------------
    # Canonical JSON
    # ------------------------------------------------------------------

    def _signable_dict(self) -> dict[str, Any]:
        """Return all fields **except** ``manifestHash`` and ``signature``.

        This is the data covered by the hash and signature.
        """
        d = self.model_dump()
        d.pop("manifestHash", None)
        d.pop("signature", None)
        return d

    def canonical_json_bytes(self) -> bytes:
        """Canonical JSON serialisation (sorted keys, compact separators, UTF-8).

        Deterministic output used for ``manifestHash`` computation.
        """
        d = self._signable_dict()
        return json.dumps(
            d, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode("utf-8")

    def compute_manifest_hash(self) -> str:
        """SHA-256 of the canonical JSON — returns ``"sha256:<hex>"``."""
        digest = hashlib.sha256(self.canonical_json_bytes()).hexdigest()
        return f"sha256:{digest}"

    # ------------------------------------------------------------------
    # Serialisation helpers
    # ------------------------------------------------------------------

    def to_json(self, indent: int = 2) -> str:
        """Full JSON representation (including ``manifestHash`` and ``signature``)."""
        return json.dumps(
            self.model_dump(),
            indent=indent,
            ensure_ascii=False,
        )

    @classmethod
    def from_json(cls, text: str) -> "SignedManifest":
        """Parse a JSON string into a ``SignedManifest``."""
        return cls.model_validate_json(text)

    @classmethod
    def from_file(cls, path: str) -> "SignedManifest":
        """Load a manifest from a JSON file path."""
        raw = Path(path).read_text(encoding="utf-8")
        return cls.from_json(raw)

    def write_to_file(self, path: str) -> None:
        """Atomically write the manifest to *path* (write-tmp + replace)."""
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        data = self.to_json()
        fd, tmp_path = tempfile.mkstemp(dir=target.parent, suffix=".tmp")
        try:
            with open(fd, "w", encoding="utf-8") as fh:
                fh.write(data)
                fh.write("\n")
                fh.flush()
            os.replace(tmp_path, target)
        except BaseException:
            Path(tmp_path).unlink(missing_ok=True)
            raise
