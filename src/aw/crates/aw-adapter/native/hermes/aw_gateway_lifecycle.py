"""Retire Gateway registrations only after native admissions reach a terminal state."""

from contextlib import nullcontext
from pathlib import Path
from typing import Any


def _authority() -> Any:
    from gateway.run_api import _ACTIVE_RUNNER
    from gateway.session_authorities import active_authority

    return active_authority(_ACTIVE_RUNNER) if _ACTIVE_RUNNER is not None else None


def admission_terminal(session_id: str, admission_id: str, authority: Any = None) -> bool:
    """Confirm a previous admission's terminal status in the native ledger."""
    from hermes_state_runtime import get_session_admission

    owner = authority if authority is not None else _authority()
    if owner is None or getattr(owner, "db", None) is None:
        return False
    row = get_session_admission(owner.db, admission_id=admission_id)
    return (row is not None and row.get("target_session_id") == session_id
            and row.get("status") == "terminal")


def _closed_and_idle(session_id: str) -> bool:
    from hermes_state_runtime import list_session_admissions

    authority = _authority()
    if authority is None:
        return False
    row = authority.db.get_session(session_id)
    return ((row is None or row.get("ended_at") is not None)
            and not list_session_admissions(authority.db, session_id=session_id))


def register_retirement(ctx: Any, known: dict, active: dict, release: Any,
                        lock: Any, load: Any) -> None:
    """Register the idempotent retirement RPC for an AW-owned Gateway session."""

    def retire(params: dict, *, session_id: str | None = None) -> dict:
        if (not isinstance(params, dict)
                or set(params) != {"target_session_id", "launch_path", "token"}
                or session_id is not None
                or not isinstance(params["target_session_id"], str)
                or not 1 <= len(params["target_session_id"]) <= 512
                or not isinstance(params["launch_path"], str)
                or not isinstance(params["token"], str)):
            raise ValueError("invalid AW Hermes Gateway session retirement")
        from hermes_cli.version_info import get_version_info
        from hermes_constants import get_hermes_home

        path = params["launch_path"]
        launch = load(path)
        if (launch["token"] != params["token"]
                or get_version_info().commit != launch["revision"]
                or get_hermes_home().resolve() != Path(launch["profile"])):
            raise ValueError("AW Hermes Gateway launch identity differs")
        target = params["target_session_id"]
        manager = getattr(ctx, "_manager", None)
        discovery = getattr(manager, "_discovery_lock", None)
        with discovery if discovery is not None else nullcontext(), lock:
            registration = known.get(target)
            if registration is None or registration[0] != path:
                return {"retired": True}
            if registration[1] != launch:
                raise ValueError("AW Hermes Gateway launch snapshot differs")
            binding = active.get(target)
            if binding is not None:
                if not admission_terminal(target, binding.admission_id):
                    return {"retired": False}
                release(binding)
            if not _closed_and_idle(target):
                return {"retired": False}
            known.pop(target)
        return {"retired": True}

    ctx.register_rpc("retire", retire)
