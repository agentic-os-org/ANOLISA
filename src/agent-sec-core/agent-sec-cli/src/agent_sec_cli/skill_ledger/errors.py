"""Custom exception hierarchy for skill-ledger."""

from pathlib import Path


class SkillLedgerError(Exception):
    """Base exception for all skill-ledger errors."""

    pass


# ---------------------------------------------------------------------------
# Key management
# ---------------------------------------------------------------------------


class KeyNotFoundError(SkillLedgerError):
    """Signing key files do not exist (run ``init`` first)."""

    def __init__(self, path: str) -> None:
        super().__init__(
            f"Signing key not found: {path}. Run 'agent-sec-cli skill-ledger init --no-baseline' first."
        )
        self.path = path


class KeyAlreadyExistsError(SkillLedgerError):
    """Signing key already exists and ``--force-keys`` was not supplied."""

    def __init__(self, path: str) -> None:
        super().__init__(
            f"Key already exists: {path}. Use init --force-keys to overwrite."
        )
        self.path = path


class PassphraseError(SkillLedgerError):
    """Passphrase is incorrect or could not be obtained."""

    pass


# ---------------------------------------------------------------------------
# Manifest / signature
# ---------------------------------------------------------------------------


class SignatureInvalidError(SkillLedgerError):
    """Digital signature verification failed (possible tampering)."""

    def __init__(self, reason: str) -> None:
        super().__init__(f"Signature verification failed: {reason}")
        self.reason = reason


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------


class ConfigError(SkillLedgerError):
    """Configuration file is missing or invalid."""

    def __init__(self, reason: str) -> None:
        super().__init__(f"Configuration error: {reason}")
        self.reason = reason


# ---------------------------------------------------------------------------
# Skill root resolution
# ---------------------------------------------------------------------------


class SkillRootResolveError(SkillLedgerError):
    """SkillFS could not resolve a canonical skill path for I/O."""

    reason_code = "skill_root_resolve_failed"

    def __init__(self, canonical_skill_dir: str | Path, reason: str) -> None:
        self.canonical_skill_dir = Path(canonical_skill_dir)
        self.reason = reason
        super().__init__(
            f"failed to resolve skill root for {self.canonical_skill_dir}: {reason}"
        )


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------


class FindingsFileError(SkillLedgerError):
    """Findings JSON file is missing or invalid."""

    def __init__(self, path: str, reason: str) -> None:
        super().__init__(f"Invalid findings file {path}: {reason}")
        self.path = path
        self.reason = reason
