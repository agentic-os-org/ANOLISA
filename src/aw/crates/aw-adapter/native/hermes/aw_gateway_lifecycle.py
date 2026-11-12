"""Retire Gateway session registrations after native admission drains."""

from contextlib import nullcontext
from typing import Any


def _closed_and_idle(session_id: str) -> bool:
    from gateway.run_api import _ACTIVE_RUNNER
    from gateway.session_authorities import active_authority
    from hermes_state_runtime import list_session_admissions

    authority = active_authority(_ACTIVE_RUNNER) if _ACTIVE_RUNNER is not None else None
    if authority is None:
        return False
    row = authority.db.get_session(session_id)
    return ((row is None or row.get("ended_at") is not None)
            and not list_session_admissions(authority.db, session_id=session_id))


def register_retirement(ctx: Any, known: dict, retired: dict, leases: dict, lock: Any) -> None:
    """Register idempotent retirement RPCs for an AW-owned Gateway session."""

    def validate(params: dict, session_id: str | None) -> str:
        if (not isinstance(params, dict)
                or set(params) != {"target_session_id", "launch_path", "token"}
                or session_id is not None
                or not isinstance(params["target_session_id"], str)
                or not 1 <= len(params["target_session_id"]) <= 512
                or not isinstance(params["launch_path"], str)
                or not isinstance(params["token"], str)):
            raise ValueError("invalid AW Hermes Gateway session retirement")
        return params["target_session_id"]

    def retire(params: dict, *, session_id: str | None = None) -> dict:
        target = validate(params, session_id)
        manager = getattr(ctx, "_manager", None)
        discovery = getattr(manager, "_discovery_lock", None)
        with discovery if discovery is not None else nullcontext(), lock:
            registration = known.get(target)
            if registration is None:
                if retired.get(target) == (params["launch_path"], params["token"]):
                    return {"retired": True}
                raise ValueError("AW Hermes Gateway session was not registered")
            if (registration[0] != params["launch_path"]
                    or registration[1]["token"] != params["token"]):
                raise ValueError("AW Hermes Gateway session retirement differs from registration")
            if target in leases or not _closed_and_idle(target):
                return {"retired": False}
            known.pop(target)
            retired[target] = params["launch_path"], params["token"]
        return {"retired": True}

    def acknowledge_retire(params: dict, *, session_id: str | None = None) -> dict:
        target = validate(params, session_id)
        with lock:
            if retired.get(target) == (params["launch_path"], params["token"]):
                retired.pop(target)
        return {"acknowledged": True}

    ctx.register_rpc("retire", retire)
    ctx.register_rpc("acknowledge_retire", acknowledge_retire)
