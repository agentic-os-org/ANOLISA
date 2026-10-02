"""Runtime activation resolver for daemon-stage ledger decisions.

The resolver keeps runtime policy decisions on the ledger side and writes a
minimal activation contract:

    {"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}

The daemon stage will reuse this internal helper when publishing the current
runtime target. A null target means the user explicitly blocked the skill or
SkillFS should fail safe to hidden.
"""

import json
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.activation_policy import (
    DEFAULT_ACTIVATION_POLICY,
    validate_activation_policy,
)
from agent_sec_cli.skill_ledger.core.checker import check
from agent_sec_cli.skill_ledger.core.exposure import (
    build_exposure_summary,
    exposure_target,
    is_pending_decision_target,
    pending_decision_target,
)
from agent_sec_cli.skill_ledger.core.live_root import (
    ResolvedSkillRoot,
    SkillRootInput,
    canonical_skill_operation,
    resolve_skill_root,
    validate_resolved_skill_root,
)
from agent_sec_cli.skill_ledger.core.version_chain import (
    SKILL_META_DIR,
    ensure_skill_meta,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend

SCHEMA_VERSION = 1
ACTIVATION_JSON = "activation.json"
ACTIVATION_XATTR = "user.agent_sec.skill_ledger.activation"


def activation_json_path(skill_dir: str | Path) -> Path:
    """Return the activation contract path for *skill_dir*."""
    return Path(skill_dir) / SKILL_META_DIR / ACTIVATION_JSON


def activation_xattr_name() -> str:
    """Return the xattr name used for the activation contract."""
    return ACTIVATION_XATTR


def snapshot_target(version_id: str) -> str:
    """Return the SkillFS target path for a version snapshot."""
    return exposure_target(version_id)


def pending_snapshot_target() -> str:
    """Return the SkillFS target path for the pending decision stub."""
    return pending_decision_target()


def ensure_pending_decision_stub(skill_dir: str | Path) -> Path:
    """Create the safe pending decision stub snapshot and return its path."""
    skill_path = Path(skill_dir)
    stub_dir = skill_path / pending_decision_target()
    if stub_dir.exists():
        shutil.rmtree(stub_dir)
    stub_dir.mkdir(parents=True, exist_ok=True)
    skill_name = skill_path.name
    (stub_dir / "SKILL.md").write_text(
        (
            "---\n"
            f"name: {skill_name}\n"
            "description: Skill requires manual review before use\n"
            "---\n"
            "# Pending Skill Ledger Review\n\n"
            "This is a safe placeholder. The real skill version is not exposed "
            "because it is not currently eligible for exposure and no trusted "
            "fallback version is available.\n\n"
            "Run `agent-sec-cli skill-ledger show <skill_dir>` to inspect the "
            "status and follow its recovery guidance. Use `scan` or `certify` "
            "to establish a trusted scanned version, or run `agent-sec-cli "
            "skill-ledger decide` to choose `allow` / `block` only when the "
            "latest metadata is trusted.\n"
        ),
        encoding="utf-8",
    )
    return stub_dir


def cleanup_pending_decision_stub(skill_dir: str | Path) -> None:
    """Remove a stale pending decision stub after real activation changes."""
    stub_dir = Path(skill_dir) / pending_decision_target()
    if stub_dir.exists():
        shutil.rmtree(stub_dir)


@canonical_skill_operation
def resolve_activation(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    policy: str = DEFAULT_ACTIVATION_POLICY,
    write_activation: bool = True,
) -> dict[str, Any]:
    """Resolve and optionally persist the runtime activation target.

    Runtime exposure has one behavior branch: activate a trusted ``pass`` or
    ``warn`` snapshot unless a user decision explicitly allows or blocks a
    version. Retired policy strings are rejected.
    """
    policy = validate_activation_policy(policy)

    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    io_skill_dir = str(root.io_dir)
    status_result = check(root, backend)
    summary = build_exposure_summary(
        root,
        backend,
        status_result=status_result,
    )
    target = summary["target"]
    active_version = summary["activeVersionId"]

    activation = {"schemaVersion": SCHEMA_VERSION, "target": target}
    activation_xattr = _activation_xattr_status(
        written=False,
        available=False,
        skipped=True,
    )
    if write_activation:
        if is_pending_decision_target(target):
            ensure_pending_decision_stub(io_skill_dir)
            activation_xattr = write_activation_contract(io_skill_dir, activation)
        else:
            activation_xattr = write_activation_contract(io_skill_dir, activation)
            cleanup_pending_decision_stub(io_skill_dir)
    activation_xattr = _canonicalize_activation_xattr(root, activation_xattr)

    return {
        "schemaVersion": SCHEMA_VERSION,
        "canonicalSkillDir": str(root.canonical_dir),
        "skillName": root.skill_name,
        "target": target,
        "activeVersionId": active_version,
        "status": summary["latestStatus"],
        "latestStatus": summary["latestStatus"],
        "latestVersionId": summary["latestVersionId"],
        "policy": policy,
        "userDecision": summary["userDecision"],
        "reasonCode": summary["reasonCode"],
        "message": summary["message"],
        "activationPath": str(activation_json_path(root.canonical_dir)),
        "activationXattr": activation_xattr,
    }


@canonical_skill_operation
def find_latest_activation_snapshot(
    skill_dir: SkillRootInput,
    backend: SigningBackend,
    *,
    policy: str,
) -> tuple[str, str] | None:
    """Return ``(version_id, target)`` for the current exposure summary."""
    validate_activation_policy(policy)
    root = resolve_skill_root(skill_dir)
    validate_resolved_skill_root(root)
    status_result = check(root, backend)
    summary = build_exposure_summary(
        root,
        backend,
        status_result=status_result,
    )
    version_id = summary["activeVersionId"]
    target = summary["target"]
    if version_id is None or target is None:
        return None
    return str(version_id), str(target)


def write_activation_contract(
    skill_dir: str | Path, activation: dict[str, Any]
) -> dict[str, Any]:
    """Atomically write the activation contract and best-effort xattr."""
    contract = _minimal_activation_contract(activation)
    meta_dir = ensure_skill_meta(skill_dir)
    path = meta_dir / ACTIVATION_JSON
    _atomic_write_json(path, contract)
    return write_activation_xattr(skill_dir, contract)


def write_activation_xattr(
    skill_dir: str | Path, activation: dict[str, Any]
) -> dict[str, Any]:
    """Best-effort write of the activation contract to the skill directory xattr."""
    setxattr = getattr(os, "setxattr", None)
    if setxattr is None:
        return _activation_xattr_status(
            written=False,
            available=False,
            error="os.setxattr unavailable",
        )

    contract = _minimal_activation_contract(activation)
    payload = serialize_activation_contract(contract)
    try:
        setxattr(str(Path(skill_dir)), ACTIVATION_XATTR, payload)
    except OSError as exc:
        return _activation_xattr_status(
            written=False,
            available=True,
            error=f"{type(exc).__name__}: {exc}",
        )
    return _activation_xattr_status(written=True, available=True)


def _minimal_activation_contract(activation: dict[str, Any]) -> dict[str, Any]:
    return {"schemaVersion": SCHEMA_VERSION, "target": activation.get("target")}


def serialize_activation_contract(activation: dict[str, Any]) -> bytes:
    """Return the stable UTF-8 JSON payload used by file and xattr contracts."""
    contract = _minimal_activation_contract(activation)
    return json.dumps(
        contract,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def _activation_xattr_status(
    *,
    written: bool,
    available: bool | None = None,
    error: str | None = None,
    skipped: bool = False,
) -> dict[str, Any]:
    status: dict[str, Any] = {
        "name": ACTIVATION_XATTR,
        "written": written,
    }
    if available is not None:
        status["available"] = available
    if error is not None:
        status["error"] = error
    if skipped:
        status["skipped"] = True
    return status


def _canonicalize_activation_xattr(
    root: ResolvedSkillRoot,
    status: dict[str, Any],
) -> dict[str, Any]:
    error = status.get("error")
    if not isinstance(error, str):
        return status
    return {**status, "error": root.canonicalize_message(error)}


def _atomic_write_json(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(dir=path.parent, suffix=".tmp")
    try:
        with open(fd, "wb") as fh:
            fh.write(serialize_activation_contract(data))
            fh.flush()
        os.replace(tmp_path, path)
    except BaseException:
        Path(tmp_path).unlink(missing_ok=True)
        raise
