"""Unified Skill Ledger exposure and warning summary."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.activation_policy import (
    allowed_scan_statuses_for_policy,
)
from agent_sec_cli.skill_ledger.core.checker import manifest_only_status
from agent_sec_cli.skill_ledger.core.live_root import (
    ResolvedSkillRoot,
    SkillRootInput,
    resolve_skill_root,
    validate_resolved_skill_root,
)
from agent_sec_cli.skill_ledger.core.manifest_helpers import (
    load_verified_version_manifest,
    user_decision_to_dict,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    SKILL_META_DIR,
    VERSIONS_DIR,
    list_version_artifact_ids,
    list_version_ids,
)
from agent_sec_cli.skill_ledger.errors import KeyNotFoundError
from agent_sec_cli.skill_ledger.models.manifest import (
    SignedManifest,
    UserDecision,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend

_ALLOWING_USER_DECISIONS = {"allow", "always_allow", "rollback"}
_USER_DECISION_STATUSES = frozenset({"pass", "warn", "deny", "none"})
_EXPOSABLE_STATUSES = allowed_scan_statuses_for_policy("pass_warn_only")
_BLOCKED_BY_USER = object()
PENDING_DECISION_SNAPSHOT = "__pending_decision__.snapshot"
PENDING_DECISION_TARGET = f"{SKILL_META_DIR}/{VERSIONS_DIR}/{PENDING_DECISION_SNAPSHOT}"


def build_exposure_summary(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    status_result: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Return the single source of truth for runtime exposure and warnings.

    The summary intentionally stays small so hooks, ``show``, and activation
    resolution can consume the same decision without learning separate policy
    branches.
    """
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    if status_result is None:
        status_result = manifest_only_status(root, backend)
    latest_status = str(status_result.get("status", "unknown"))
    latest_version = _latest_version_id(status_result)

    decision_candidate = _find_user_decision_candidate(
        io_skill_dir,
        backend,
        expected_skill_name=root.skill_name,
    )
    if decision_candidate is _BLOCKED_BY_USER:
        return _with_identity(
            root,
            _summary(
                latest_status=latest_status,
                latest_version=latest_version,
                active_version=None,
                user_decision=_newest_block_decision(
                    io_skill_dir,
                    backend,
                    expected_skill_name=root.skill_name,
                ),
                reason_code="user_block",
                message=None,
            ),
        )
    if isinstance(decision_candidate, tuple):
        version_id, decision = decision_candidate
        return _with_identity(
            root,
            _summary(
                latest_status=latest_status,
                latest_version=latest_version,
                active_version=version_id,
                user_decision=decision,
                reason_code=f"user_{decision.action}",
                message=None,
            ),
        )

    candidate = _find_policy_candidate(
        io_skill_dir,
        backend,
        expected_skill_name=root.skill_name,
    )
    active_version = candidate[0] if candidate is not None else None

    if latest_status in {"pass", "warn"}:
        if active_version == latest_version and active_version is not None:
            return _with_identity(
                root,
                _summary(
                    latest_status=latest_status,
                    latest_version=latest_version,
                    active_version=active_version,
                    user_decision=None,
                    reason_code="normal",
                    message=None,
                ),
            )
        return _with_identity(
            root,
            _summary(
                latest_status=latest_status,
                latest_version=latest_version,
                active_version=active_version,
                user_decision=None,
                reason_code="tampered",
                message=_activation_message(
                    f"Latest {latest_status} snapshot is not a trusted activation target",
                    active_version,
                ),
            ),
        )

    if latest_status == "drifted":
        return _with_identity(
            root,
            _summary(
                latest_status=latest_status,
                latest_version=latest_version,
                active_version=active_version,
                user_decision=None,
                reason_code="root_drift",
                message=_activation_message(
                    f"Current skill root drifted from latest signed version {latest_version or 'none'}",
                    active_version,
                ),
            ),
        )

    if latest_status == "tampered":
        return _with_identity(
            root,
            _summary(
                latest_status=latest_status,
                latest_version=latest_version,
                active_version=active_version,
                user_decision=None,
                reason_code="tampered",
                message=_activation_message(
                    f"Latest skill metadata is tampered for version {latest_version or 'none'}",
                    active_version,
                ),
            ),
        )

    reason_code = (
        "latest_risk_fallback_to_previous"
        if active_version is not None
        else "latest_risk_pending_decision"
    )
    return _with_identity(
        root,
        _summary(
            latest_status=latest_status,
            latest_version=latest_version,
            active_version=active_version,
            user_decision=None,
            reason_code=reason_code,
            message=_activation_message(
                f"Latest skill status is {latest_status} for version {latest_version or 'none'}",
                active_version,
            ),
        ),
    )


def exposure_target(version_id: str) -> str:
    """Return the SkillFS activation target for a snapshot version."""
    return f"{SKILL_META_DIR}/{VERSIONS_DIR}/{version_id}.snapshot"


def pending_decision_target() -> str:
    """Return the SkillFS activation target for the safe pending review stub."""
    return PENDING_DECISION_TARGET


def is_pending_decision_target(target: Any) -> bool:
    """Return whether *target* is the safe pending review stub target."""
    return target == PENDING_DECISION_TARGET


def _summary(
    *,
    latest_status: str,
    latest_version: str | None,
    active_version: str | None,
    user_decision: UserDecision | None,
    reason_code: str,
    message: str | None,
) -> dict[str, Any]:
    target = exposure_target(active_version) if active_version else None
    if target is None and user_decision is None and message is not None:
        target = pending_decision_target()
    return {
        "latestStatus": latest_status,
        "latestVersionId": latest_version,
        "activeVersionId": active_version,
        "target": target,
        "userDecision": user_decision_to_dict(user_decision),
        "reasonCode": reason_code,
        "message": message,
    }


def _with_identity(
    root: ResolvedSkillRoot,
    summary: dict[str, Any],
) -> dict[str, Any]:
    return {
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        **summary,
    }


def _activation_message(prefix: str, active_version: str | None) -> str:
    if active_version is None:
        return (
            f"{prefix}; active skill is a safe review stub pending user decision. "
            "Review with 'agent-sec-cli skill-ledger show' or "
            "'agent-sec-cli skill-ledger export', then choose with "
            "'agent-sec-cli skill-ledger decide'."
        )
    return f"{prefix}; active version is {active_version}."


def _latest_version_id(
    status_result: dict[str, Any],
) -> str | None:
    if status_result.get("status") == "tampered":
        return None
    value = status_result.get("versionId")
    if isinstance(value, str) and value:
        return value
    return None


def _find_user_decision_candidate(
    skill_dir: str | Path,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
) -> tuple[str, UserDecision] | object | None:
    saw_newer_trusted_version = False
    saw_invalid_artifact = False
    for version_id in reversed(list_version_artifact_ids(skill_dir)):
        try:
            manifest = load_verified_version_manifest(
                skill_dir,
                version_id,
                backend,
                expected_skill_name=expected_skill_name,
            )
        except KeyNotFoundError:
            saw_invalid_artifact = True
            continue
        if manifest is None or manifest.scanStatus not in _USER_DECISION_STATUSES:
            saw_invalid_artifact = True
            continue
        decision = manifest.userDecision
        if decision is None:
            saw_newer_trusted_version = True
            continue
        if decision.action == "block":
            return None if saw_newer_trusted_version else _BLOCKED_BY_USER
        if decision.action in _ALLOWING_USER_DECISIONS:
            if saw_invalid_artifact:
                return None
            return version_id, decision
        saw_newer_trusted_version = True
    return None


def _newest_block_decision(
    skill_dir: str | Path,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
) -> UserDecision | None:
    for version_id in reversed(list_version_ids(skill_dir)):
        manifest = _load_trusted_activation_manifest(
            skill_dir,
            version_id,
            backend,
            expected_skill_name=expected_skill_name,
            allowed_statuses=_USER_DECISION_STATUSES,
        )
        if manifest is None:
            continue
        if (
            manifest.userDecision is not None
            and manifest.userDecision.action == "block"
        ):
            return manifest.userDecision
    return None


def _find_policy_candidate(
    skill_dir: str | Path,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
) -> tuple[str, str] | None:
    for version_id in reversed(list_version_ids(skill_dir)):
        manifest = _load_trusted_activation_manifest(
            skill_dir,
            version_id,
            backend,
            expected_skill_name=expected_skill_name,
            allowed_statuses=_EXPOSABLE_STATUSES,
        )
        if manifest is None:
            continue
        return version_id, exposure_target(version_id)
    return None


def _load_trusted_activation_manifest(
    skill_dir: str | Path,
    version_id: str,
    backend: SigningBackend,
    *,
    expected_skill_name: str,
    allowed_statuses: frozenset[str],
) -> SignedManifest | None:
    try:
        manifest = load_verified_version_manifest(
            skill_dir,
            version_id,
            backend,
            expected_skill_name=expected_skill_name,
        )
    except KeyNotFoundError:
        return None
    if manifest is None:
        return None
    if manifest.scanStatus not in allowed_statuses:
        return None
    return manifest
