"""Shared manifest integrity helpers for skill-ledger core workflows."""

from agent_sec_cli.skill_ledger.errors import SignatureInvalidError
from agent_sec_cli.skill_ledger.models.manifest import SignedManifest
from agent_sec_cli.skill_ledger.signing.base import SigningBackend

MANIFEST_HASH_MISMATCH_ERROR = "manifestHash does not match manifest content"
MISSING_SIGNATURE_ERROR = "Missing signature"
SIGNATURE_FALSE_ERROR = "signature verification returned false"
SIGNATURE_INVALID_ERROR = "signature verification failed"
SIGNATURE_ALGORITHM_MISMATCH_ERROR = "signature algorithm does not match backend"
SKILL_NAME_MISMATCH_ERROR = "manifest skillName does not match the resolved skill"
VERSION_ID_MISMATCH_ERROR = "manifest versionId does not match the stored version"


def _manifest_hash_error(manifest: SignedManifest) -> str | None:
    """Return a diagnostic string when ``manifestHash`` is not self-consistent."""
    if manifest.manifestHash != manifest.compute_manifest_hash():
        return MANIFEST_HASH_MISMATCH_ERROR
    return None


def _verify_manifest_signature(
    manifest: SignedManifest,
    backend: SigningBackend,
) -> tuple[bool, str | None]:
    """Verify the manifest signature with strict bool/exception handling."""
    if manifest.signature is None:
        return False, MISSING_SIGNATURE_ERROR
    if manifest.signature.algorithm != backend.name:
        return False, SIGNATURE_ALGORITHM_MISMATCH_ERROR

    try:
        verified = backend.verify(
            manifest.manifestHash.encode("utf-8"),
            manifest.signature.value,
            manifest.signature.keyFingerprint,
        )
    except SignatureInvalidError:
        # Backends may include attacker-controlled fingerprints or signature
        # bytes in their exception text. Keep public tamper diagnostics stable.
        return False, SIGNATURE_INVALID_ERROR

    if verified is not True:
        return False, SIGNATURE_FALSE_ERROR
    return True, None


def verify_manifest_authenticity(
    manifest: SignedManifest,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
    expected_version_id: str | None = None,
) -> tuple[bool, str | None]:
    """Verify integrity before binding signed identity to its storage context."""
    hash_error = _manifest_hash_error(manifest)
    if hash_error is not None:
        return False, hash_error

    valid, error = _verify_manifest_signature(manifest, backend)
    if not valid:
        return False, error

    if manifest.skillName != expected_skill_name:
        return False, SKILL_NAME_MISMATCH_ERROR
    if expected_version_id is not None and manifest.versionId != expected_version_id:
        return False, VERSION_ID_MISMATCH_ERROR
    return True, None
