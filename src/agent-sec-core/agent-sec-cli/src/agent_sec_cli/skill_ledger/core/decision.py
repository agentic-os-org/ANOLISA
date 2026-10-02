"""User decisions for Skill Ledger runtime exposure."""

from __future__ import annotations

import fcntl
import json
import logging
import shutil
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.config import resolve_activation_policy
from agent_sec_cli.skill_ledger.core.certifier import _sign_manifest, scan_skill
from agent_sec_cli.skill_ledger.core.checker import check, manifest_only_status
from agent_sec_cli.skill_ledger.core.exposure import build_exposure_summary
from agent_sec_cli.skill_ledger.core.file_hasher import (
    compute_file_hashes,
    diff_file_hashes,
)
from agent_sec_cli.skill_ledger.core.live_root import (
    ResolvedSkillRoot,
    SkillRootInput,
    canonical_skill_operation,
    resolve_skill_root,
    skill_root_manageability,
    validate_resolved_skill_root,
)
from agent_sec_cli.skill_ledger.core.manifest_helpers import (
    load_verified_version_manifest,
    snapshot_matches_manifest,
    user_decision_to_dict,
    verify_latest_manifest_artifact,
)
from agent_sec_cli.skill_ledger.core.resolver import (
    resolve_activation,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    SKILL_META_DIR,
    ensure_skill_meta,
    list_version_ids,
    load_latest_manifest,
    save_manifest,
    snapshot_dir_path,
)
from agent_sec_cli.skill_ledger.errors import SkillLedgerError
from agent_sec_cli.skill_ledger.models.manifest import (
    SignedManifest,
    UserDecision,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from agent_sec_cli.skill_ledger.utils import utc_now_iso

_TRUSTED_CURRENT_STATUSES = {"pass", "warn", "deny"}
_ALLOWING_DECISIONS = {"allow", "always_allow", "rollback"}
_ROOT_COPY_EXCLUDED = {SKILL_META_DIR, ".git"}
_DECISION_LOCK = "decision.lock"
_FINDING_SUMMARY_LIMIT = 3
_FINDING_TEXT_LIMIT = 160
_FINDING_LEVEL_RANK = {
    "deny": 0,
    "critical": 0,
    "high": 0,
    "warn": 1,
    "warning": 1,
    "medium": 1,
    "low": 1,
    "pass": 2,
    "info": 2,
    "informational": 2,
}
logger = logging.getLogger(__name__)


@canonical_skill_operation
def decide_skill(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    action: str,
    target_version_id: str | None = None,
    reason: str | None = None,
) -> dict[str, Any]:
    """Apply a user decision to a skill and refresh activation."""
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    if action == "rollback":
        if not target_version_id:
            target_version_id = _default_rollback_target(root, backend)
        return rollback_skill(
            root,
            backend,
            target_version_id=target_version_id,
            reason=reason,
        )
    if action not in {"allow", "always_allow", "block"}:
        raise SkillLedgerError(
            "decision action must be one of: allow, always_allow, block, rollback"
        )

    with _skill_decision_lock(io_skill_dir):
        manifest, status_result = _load_latest_decidable_manifest(
            root,
            backend,
        )
        manifest.userDecision = UserDecision(action=action, reason=reason)
        _sign_and_save(io_skill_dir, manifest, backend)
        activation = _refresh_activation(root, backend)
    return _decision_payload(
        root,
        manifest,
        status=status_result.get("status"),
        activation=activation,
    )


@canonical_skill_operation
def clear_decision(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
) -> dict[str, Any]:
    """Remove the latest version's user decision and refresh activation."""
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    with _skill_decision_lock(io_skill_dir):
        manifest = _load_latest_verified_manifest(root, backend)
        manifest.userDecision = None
        _sign_and_save(io_skill_dir, manifest, backend)
        activation = _refresh_activation(root, backend)
    return _decision_payload(
        root,
        manifest,
        status=None,
        activation=activation,
    )


@canonical_skill_operation
def rollback_skill(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    target_version_id: str,
    reason: str | None = None,
) -> dict[str, Any]:
    """Restore a trusted snapshot to the root and record rollback as a new version."""
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    with _skill_decision_lock(io_skill_dir):
        _load_trusted_version(
            io_skill_dir,
            target_version_id,
            backend,
            expected_skill_name=root.skill_name,
        )
        target_snapshot = snapshot_dir_path(io_skill_dir, target_version_id)

        backup_dir = _backup_root(io_skill_dir)
        try:
            _replace_root_from_snapshot(io_skill_dir, target_snapshot)
            scan_skill(root, backend, force=True)
            manifest = _load_latest_verified_manifest(root, backend)
            current_hashes = compute_file_hashes(io_skill_dir)
            if not diff_file_hashes(manifest.fileHashes, current_hashes)["match"]:
                raise SkillLedgerError(
                    "rollback scan did not certify the restored skill root"
                )
            manifest.userDecision = UserDecision(
                action="rollback",
                targetVersionId=target_version_id,
                reason=reason,
            )
            _sign_and_save(io_skill_dir, manifest, backend)
        except BaseException:
            _replace_root_from_snapshot(io_skill_dir, backup_dir)
            raise

        activation = _refresh_activation(root, backend)
        return _decision_payload(
            root,
            manifest,
            status=manifest.scanStatus,
            activation=activation,
            extra={"rollbackBackup": str(root.canonical_path(backup_dir))},
        )


@canonical_skill_operation
def show_skill(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    policy: str | None = None,
) -> dict[str, Any]:
    """Return latest, active, decision, and consistency information."""
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    resolved_policy = resolve_activation_policy(
        {"activationPolicy": policy} if policy is not None else None
    )
    managed, manageability_reason = skill_root_manageability(root)
    if not managed:
        logger.info(
            "skill-ledger show skipped unmanaged skill root: skill_dir=%s reason=%s",
            root.canonical_dir,
            manageability_reason,
        )
        return _unmanaged_show_payload(
            root,
            policy=resolved_policy,
            reason=manageability_reason,
        )
    status_result = check(root, backend)
    summary = build_exposure_summary(
        root,
        backend,
        status_result=status_result,
    )
    latest_manifest = _verified_latest_for_show(root, backend, status_result)
    active_version = summary.get("activeVersionId")
    active_manifest = (
        load_verified_version_manifest(
            io_skill_dir,
            active_version,
            backend,
            expected_skill_name=root.skill_name,
        )
        if active_version
        else None
    )
    root_matches_active = _root_matches_manifest(io_skill_dir, active_manifest)
    consistency_reason = _show_consistency_reason(
        summary=summary,
        latest_manifest=latest_manifest,
        active_manifest=active_manifest,
        active_version=active_version,
        root_matches_active=root_matches_active,
        policy=resolved_policy,
    )
    findings = status_result.get("findings", [])
    display_message = _message_with_findings(summary, findings)
    warnings = [display_message] if display_message is not None else []
    return {
        **summary,
        "message": display_message,
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "activationPolicy": resolved_policy,
        "latest": _manifest_summary(latest_manifest, status_result),
        "active": _manifest_summary(active_manifest, None),
        "rootMatchesActive": root_matches_active,
        "consistencyReason": consistency_reason,
        "findings": findings,
        "warnings": warnings,
    }


def _unmanaged_show_payload(
    root: ResolvedSkillRoot,
    *,
    policy: str,
    reason: str,
) -> dict[str, Any]:
    return {
        "latestStatus": "unmanaged",
        "latestVersionId": None,
        "activeVersionId": None,
        "target": None,
        "userDecision": None,
        "reasonCode": "unmanaged_skill_root",
        "message": None,
        "managed": False,
        "manageabilityReason": reason,
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "activationPolicy": policy,
        "latest": None,
        "active": None,
        "rootMatchesActive": None,
        "consistencyReason": (
            f"skill root is not managed by the current Skill Ledger daemon: {reason}"
        ),
        "findings": [],
        "warnings": [],
    }


@canonical_skill_operation
def export_skill(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    version: str,
    output: str,
    policy: str | None = None,
) -> dict[str, Any]:
    """Export a signed snapshot plus manifest and findings for user review.

    This read-only review path resolves the current I/O root but does not require
    the canonical path to be explicitly listed in ``managedSkillDirs``.
    """
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    version_id = _resolve_export_version(root, backend, version, policy=policy)
    manifest = _load_trusted_version(
        io_skill_dir,
        version_id,
        backend,
        expected_skill_name=root.skill_name,
    )
    snapshot = snapshot_dir_path(io_skill_dir, version_id)
    out_dir = Path(output)
    out_dir_created = not out_dir.exists()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SkillLedgerError(
            f"export output already exists and is not empty: {out_dir}"
        )
    out_dir.mkdir(parents=True, exist_ok=True)
    snapshot_out = out_dir / "snapshot"
    if snapshot_out.exists():
        shutil.rmtree(snapshot_out)
    shutil.copytree(snapshot, snapshot_out)
    if not snapshot_matches_manifest(snapshot_out, manifest):
        shutil.rmtree(snapshot_out)
        if out_dir_created:
            out_dir.rmdir()
        raise SkillLedgerError(
            f"exported snapshot does not match manifest: {version_id}"
        )
    (out_dir / "manifest.json").write_text(manifest.to_json() + "\n", encoding="utf-8")
    findings = _collect_findings(manifest)
    (out_dir / "findings.json").write_text(
        json.dumps(findings, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return {
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "versionId": version_id,
        "output": str(out_dir),
        "snapshot": str(snapshot_out),
        "manifest": str(out_dir / "manifest.json"),
        "findings": str(out_dir / "findings.json"),
    }


def _load_latest_decidable_manifest(
    root: ResolvedSkillRoot,
    backend: SigningBackend,
) -> tuple[SignedManifest, dict[str, Any]]:
    manifest = _load_latest_verified_manifest(root, backend)
    status_result = manifest_only_status(root, backend)
    if status_result.get("status") not in _TRUSTED_CURRENT_STATUSES:
        raise SkillLedgerError(
            "cannot decide on untrusted latest skill version: "
            f"{status_result.get('status')}"
        )
    return manifest, status_result


def _load_latest_verified_manifest(
    root: ResolvedSkillRoot,
    backend: SigningBackend,
) -> SignedManifest:
    """Load latest only when its stored version artifact is exactly trusted."""
    io_skill_dir = str(root.io_dir)
    manifest = load_latest_manifest(io_skill_dir)
    if manifest is None:
        raise SkillLedgerError("skill has no signed manifest")
    valid, error = verify_latest_manifest_artifact(
        io_skill_dir,
        manifest,
        backend,
        expected_skill_name=root.skill_name,
    )
    if not valid:
        raise SkillLedgerError(f"untrusted latest manifest: {error}")
    return manifest


def _verified_latest_for_show(
    root: ResolvedSkillRoot,
    backend: SigningBackend,
    status_result: dict[str, Any],
) -> SignedManifest | None:
    """Return the same verified latest manifest described by ``status_result``."""
    expected_version = status_result.get("versionId")
    if not isinstance(expected_version, str):
        return None
    try:
        manifest = _load_latest_verified_manifest(root, backend)
    except SkillLedgerError:
        return None
    return manifest if manifest.versionId == expected_version else None


def _default_rollback_target(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
) -> str:
    activation = resolve_activation(
        skill_dir,
        backend,
        policy=resolve_activation_policy(),
        write_activation=False,
    )
    active_version = activation.get("activeVersionId")
    if not active_version:
        raise SkillLedgerError(
            "cannot choose rollback target: no active version under current "
            "user decision or activationPolicy"
        )
    return str(active_version)


def _load_trusted_version(
    skill_dir: str,
    version_id: str,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
) -> SignedManifest:
    if version_id not in list_version_ids(skill_dir):
        raise SkillLedgerError(f"unknown skill version: {version_id}")
    manifest = load_verified_version_manifest(
        skill_dir,
        version_id,
        backend,
        expected_skill_name=expected_skill_name,
    )
    if manifest is None:
        raise SkillLedgerError(f"untrusted version artifact: {version_id}")
    return manifest


def _sign_and_save(
    skill_dir: str,
    manifest: SignedManifest,
    backend: SigningBackend,
) -> None:
    manifest.updatedAt = utc_now_iso()
    _sign_manifest(manifest, backend)
    save_manifest(skill_dir, manifest, write_version=True)


def _refresh_activation(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
) -> dict[str, Any]:
    return resolve_activation(
        skill_dir,
        backend,
        policy=resolve_activation_policy(),
        write_activation=True,
    )


def _decision_payload(
    root: ResolvedSkillRoot,
    manifest: SignedManifest,
    *,
    status: str | None,
    activation: dict[str, Any],
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    data: dict[str, Any] = {
        "status": "decided",
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "versionId": manifest.versionId,
        "scanStatus": manifest.scanStatus,
        "manifestHash": manifest.manifestHash,
        "userDecision": _decision_dict(manifest),
        "activation": activation,
    }
    if status is not None:
        data["currentStatus"] = status
    if extra:
        data.update(extra)
    return data


def _decision_dict(manifest: SignedManifest | None) -> dict[str, Any] | None:
    if manifest is None or manifest.userDecision is None:
        return None
    return user_decision_to_dict(manifest.userDecision)


def _manifest_summary(
    manifest: SignedManifest | None,
    status_result: dict[str, Any] | None,
) -> dict[str, Any] | None:
    if manifest is None:
        return None
    status = (
        status_result.get("status")
        if status_result is not None
        else manifest.scanStatus
    )
    return {
        "versionId": manifest.versionId,
        "status": status,
        "scanStatus": manifest.scanStatus,
        "manifestHash": manifest.manifestHash,
        "userDecision": _decision_dict(manifest),
    }


def _show_consistency_reason(
    *,
    summary: dict[str, Any],
    latest_manifest: SignedManifest | None,
    active_manifest: SignedManifest | None,
    active_version: str | None,
    root_matches_active: bool | None,
    policy: str,
) -> str | None:
    reason_code = summary.get("reasonCode")
    latest_version = latest_manifest.versionId if latest_manifest is not None else None
    if reason_code == "user_block":
        return "user decision block hides this skill"
    if reason_code in {"root_drift", "tampered"}:
        return summary.get("message")
    if reason_code in {
        "latest_risk_fallback_to_previous",
        "latest_risk_hidden",
        "latest_risk_pending_decision",
    }:
        return summary.get("message")
    if latest_version != active_version:
        active_decision = active_manifest.userDecision if active_manifest else None
        if (
            active_decision is not None
            and active_decision.action in _ALLOWING_DECISIONS
        ):
            return (
                f"user decision {active_decision.action} pins active version "
                f"{active_version or 'none'} instead of latest {latest_version or 'none'}"
            )
        if latest_manifest is None:
            return "no signed manifest snapshot is available"
        if active_version is None:
            return (
                f"activationPolicy {policy} hides latest version {latest_version} "
                f"with scanStatus {latest_manifest.scanStatus}"
            )
        return (
            f"activationPolicy {policy} exposes version {active_version} "
            f"instead of latest {latest_version or 'none'}"
        )
    if root_matches_active is False:
        return (
            "root drift: current skill root does not match active snapshot "
            f"{active_version or 'none'}"
        )
    return None


def _message_with_findings(
    summary: dict[str, Any],
    findings: Any,
) -> str | None:
    message = summary.get("message")
    if not isinstance(message, str) or not message:
        return None

    reason_code = summary.get("reasonCode")
    if reason_code not in {
        "latest_risk_fallback_to_previous",
        "latest_risk_pending_decision",
    }:
        findings_summary = _summarize_findings(findings)
        if findings_summary:
            return f"{message} Latest findings: {findings_summary}."
        return message

    latest_version = _display_value(summary.get("latestVersionId"))
    latest_status = _display_value(summary.get("latestStatus"), default="unknown")
    active_version = summary.get("activeVersionId")
    if isinstance(active_version, str) and active_version:
        active_clause = f"current active version is {active_version}"
        action_clause = (
            "Review hidden latest with export --version latest, then decide: "
            f"block, rollback --version {active_version}, or allow after review."
        )
    else:
        active_clause = "no active safe version is exposed yet"
        action_clause = (
            "Review hidden latest with export --version latest, then decide: "
            "block or allow after review."
        )

    parts = [
        f"Latest version {latest_version} is {latest_status} and is not exposed; "
        f"{active_clause}."
    ]
    findings_summary = _summarize_findings(findings)
    if findings_summary:
        parts.append(f"Latest findings: {findings_summary}.")
    parts.append(action_clause)
    return " ".join(parts)


def _summarize_findings(findings: Any) -> str | None:
    if not isinstance(findings, list):
        return None
    displayable = [finding for finding in findings if isinstance(finding, dict)]
    if not displayable:
        return None

    ranked = sorted(
        enumerate(displayable),
        key=lambda item: (_finding_level_rank(item[1]), item[0]),
    )
    selected = [finding for _, finding in ranked[:_FINDING_SUMMARY_LIMIT]]
    entries = [_format_finding_summary(finding) for finding in selected]
    entries = [entry for entry in entries if entry]
    if not entries:
        return None
    remaining = len(displayable) - len(selected)
    if remaining > 0:
        entries.append(f"+{remaining} more findings")
    return "; ".join(entries)


def _finding_level_rank(finding: dict[str, Any]) -> int:
    level = _finding_field(finding, "level") or _finding_field(finding, "severity")
    return _FINDING_LEVEL_RANK.get(level.lower(), 3) if level else 3


def _format_finding_summary(finding: dict[str, Any]) -> str | None:
    level = _finding_field(finding, "level") or _finding_field(finding, "severity")
    if not level:
        level = "unknown"
    location = _finding_field(finding, "file") or _finding_field(finding, "path")
    rule = (
        _finding_field(finding, "rule")
        or _finding_field(finding, "rule_id")
        or _finding_field(finding, "title")
    )
    message = _finding_field(finding, "message") or _finding_field(
        finding, "description"
    )

    prefix_parts = [f"[{level}]"]
    if location:
        prefix_parts.append(location)
    if rule:
        prefix_parts.append(rule)
    text = " ".join(prefix_parts)
    if message:
        text = f"{text}: {message}"
    return _truncate_finding_part(text)


def _finding_field(finding: dict[str, Any], key: str) -> str | None:
    value = finding.get(key)
    if value is None:
        metadata = finding.get("metadata")
        if isinstance(metadata, dict):
            value = metadata.get(key)
    if value is None:
        return None
    text = _sanitize_finding_text(str(value))
    return text or None


def _sanitize_finding_text(text: str) -> str:
    safe_chars = [
        " " if ord(char) < 0x20 or 0x7F <= ord(char) <= 0x9F else char for char in text
    ]
    return " ".join("".join(safe_chars).split())


def _truncate_finding_part(text: str) -> str:
    if len(text) <= _FINDING_TEXT_LIMIT:
        return text
    return text[: _FINDING_TEXT_LIMIT - 3].rstrip() + "..."


def _display_value(value: Any, *, default: str = "none") -> str:
    if value is None:
        return default
    text = str(value).strip()
    return text or default


def _root_matches_manifest(
    skill_dir: str,
    manifest: SignedManifest | None,
) -> bool | None:
    if manifest is None:
        return None
    try:
        root_hashes = compute_file_hashes(skill_dir)
    except ValueError:
        return False
    return bool(diff_file_hashes(manifest.fileHashes, root_hashes)["match"])


def _resolve_export_version(
    root: ResolvedSkillRoot,
    backend: SigningBackend,
    version: str,
    *,
    policy: str | None,
) -> str:
    io_skill_dir = str(root.io_dir)
    if version == "latest":
        return _load_latest_verified_manifest(root, backend).versionId
    if version == "active":
        activation = resolve_activation(
            root,
            backend,
            policy=policy or resolve_activation_policy(),
            write_activation=False,
        )
        active_version = activation.get("activeVersionId")
        if not active_version:
            raise SkillLedgerError("skill has no active version to export")
        return str(active_version)
    if version in list_version_ids(io_skill_dir):
        return version
    raise SkillLedgerError(f"unknown export version: {version}")


def _collect_findings(manifest: SignedManifest) -> list[dict[str, Any]]:
    return [finding for scan in manifest.scans for finding in scan.findings]


@contextmanager
def _skill_decision_lock(skill_dir: str) -> Iterator[None]:
    meta = ensure_skill_meta(skill_dir)
    lock_path = meta / _DECISION_LOCK
    with lock_path.open("a", encoding="utf-8") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def _backup_root(skill_dir: str) -> Path:
    meta = ensure_skill_meta(skill_dir)
    backup = (
        meta
        / "backups"
        / f"rollback-{utc_now_iso().replace(':', '').replace('+', 'Z')}"
    )
    _copy_root(Path(skill_dir), backup)
    return backup


def _replace_root_from_snapshot(skill_dir: str, snapshot: Path) -> None:
    root = Path(skill_dir)
    for entry in list(root.iterdir()):
        if entry.name in _ROOT_COPY_EXCLUDED:
            continue
        if entry.is_symlink():
            entry.unlink()
        elif entry.is_dir():
            shutil.rmtree(entry)
        elif entry.is_file():
            entry.unlink()
        else:
            entry.unlink()
    _copy_root(snapshot, root)


def _copy_root(src: Path, dst: Path) -> None:
    dst.mkdir(parents=True, exist_ok=True)
    for entry in sorted(src.rglob("*")):
        if entry.is_symlink():
            continue
        rel = entry.relative_to(src)
        if any(part in _ROOT_COPY_EXCLUDED for part in rel.parts):
            continue
        target = dst / rel
        if entry.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        elif entry.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(entry, target)
