"""Audit command — deep verification of the version chain integrity.

Implements ``agent-sec-cli skill-ledger audit <skill_dir> [--verify-snapshots]``:

1. Load all public keys (key.pub + keyring/)
2. Walk versions/ chronologically
3. Verify each manifest's authenticity and explicit parent linkage
4. Optionally verify snapshot file hashes
"""

from typing import Any

from agent_sec_cli.skill_ledger.core.file_hasher import (
    compute_snapshot_file_hashes,
    diff_file_hashes,
)
from agent_sec_cli.skill_ledger.core.live_root import (
    SkillRootInput,
    canonical_skill_operation,
    resolve_skill_root,
    validate_resolved_skill_root,
)
from agent_sec_cli.skill_ledger.core.manifest_integrity import (
    MISSING_SIGNATURE_ERROR,
    verify_manifest_authenticity,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    latest_json_path,
    list_version_artifact_ids,
    load_latest_manifest,
    load_version_manifest,
    snapshot_dir_path,
)
from agent_sec_cli.skill_ledger.errors import KeyNotFoundError
from agent_sec_cli.skill_ledger.models.manifest import SignedManifest
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from pydantic import ValidationError


def _public_manifest_error(error: ValueError) -> str:
    """Avoid returning manifest-controlled values from Pydantic diagnostics."""
    if isinstance(error, ValidationError):
        return "schema validation failed"
    return str(error)


def _authenticity_error(error: str | None) -> str:
    """Return a stable audit diagnostic without interpreting untrusted fields."""
    if error == MISSING_SIGNATURE_ERROR:
        return MISSING_SIGNATURE_ERROR
    if error is None:
        return "Manifest authenticity verification failed"
    return f"Manifest authenticity invalid: {error}"


@canonical_skill_operation
def audit(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    verify_snapshots: bool = False,
) -> dict[str, Any]:
    """Perform a deep integrity audit of the version chain.

    Returns ``{"valid": bool, "versions_checked": int, "errors": [...]}``.
    """
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)

    errors: list[dict[str, Any]] = []
    version_ids = list_version_artifact_ids(io_skill_dir)

    manifests: dict[str, SignedManifest | None] = {}
    authenticated: dict[str, bool] = {}

    for vid in version_ids:
        try:
            manifest = load_version_manifest(io_skill_dir, vid)
        except (ValueError, ValidationError) as exc:
            manifests[vid] = None
            authenticated[vid] = False
            errors.append(
                {
                    "versionId": vid,
                    "error": (
                        f"Version manifest {vid}.json is corrupted: "
                        f"{_public_manifest_error(exc)}"
                    ),
                }
            )
            continue

        if manifest is None:
            manifests[vid] = None
            authenticated[vid] = False
            errors.append(
                {"versionId": vid, "error": f"Version file {vid}.json is missing"}
            )
            continue

        manifests[vid] = manifest
        try:
            authenticity_valid, authenticity_error = verify_manifest_authenticity(
                manifest,
                backend,
                expected_skill_name=root.skill_name,
                expected_version_id=vid,
            )
        except KeyNotFoundError:
            authenticity_valid = False
            authenticity_error = "manifest signature could not be verified"
        authenticated[vid] = authenticity_valid
        if not authenticity_valid:
            errors.append(
                {
                    "versionId": vid,
                    "error": _authenticity_error(authenticity_error),
                }
            )

        # Never compare a snapshot against attacker-controlled fileHashes.
        if verify_snapshots and authenticity_valid:
            snap_path = snapshot_dir_path(io_skill_dir, vid)
            if snap_path.is_dir():
                try:
                    snap_hashes = compute_snapshot_file_hashes(str(snap_path))
                except ValueError as exc:
                    errors.append(
                        {
                            "versionId": vid,
                            "error": f"Snapshot invalid — {exc}",
                        }
                    )
                else:
                    diff = diff_file_hashes(manifest.fileHashes, snap_hashes)
                    if not diff["match"]:
                        errors.append(
                            {
                                "versionId": vid,
                                "error": (
                                    f"Snapshot mismatch — added: {diff['added']}, "
                                    f"removed: {diff['removed']}, modified: {diff['modified']}"
                                ),
                            }
                        )
            else:
                errors.append(
                    {
                        "versionId": vid,
                        "error": f"Snapshot directory {vid}.snapshot/ is missing",
                    }
                )

    version_positions = {
        version_id: index for index, version_id in enumerate(version_ids)
    }
    for vid in version_ids:
        manifest = manifests.get(vid)
        if manifest is None or not authenticated.get(vid, False):
            continue

        parent_id = manifest.previousVersionId
        parent_signature = manifest.previousManifestSignature
        if parent_id is None and parent_signature is None:
            # A valid signer may intentionally start a new segment after corrupt history.
            continue
        if parent_id is None or parent_signature is None:
            errors.append(
                {
                    "versionId": vid,
                    "error": (
                        "previousVersionId and previousManifestSignature must both be null "
                        "or both be set — chain broken"
                    ),
                }
            )
            continue

        parent_position = version_positions.get(parent_id)
        if parent_position is None:
            errors.append(
                {
                    "versionId": vid,
                    "error": "Referenced parent version does not exist — chain broken",
                }
            )
            continue
        if parent_position >= version_positions[vid]:
            errors.append(
                {
                    "versionId": vid,
                    "error": "previousVersionId must reference an earlier version — chain broken",
                }
            )
            continue

        parent = manifests.get(parent_id)
        if parent is None or not authenticated.get(parent_id, False):
            errors.append(
                {
                    "versionId": vid,
                    "error": (
                        "Cannot authenticate referenced parent — "
                        "prior version manifest is missing or invalid; chain broken"
                    ),
                }
            )
            continue

        if parent.signature is None or parent_signature != parent.signature.value:
            errors.append(
                {
                    "versionId": vid,
                    "error": (
                        "previousManifestSignature does not match the referenced "
                        "parent version's signature — chain broken"
                    ),
                }
            )

    # Verify latest.json consistency
    latest_exists = latest_json_path(io_skill_dir).is_file()
    try:
        latest = load_latest_manifest(io_skill_dir)
    except (ValueError, ValidationError) as exc:
        errors.append(
            {
                "versionId": "latest.json",
                "error": f"latest.json is corrupted: {_public_manifest_error(exc)}",
            }
        )
        latest = None
    if latest is not None:
        try:
            latest_valid, latest_error = verify_manifest_authenticity(
                latest,
                backend,
                expected_skill_name=root.skill_name,
            )
        except KeyNotFoundError:
            latest_valid = False
            latest_error = "manifest signature could not be verified"
        if not latest_valid:
            errors.append(
                {
                    "versionId": "latest.json",
                    "error": _authenticity_error(latest_error),
                }
            )
        else:
            verified_version_ids = [
                version_id
                for version_id in version_ids
                if authenticated.get(version_id, False)
            ]
            expected_latest_vid = (
                verified_version_ids[-1] if verified_version_ids else None
            )
            if expected_latest_vid is None:
                errors.append(
                    {
                        "versionId": "latest.json",
                        "error": (
                            "latest.json exists but no authenticated "
                            "version artifact was found"
                        ),
                    }
                )
            elif latest.versionId != expected_latest_vid:
                errors.append(
                    {
                        "versionId": "latest.json",
                        "error": (
                            f"latest.json points to {latest.versionId} "
                            f"but latest verified version is {expected_latest_vid}"
                        ),
                    }
                )
            else:
                stored_latest = manifests.get(expected_latest_vid)
                if (
                    stored_latest is None
                    or not authenticated.get(expected_latest_vid, False)
                    or stored_latest != latest
                ):
                    errors.append(
                        {
                            "versionId": "latest.json",
                            "error": (
                                "latest.json does not match its authenticated "
                                "version artifact"
                            ),
                        }
                    )
    elif version_ids and not latest_exists:
        errors.append(
            {
                "versionId": "latest.json",
                "error": "latest.json is missing while version artifacts exist",
            }
        )

    result = {
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "valid": len(errors) == 0,
        "versions_checked": len(version_ids),
        "errors": errors,
    }
    if not version_ids and not errors:
        result["message"] = "No versions found — nothing to audit"
    return root.canonicalize_payload(result)
