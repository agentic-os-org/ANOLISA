"""Shared helpers for trusted Skill Ledger manifest handling."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.core.file_hasher import (
    compute_snapshot_file_hashes,
    diff_file_hashes,
)
from agent_sec_cli.skill_ledger.core.manifest_integrity import (
    verify_manifest_authenticity,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    is_version_id,
    list_version_artifact_ids,
    load_version_manifest,
    snapshot_dir_path,
)
from agent_sec_cli.skill_ledger.models.manifest import (
    SignedManifest,
    UserDecision,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend


def snapshot_matches_manifest(snapshot: str | Path, manifest: SignedManifest) -> bool:
    """Return whether a snapshot's strict file hashes match its manifest."""
    try:
        snapshot_hashes = compute_snapshot_file_hashes(snapshot)
    except ValueError:
        return False
    return bool(diff_file_hashes(manifest.fileHashes, snapshot_hashes)["match"])


def load_verified_version_manifest(
    skill_dir: str | Path,
    version_id: str,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
    verify_snapshot: bool = True,
) -> SignedManifest | None:
    """Load a version only when its identity, signature, and snapshot verify."""
    if not is_version_id(version_id):
        return None
    try:
        manifest = load_version_manifest(skill_dir, version_id)
    except (json.JSONDecodeError, ValueError):
        return None
    if manifest is None:
        return None

    valid, _ = verify_manifest_authenticity(
        manifest,
        backend,
        expected_skill_name=expected_skill_name,
        expected_version_id=version_id,
    )
    if not valid:
        return None
    if verify_snapshot and not snapshot_matches_manifest(
        snapshot_dir_path(skill_dir, version_id), manifest
    ):
        return None
    return manifest


def load_newest_verified_version_manifest(
    skill_dir: str | Path,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
    verify_snapshot: bool = True,
) -> SignedManifest | None:
    """Return the newest stored version whose requested trust checks pass."""
    for version_id in reversed(list_version_artifact_ids(skill_dir)):
        manifest = load_verified_version_manifest(
            skill_dir,
            version_id,
            backend,
            expected_skill_name=expected_skill_name,
            verify_snapshot=verify_snapshot,
        )
        if manifest is not None:
            return manifest
    return None


def verify_latest_manifest_artifact(
    skill_dir: str | Path,
    manifest: SignedManifest,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
    verify_snapshot: bool = True,
) -> tuple[bool, str | None]:
    """Verify that latest is authentic and names the newest verified artifact."""
    valid, error = verify_manifest_authenticity(
        manifest,
        backend,
        expected_skill_name=expected_skill_name,
    )
    if not valid:
        return False, error

    stored_manifest = load_newest_verified_version_manifest(
        skill_dir,
        backend,
        expected_skill_name=expected_skill_name,
        verify_snapshot=False,
    )
    if stored_manifest is None:
        return False, "no verified version artifact is available"
    if stored_manifest.versionId != manifest.versionId:
        return False, "latest manifest does not reference the newest verified artifact"
    if stored_manifest != manifest:
        return False, "latest manifest does not match its version artifact"
    if verify_snapshot and not snapshot_matches_manifest(
        snapshot_dir_path(skill_dir, stored_manifest.versionId), stored_manifest
    ):
        return False, "latest version snapshot is missing or invalid"
    return True, None


def user_decision_to_dict(decision: UserDecision | None) -> dict[str, Any] | None:
    """Return a JSON payload for a user decision."""
    if decision is None:
        return None
    return decision.model_dump(exclude_none=True)
