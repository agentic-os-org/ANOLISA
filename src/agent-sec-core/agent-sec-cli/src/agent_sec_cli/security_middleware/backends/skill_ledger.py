"""Skill-ledger backend — dispatch to skill-ledger core operations.

Routes the ``command`` kwarg to the appropriate handler, returning a
unified :class:`ActionResult`.
"""

import copy
import json
import re
from typing import Any

from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult
from agent_sec_cli.skill_ledger.activation_policy import (
    validate_activation_policy,
)
from agent_sec_cli.skill_ledger.config import load_config, resolve_skill_dirs
from agent_sec_cli.skill_ledger.core.auditor import audit
from agent_sec_cli.skill_ledger.core.certifier import (
    certify,
    scan_batch,
    scan_skill,
)
from agent_sec_cli.skill_ledger.core.checker import check, check_batch
from agent_sec_cli.skill_ledger.core.decision import (
    clear_decision,
    decide_skill,
    export_skill,
    show_skill,
)
from agent_sec_cli.skill_ledger.core.status import ledger_status
from agent_sec_cli.skill_ledger.errors import ConfigError
from agent_sec_cli.skill_ledger.scanner.names import validate_scanner_name
from agent_sec_cli.skill_ledger.scanner.registry import ScannerRegistry
from agent_sec_cli.skill_ledger.signing.ed25519 import NativeEd25519Backend
from agent_sec_cli.skill_ledger.signing.key_manager import (
    archive_current_public_key,
    ensure_keys_not_exist,
    keys_exist,
)

_UNENCRYPTED_AUTO_KEY_WARNING = (
    "Warning: created an unencrypted Skill Ledger signing key. Run "
    "'agent-sec-cli skill-ledger init --force-keys --passphrase' to enable "
    "passphrase protection."
)
_PASSPHRASE_EXISTING_KEY_ERROR = (
    "key already exists; use "
    "'agent-sec-cli skill-ledger init --force-keys --passphrase' to rotate it "
    "with passphrase protection."
)
_CAMEL_ACRONYM_BOUNDARY_RE = re.compile(r"(.)([A-Z][a-z]+)")
_CAMEL_WORD_BOUNDARY_RE = re.compile(r"([a-z0-9])([A-Z])")
_VERDICT_SEVERITY = {
    "pass": 0,
    "none": 1,
    "warn": 2,
    "unmanaged": 3,
    "drifted": 4,
    "deny": 5,
    "tampered": 6,
    "error": 7,
}


def _to_snake_case(value: str) -> str:
    """Convert a JSON field name from camelCase/PascalCase to snake_case."""
    value = value.replace("-", "_")
    value = _CAMEL_ACRONYM_BOUNDARY_RE.sub(r"\1_\2", value)
    return _CAMEL_WORD_BOUNDARY_RE.sub(r"\1_\2", value).lower()


def _snake_case_event_keys(value: Any) -> Any:
    """Return *value* with Skill Ledger event result keys normalized.

    Scanner finding ``metadata`` is intentionally opaque because its contents are
    scanner-owned rather than part of the Skill Ledger event contract.
    """
    if isinstance(value, list):
        return [_snake_case_event_keys(item) for item in value]
    if not isinstance(value, dict):
        return value

    normalized: dict[str, Any] = {}
    scan_status: Any = None
    has_scan_status = False
    for key, item in value.items():
        if key == "metadata":
            normalized[key] = copy.deepcopy(item)
            continue
        if key == "scanStatus":
            scan_status = item
            has_scan_status = True
            continue
        normalized[_to_snake_case(key)] = _snake_case_event_keys(item)

    if has_scan_status:
        normalized["verdict"] = scan_status
    return normalized


def _known_verdict(value: Any) -> str | None:
    """Return a recognized Skill Ledger verdict value."""
    if isinstance(value, str) and value in _VERDICT_SEVERITY:
        return value
    return None


def _batch_verdict(results: Any) -> str | None:
    """Return the most severe verdict from a batch command result."""
    if not isinstance(results, list):
        return None

    verdicts: list[str] = []
    for item in results:
        if not isinstance(item, dict):
            continue
        for value in (item.get("status"), item.get("verdict")):
            verdict = _known_verdict(value)
            if verdict is not None:
                verdicts.append(verdict)

    if not verdicts:
        return None
    return max(verdicts, key=_VERDICT_SEVERITY.__getitem__)


def _project_event_verdict(data: dict[str, Any]) -> str | None:
    """Project command-specific output into the canonical event verdict."""
    command = data.get("command")
    if not isinstance(command, str):
        return None

    if command in {"init", "scan", "check"} and "results" in data:
        return _batch_verdict(data.get("results"))
    if command == "show":
        return _known_verdict(data.get("latest_status"))
    if command == "check":
        return _known_verdict(data.get("status"))
    if command in {"scan", "certify"}:
        return _known_verdict(data.get("verdict"))
    if command == "decide":
        return _known_verdict(data.get("current_status")) or _known_verdict(
            data.get("verdict")
        )
    return None


def _normalize_event_result(data: dict[str, Any]) -> dict[str, Any]:
    """Build the Skill Ledger event-only result payload."""
    normalized = _snake_case_event_keys(data)
    verdict = _project_event_verdict(normalized)
    if verdict is not None:
        normalized["verdict"] = verdict
    return normalized


class SkillLedgerBackend(BaseBackend):
    """Dispatch backend for all skill-ledger subcommands."""

    @staticmethod
    def _sanitize_request(kwargs: dict[str, Any]) -> dict[str, Any]:
        """Return a log-safe copy of request kwargs."""
        request = copy.deepcopy(kwargs)
        if request.get("passphrase") is not None:
            request["passphrase"] = "[REDACTED]"
        return request

    def build_event_details(
        self, result: ActionResult, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build skill-ledger audit details without logging key passphrases."""
        result_data = copy.deepcopy(result.data)
        return {
            "request": self._sanitize_request(kwargs),
            "result": _normalize_event_result(result_data),
        }

    def build_error_details(
        self, exception: Exception, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build skill-ledger failure audit details without logging key passphrases."""
        return {
            "request": self._sanitize_request(kwargs),
            "error": str(exception),
            "error_type": type(exception).__name__,
        }

    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        """Dispatch to the handler identified by ``command``."""
        command = kwargs.pop("command", "")
        handler_name = f"_do_{command.replace('-', '_')}"
        handler = getattr(self, handler_name, None)
        if handler is None:
            return ActionResult(
                success=False,
                error=f"Unknown skill-ledger command: {command!r}",
                exit_code=1,
                error_type="ValueError",
            )
        try:
            # Validate before handlers can initialize keys or mutate ledger state.
            load_config()
            for name in kwargs.get("scanner_names") or []:
                validate_scanner_name(name)
            if "scanner" in kwargs:
                validate_scanner_name(kwargs["scanner"])
            if kwargs.get("policy") is not None:
                validate_activation_policy(kwargs["policy"])
        except (ConfigError, ValueError) as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )
        return handler(ctx, **kwargs)

    # ------------------------------------------------------------------
    # Handlers
    # ------------------------------------------------------------------

    def _generate_keys(
        self, *, force: bool = False, passphrase: str | None = None
    ) -> dict:
        """Generate key material and return the backend result dict."""
        ensure_keys_not_exist(force=force)
        # Archive the old public key into the keyring so that existing
        # signatures remain verifiable after key rotation.
        if force:
            try:
                archive_current_public_key()
            except Exception as exc:
                raise RuntimeError(
                    f"failed to archive existing public key before rotation: {exc}"
                ) from exc
        backend = NativeEd25519Backend()
        return backend.generate_keys(passphrase)

    def _ensure_keys(self) -> tuple[bool, dict[str, Any] | None, list[str]]:
        """Create default unencrypted keys when absent."""
        if keys_exist():
            return False, None, []
        result = self._generate_keys(force=False, passphrase=None)
        warnings = []
        if result.get("encrypted") is False:
            warnings.append(_UNENCRYPTED_AUTO_KEY_WARNING)
        return True, result, warnings

    def _do_init(
        self,
        ctx: RequestContext,
        *,
        baseline: bool = True,
        passphrase: str | None = None,
        passphrase_requested: bool = False,
        force_keys: bool = False,
        scanner_names: list[str] | None = None,
        **kw: Any,
    ) -> ActionResult:
        key_created = False
        key_result: dict[str, Any] | None = None
        try:
            if passphrase_requested and keys_exist() and not force_keys:
                return ActionResult(
                    success=False,
                    error=_PASSPHRASE_EXISTING_KEY_ERROR,
                    exit_code=1,
                    error_type="ValueError",
                )
            if force_keys or not keys_exist():
                key_result = self._generate_keys(
                    force=force_keys, passphrase=passphrase
                )
                key_created = True

            results: list[dict[str, Any]] = []
            if baseline:
                dirs = resolve_skill_dirs()
                if dirs:
                    backend = NativeEd25519Backend()
                    results = scan_batch(dirs, backend, scanner_names=scanner_names)
            has_error = any(r.get("status") == "error" for r in results)
            data = {
                "command": "init",
                "keyCreated": key_created,
                "key": key_result,
                "baseline": baseline,
                "results": results,
            }
            return ActionResult(
                success=not has_error,
                stdout=json.dumps(data, ensure_ascii=False) + "\n",
                data=data,
                exit_code=1 if has_error else 0,
                error_type="SkillLedgerError" if has_error else "",
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_check(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str | None = None,
        all_skills: bool = False,
        **kw: Any,
    ) -> ActionResult:
        backend = NativeEd25519Backend()

        try:
            if all_skills:
                dirs = resolve_skill_dirs()
                if not dirs:
                    return ActionResult(
                        success=False,
                        error="No skill directories found in config.json",
                        exit_code=1,
                        error_type="FileNotFoundError",
                    )
                results = check_batch(dirs, backend)
                has_error = any(r.get("status") == "error" for r in results)
                has_critical = any(
                    r.get("status") in ("tampered", "deny", "error") for r in results
                )
                data = {"command": "check", "results": results}
                return ActionResult(
                    success=not has_critical,
                    stdout=json.dumps({"results": results}, ensure_ascii=False) + "\n",
                    data=data,
                    exit_code=1 if has_critical else 0,
                    error_type="SkillLedgerError" if has_error else "",
                )
            else:
                if skill_dir is None:
                    return ActionResult(
                        success=False,
                        error="skill_dir is required (or use --all)",
                        exit_code=1,
                        error_type="ValueError",
                    )
                result = check(skill_dir, backend)
                status = result.get("status", "")
                is_critical = status in ("tampered", "deny")
                return ActionResult(
                    success=not is_critical,
                    stdout=json.dumps(result, ensure_ascii=False) + "\n",
                    data={"command": "check", **result},
                    exit_code=1 if is_critical else 0,
                )
        except Exception as exc:
            error_data = {"status": "error", "error": str(exc)}
            return ActionResult(
                success=False,
                stdout=json.dumps(error_data, ensure_ascii=False) + "\n",
                data={"command": "check", **error_data},
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_certify(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str | None = None,
        all_skills: bool = False,
        findings: str | None = None,
        scanner: str = "skill-vetter",
        scanner_version: str | None = None,
        scanner_names: list[str] | None = None,
        delete_findings: bool = False,
        **kw: Any,
    ) -> ActionResult:
        try:
            if all_skills or scanner_names:
                return ActionResult(
                    success=False,
                    error="certify imports external findings only; use 'skill-ledger scan' for built-in scanners",
                    exit_code=1,
                    error_type="ValueError",
                )
            if skill_dir is None:
                return ActionResult(
                    success=False,
                    error="skill_dir is required",
                    exit_code=1,
                    error_type="ValueError",
                )
            if findings is None:
                return ActionResult(
                    success=False,
                    error="--findings is required for certify; use 'skill-ledger scan' for built-in scanners",
                    exit_code=1,
                    error_type="ValueError",
                )
            key_created, key_result, warnings = self._ensure_keys()
            backend = NativeEd25519Backend()
            result = certify(
                skill_dir,
                backend,
                findings_path=findings,
                scanner=scanner,
                scanner_version=scanner_version,
                delete_findings=delete_findings,
            )
            result["keyCreated"] = key_created
            if key_result is not None:
                result["key"] = key_result
            if warnings:
                result["warnings"] = warnings
            return ActionResult(
                success=True,
                stdout=json.dumps(result, ensure_ascii=False) + "\n",
                data={"command": "certify", **result},
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_scan(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str | None = None,
        all_skills: bool = False,
        scanner_names: list[str] | None = None,
        force: bool = False,
        **kw: Any,
    ) -> ActionResult:
        try:
            if all_skills:
                if skill_dir is not None:
                    return ActionResult(
                        success=False,
                        error="--all and skill_dir are mutually exclusive.",
                        exit_code=1,
                        error_type="ValueError",
                    )
                dirs = resolve_skill_dirs()
                if not dirs:
                    return ActionResult(
                        success=False,
                        error="No skill directories found in config.json",
                        exit_code=1,
                        error_type="FileNotFoundError",
                    )
                key_created, key_result, warnings = self._ensure_keys()
                backend = NativeEd25519Backend()
                results = scan_batch(
                    dirs,
                    backend,
                    scanner_names=scanner_names,
                    force=force,
                )
                has_error = any(r.get("status") == "error" for r in results)
                data = {
                    "command": "scan",
                    "keyCreated": key_created,
                    "results": results,
                }
                if key_result is not None:
                    data["key"] = key_result
                if warnings:
                    data["warnings"] = warnings
                return ActionResult(
                    success=not has_error,
                    stdout=json.dumps(data, ensure_ascii=False) + "\n",
                    data=data,
                    exit_code=1 if has_error else 0,
                    error_type="SkillLedgerError" if has_error else "",
                )
            if skill_dir is None:
                return ActionResult(
                    success=False,
                    error="skill_dir is required (or use --all)",
                    exit_code=1,
                    error_type="ValueError",
                )
            key_created, key_result, warnings = self._ensure_keys()
            backend = NativeEd25519Backend()
            result = scan_skill(
                skill_dir,
                backend,
                scanner_names=scanner_names,
                force=force,
            )
            result["keyCreated"] = key_created
            if key_result is not None:
                result["key"] = key_result
            if warnings:
                result["warnings"] = warnings
            return ActionResult(
                success=True,
                stdout=json.dumps(result, ensure_ascii=False) + "\n",
                data={"command": "scan", **result},
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_status(
        self,
        ctx: RequestContext,
        *,
        verbose: bool = False,
        **kw: Any,
    ) -> ActionResult:
        backend = NativeEd25519Backend()

        try:
            result = ledger_status(backend, verbose=verbose)
            data = {"command": "status", **result}
            return ActionResult(
                success=True,
                stdout=json.dumps(data, ensure_ascii=False) + "\n",
                data=data,
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_audit(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str,
        verify_snapshots: bool = False,
        **kw: Any,
    ) -> ActionResult:
        backend = NativeEd25519Backend()

        try:
            result = audit(skill_dir, backend, verify_snapshots=verify_snapshots)
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

        return ActionResult(
            success=result["valid"],
            stdout=json.dumps(result, ensure_ascii=False) + "\n",
            data={"command": "audit", **result},
            exit_code=0 if result["valid"] else 1,
        )

    def _do_list_scanners(self, ctx: RequestContext, **kw: Any) -> ActionResult:
        registry = ScannerRegistry.from_config()
        scanners = registry.list_scanners(enabled_only=False)

        scanner_data = []
        for s in scanners:
            scanner_data.append(
                {
                    "name": s.name,
                    "type": s.type,
                    "parser": s.parser,
                    "enabled": s.enabled,
                    "autoInvocable": s.enabled and s.type == "builtin",
                    "description": s.description,
                }
            )

        data = {"command": "list-scanners", "scanners": scanner_data}
        return ActionResult(
            success=True,
            stdout=json.dumps(data, ensure_ascii=False) + "\n",
            data=data,
        )

    def _do_decide(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str,
        decision_action: str | None = None,
        target_version_id: str | None = None,
        reason: str | None = None,
        clear: bool = False,
        **kw: Any,
    ) -> ActionResult:
        try:
            key_created, key_result, warnings = self._ensure_keys()
            backend = NativeEd25519Backend()
            if clear:
                result = clear_decision(skill_dir, backend)
            else:
                if decision_action is None:
                    return ActionResult(
                        success=False,
                        error="--action is required unless --clear is set",
                        exit_code=1,
                        error_type="ValueError",
                    )
                result = decide_skill(
                    skill_dir,
                    backend,
                    action=decision_action,
                    target_version_id=target_version_id,
                    reason=reason,
                )
            result["keyCreated"] = key_created
            if key_result is not None:
                result["key"] = key_result
            if warnings:
                result["warnings"] = warnings
            data = {"command": "decide", **result}
            return ActionResult(
                success=True,
                stdout=json.dumps(data, ensure_ascii=False) + "\n",
                data=data,
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_show(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str,
        policy: str | None = None,
        **kw: Any,
    ) -> ActionResult:
        backend = NativeEd25519Backend()
        try:
            result = show_skill(skill_dir, backend, policy=policy)
            data = {"command": "show", **result}
            return ActionResult(
                success=True,
                stdout=json.dumps(data, ensure_ascii=False) + "\n",
                data=data,
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )

    def _do_export(
        self,
        ctx: RequestContext,
        *,
        skill_dir: str,
        version: str,
        output: str,
        policy: str | None = None,
        **kw: Any,
    ) -> ActionResult:
        backend = NativeEd25519Backend()
        try:
            result = export_skill(
                skill_dir,
                backend,
                version=version,
                output=output,
                policy=policy,
            )
            data = {"command": "export", **result}
            return ActionResult(
                success=True,
                stdout=json.dumps(data, ensure_ascii=False) + "\n",
                data=data,
            )
        except Exception as exc:
            return ActionResult(
                success=False,
                error=str(exc),
                exit_code=1,
                error_type=type(exc).__name__,
            )
