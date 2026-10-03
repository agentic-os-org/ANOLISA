"""Check command — the full state machine from design doc §2.

Implements ``agent-sec-cli skill-ledger check <skill_dir>``:

1. Read ``latest.json``
2. Missing with no version artifacts → ``{"status": "none"}``
3. Missing with version artifacts → ``{"status": "tampered", "reason": ...}``
4. Manifest present → verify its hash, signature, and signed identity
5. Invalid → ``{"status": "tampered", "reason": ...}``
6. Valid → compare current fileHashes; mismatch → ``{"status": "drifted", ...}``
7. Match → dispatch ``scanStatus`` as ``deny`` / ``warn`` / ``none`` / ``pass``
"""

import json
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.core.file_hasher import (
    compute_file_hashes,
    diff_file_hashes,
)
from agent_sec_cli.skill_ledger.core.live_root import (
    ResolvedSkillRoot,
    SkillRootInput,
    canonical_skill_operation,
    resolve_skill_root,
    validate_resolved_skill_root,
)
from agent_sec_cli.skill_ledger.core.manifest_helpers import (
    snapshot_matches_manifest,
    verify_latest_manifest_artifact,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    latest_json_path,
    list_version_artifact_ids,
    load_latest_manifest,
    snapshot_dir_path,
)
from agent_sec_cli.skill_ledger.errors import KeyNotFoundError
from agent_sec_cli.skill_ledger.models.manifest import (
    SignedManifest,
)
from agent_sec_cli.skill_ledger.path_identity import (
    normalize_canonical_skill_dir,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend


def _safe_metadata(root: ResolvedSkillRoot) -> dict[str, Any]:
    """Return path-derived identity while withholding all manifest-derived fields."""
    return {
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "versionId": None,
        "createdAt": None,
        "updatedAt": None,
        "fileCount": None,
        "manifestHash": None,
        "userDecision": None,
    }


def _manifest_metadata(
    manifest: SignedManifest,
    root: ResolvedSkillRoot,
) -> dict[str, Any]:
    """Return standard metadata fields extracted from a loaded manifest.

    These fields are included in every ``check`` / ``check --all`` return dict
    so that consumers (Agent, plugin, ``status`` command) never need to read
    ``.skill-meta/latest.json`` directly.
    """
    return {
        **_safe_metadata(root),
        "versionId": manifest.versionId,
        "createdAt": manifest.createdAt,
        "updatedAt": manifest.updatedAt,
        "fileCount": len(manifest.fileHashes),
        "manifestHash": manifest.manifestHash,
        "userDecision": (
            manifest.userDecision.model_dump(exclude_none=True)
            if manifest.userDecision is not None
            else None
        ),
    }


def _load_authenticated_manifest(
    root: ResolvedSkillRoot,
    backend: SigningBackend,
) -> tuple[SignedManifest | None, dict[str, Any] | None]:
    """Load latest.json and return a terminal result unless it is authenticated."""
    io_skill_dir = str(root.io_dir)
    try:
        manifest = load_latest_manifest(io_skill_dir)
    except (json.JSONDecodeError, ValueError):
        if latest_json_path(io_skill_dir).is_file():
            return None, {
                **_safe_metadata(root),
                "status": "tampered",
                "reason": "manifest file is corrupted or schema-invalid",
            }
        manifest = None

    if manifest is None:
        if list_version_artifact_ids(io_skill_dir):
            return None, {
                **_safe_metadata(root),
                "status": "tampered",
                "reason": "latest.json is missing while version artifacts exist",
            }
        return None, {**_safe_metadata(root), "status": "none"}

    try:
        valid, error = verify_latest_manifest_artifact(
            io_skill_dir,
            manifest,
            backend,
            expected_skill_name=root.skill_name,
            verify_snapshot=False,
        )
    except KeyNotFoundError:
        return None, {
            **_safe_metadata(root),
            "status": "tampered",
            "reason": "manifest signature could not be verified",
        }
    if not valid:
        return None, {
            **_safe_metadata(root),
            "status": "tampered",
            "reason": error,
        }
    return manifest, None


@canonical_skill_operation
def check(skill_dir: SkillRootInput, backend: SigningBackend) -> dict[str, Any]:
    """Execute the full check state machine.

    Returns a JSON-serialisable dict with at minimum ``{"status": "<status>"}``.
    When a manifest is available the dict also includes standard metadata:
    ``skillName``, ``versionId``, ``createdAt``, ``updatedAt``, ``fileCount``,
    ``manifestHash``.
    """
    # Step 0: Resolve once, then keep identity separate from filesystem I/O.
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)

    # A manifest cannot influence status or metadata until its authenticity is proven.
    manifest, terminal_result = _load_authenticated_manifest(root, backend)
    if terminal_result is not None:
        return terminal_result
    assert manifest is not None

    meta = _manifest_metadata(manifest, root)

    current_hashes = compute_file_hashes(io_skill_dir)
    diff = diff_file_hashes(manifest.fileHashes, current_hashes)
    if not diff["match"]:
        return {
            **meta,
            "status": "drifted",
            "added": diff["added"],
            "removed": diff["removed"],
            "modified": diff["modified"],
        }

    scan_status = manifest.scanStatus

    if scan_status == "deny":
        findings = _collect_findings(manifest)
        return {**meta, "status": "deny", "findings": findings}

    if scan_status == "warn":
        findings = _collect_findings(manifest)
        return {**meta, "status": "warn", "findings": findings}

    if scan_status == "none":
        return {**meta, "status": "none"}

    # pass (or any other value)
    return {**meta, "status": "pass"}


@canonical_skill_operation
def manifest_only_status(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
) -> dict[str, Any]:
    """Return latest trusted manifest status without hashing root files."""
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    manifest, terminal_result = _load_authenticated_manifest(root, backend)
    if terminal_result is not None:
        return terminal_result
    assert manifest is not None

    if not snapshot_matches_manifest(
        snapshot_dir_path(io_skill_dir, manifest.versionId),
        manifest,
    ):
        return {
            **_safe_metadata(root),
            "status": "tampered",
            "reason": "snapshot does not match manifest",
        }

    meta = _manifest_metadata(manifest, root)
    if manifest.scanStatus in {"deny", "warn"}:
        return {
            **meta,
            "status": manifest.scanStatus,
            "findings": _collect_findings(manifest),
        }
    if manifest.scanStatus == "none":
        return {**meta, "status": "none"}
    return {**meta, "status": "pass"}


def _collect_findings(manifest: SignedManifest) -> list[dict[str, Any]]:
    """Extract findings from all scans in the manifest."""
    return [f for scan in manifest.scans for f in scan.findings]


def check_batch(
    skill_dirs: list[Path],
    backend: SigningBackend,
) -> list[dict[str, Any]]:
    """Check multiple skill directories and return a list of per-skill results.

    Each entry is the enriched dict returned by :func:`check`.  On per-skill
    errors the entry contains ``{"skillName": ..., "status": "error", ...}``
    so that callers always receive one result per input directory.
    """
    results: list[dict[str, Any]] = []
    for skill_dir in skill_dirs:
        try:
            result = check(str(skill_dir), backend)
            results.append(result)
        except Exception as exc:
            canonical_dir = normalize_canonical_skill_dir(skill_dir)
            results.append(
                {
                    "canonicalSkillDir": str(canonical_dir),
                    "skillName": canonical_dir.name,
                    "status": "error",
                    "error": str(exc),
                }
            )
    return results
