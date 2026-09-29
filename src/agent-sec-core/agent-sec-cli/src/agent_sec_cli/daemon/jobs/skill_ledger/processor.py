"""Synchronous Skill Ledger scan and activation processing."""

from pathlib import Path
from typing import Any

from agent_sec_cli.daemon.jobs.skill_ledger.protocol import SkillFsChange
from agent_sec_cli.skill_ledger.config import resolve_activation_policy
from agent_sec_cli.skill_ledger.core.certifier import scan_skill
from agent_sec_cli.skill_ledger.core.live_root import (
    ResolvedSkillRoot,
    resolve_skill_root,
)
from agent_sec_cli.skill_ledger.core.resolver import (
    resolve_activation as resolve_skill_activation,
)
from agent_sec_cli.skill_ledger.errors import SkillRootResolveError
from agent_sec_cli.skill_ledger.signing.ed25519 import NativeEd25519Backend
from agent_sec_cli.skill_ledger.signing.key_manager import keys_exist


def process_skill_change(change: SkillFsChange) -> dict[str, Any]:
    """Run scan and activation resolution for a debounced SkillFS change."""
    try:
        root = _resolve_skill_root(change.canonical_skill_dir)
    except SkillRootResolveError as exc:
        return _skipped_resolve_failure_result(change, str(exc))

    policy = _resolve_activation_policy()
    backend = _ensure_default_backend()
    scan_result: dict[str, Any] | None = None
    scan_error: str | None = None
    try:
        scan_result = _scan_skill(root, backend)
    except Exception as exc:
        scan_error = root.canonicalize_message(str(exc))

    activation_result: dict[str, Any] | None = None
    activation_error: str | None = None
    try:
        activation_result = _resolve_activation(root, backend, policy)
    except Exception as exc:
        activation_error = root.canonicalize_message(str(exc))

    errors = [error for error in (scan_error, activation_error) if error is not None]
    result: dict[str, Any] = {
        "status": "error" if errors else "processed",
        "skill": change.to_dict(),
        "scan": scan_result,
        "activation": activation_result,
    }
    if errors:
        result["error"] = "; ".join(errors)
    if scan_error is not None:
        result["scanError"] = scan_error
    if activation_error is not None:
        result["activationError"] = activation_error
    return result


def _skipped_resolve_failure_result(
    change: SkillFsChange,
    message: str,
) -> dict[str, Any]:
    return {
        "status": "skipped",
        "reasonCode": SkillRootResolveError.reason_code,
        "message": message,
        "skill": change.to_dict(),
        "scan": None,
        "activation": None,
    }


def _ensure_default_backend() -> Any:
    if not keys_exist():
        NativeEd25519Backend().generate_keys(passphrase=None)
    return NativeEd25519Backend()


def _resolve_skill_root(canonical_skill_dir: Path) -> ResolvedSkillRoot:
    return resolve_skill_root(canonical_skill_dir)


def _scan_skill(skill_dir: ResolvedSkillRoot, backend: Any) -> dict[str, Any]:
    return scan_skill(skill_dir, backend, force=False)


def _resolve_activation(
    skill_dir: ResolvedSkillRoot,
    backend: Any,
    policy: str,
) -> dict[str, Any]:
    return resolve_skill_activation(skill_dir, backend, policy=policy)


def _resolve_activation_policy() -> str:
    return resolve_activation_policy()
