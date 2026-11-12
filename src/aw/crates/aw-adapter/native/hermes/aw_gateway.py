"""Bind a Gateway-owned Hermes conversation to one verified AW launch."""

from contextlib import nullcontext
from contextvars import ContextVar
import inspect
import json
import math
from pathlib import Path
import shlex
import sys
from threading import RLock
from typing import Any
from uuid import uuid4

from . import aw_skill
from .aw_gateway_lifecycle import register_retirement


_bindings: dict[str, "Binding"] = {}
_roots: dict[str, "Binding"] = {}
_leases: dict[str, tuple["Binding", str]] = {}
_known: dict[str, tuple[str, dict[str, Any], Any, Any]] = {}
_retired: dict[str, tuple[str, str]] = {}
_checked: dict[tuple[str, str, str], tuple[str, dict | None]] = {}
_lock = RLock()
_restores = []
_skill_restore = None
_manager_restore = None
_turn_restore = None
_alive = False
_expected_input: ContextVar[str | None] = ContextVar("aw_hermes_expected_input", default=None)
_bridge_call: ContextVar[tuple[str, str, str] | None] = ContextVar(
    "aw_hermes_bridge_call", default=None)


class _CheckedInputChanged(ValueError):
    pass


def _json_native(value: Any) -> bool:
    if type(value) is dict:
        return all(type(key) is str and _json_native(key) and _json_native(child)
                   for key, child in value.items())
    if type(value) is list:
        return all(_json_native(child) for child in value)
    if type(value) is int:
        return -(1 << 63) <= value < (1 << 64)
    if type(value) is float:
        return math.isfinite(value)
    if type(value) is str:
        try:
            value.encode("utf-8")
        except UnicodeEncodeError:
            return False
        return True
    return type(value) in (bool, type(None))


def _identity(values: dict[str, Any], binding: "Binding") -> tuple[str, str, str]:
    # Concurrent pre hooks omit conversation_id, and compaction rotates the
    # physical session_id. The verified binding keeps the call key stable.
    session = binding.identity
    request, call = values.get("api_request_id"), values.get("tool_call_id")
    if any(not isinstance(value, str) or not value or len(value) > 512
           for value in (session, request, call)):
        raise ValueError("Hermes tool call has no bounded session/request/call identity")
    return session, request, call


def _bound(values: dict[str, Any]) -> "Binding | None":
    with _lock:
        session = values.get("session_id")
        conversation = values.get("conversation_id")
        if isinstance(session, str) and session:
            binding = _bindings.get(session)
            if binding is None:
                if isinstance(conversation, str) and conversation in _roots:
                    raise ValueError("Hermes compressed session has no verified AW lineage")
                return None
            if isinstance(conversation, str) and conversation:
                root = _roots.get(conversation)
                if root is not None and root is not binding:
                    raise ValueError("Hermes conversation belongs to another AW launch")
                _roots[conversation] = binding
            return binding
        return _roots.get(conversation) if isinstance(conversation, str) else None


def _release(binding: "Binding") -> None:
    for identity, owner in list(_bindings.items()):
        if owner is binding:
            _bindings.pop(identity)
    for identity, owner in list(_roots.items()):
        if owner is binding:
            _roots.pop(identity)
    for key in list(_checked):
        if key[0] == binding.identity:
            _checked.pop(key)
    _leases.pop(binding.identity, None)


class Binding:
    def __init__(self, path: str, launch: dict[str, Any], ctx: Any, load: Any):
        from agent.shell_hooks import ShellHookSpec

        self.path, self.launch, self.ctx, self.load = path, launch, ctx, load
        self.identity = ""
        self.submission_id = ""
        self.before, self.after, self.replacement = [], [], None
        seen = set()
        for hook in launch["hooks"]:
            event, step, action, timeout = (
                hook.get("event"), hook.get("step"), hook.get("on_error"), hook.get("timeout"))
            if (event not in ("tool.before", "tool.after") or not isinstance(step, str)
                    or not step or action not in ("block", "report")
                    or isinstance(timeout, bool) or not isinstance(timeout, int)
                    or not 1 <= timeout <= 300 or (event, step) in seen):
                raise ValueError("invalid AW Hermes Gateway callback registration")
            seen.add((event, step))
            command = shlex.join([
                launch["binary"], "hook", "--binding", launch["binding"],
                "--adapter", "hermes", "--event", event, "--step", step,
                "--on-error", action,
            ])
            spec = ShellHookSpec(
                event="pre_tool_call" if event == "tool.before" else "post_tool_call",
                command=command, timeout=timeout, fail_closed=action == "block")
            if hook.get("replace_result"):
                if event != "tool.after" or self.replacement is not None:
                    raise ValueError("Hermes allows one tool result replacement step")
                self.replacement = (hook, spec)
            elif event == "tool.before":
                self.before.append((hook, spec))
            else:
                self.after.append((hook, spec))

    def check(self) -> None:
        current = self.load(self.path)
        if current != self.launch:
            raise ValueError("AW Hermes Gateway launch changed after binding")
        cwd = self.ctx.session_cwd()
        if not cwd or Path(cwd).resolve() != Path(self.launch["cwd"]):
            raise ValueError("Hermes Gateway session cwd differs from AW binding")

    def invoke(self, hook: dict, spec: Any, event: str, values: dict[str, Any]):
        from agent.shell_hooks import _evaluate_result, _serialize_payload, _spawn

        payload = json.loads(_serialize_payload(event, values))
        payload["cwd"] = self.launch["cwd"]
        response = _spawn(spec, json.dumps(payload, ensure_ascii=False))
        return _evaluate_result(spec, response), response


def register(ctx: Any, load: Any) -> None:
    """Install dormant Gateway callbacks; an authorized session RPC activates one launch."""
    global _alive
    from hermes_cli.version_info import get_version_info
    from hermes_constants import get_hermes_home

    _alive = True

    def prime(params: dict, *, session_id: str | None = None) -> dict:
        if (not isinstance(params, dict)
                or set(params) != {"session_id", "launch_path", "token"}
                or not isinstance(session_id, str) or not 1 <= len(session_id) <= 512
                or params["session_id"] != session_id
                or not isinstance(params["launch_path"], str)
                or not isinstance(params["token"], str)):
            raise ValueError("invalid AW Hermes Gateway session registration")
        path = params["launch_path"]
        launch = load(path)
        if (launch["token"] != params["token"]
                or get_version_info().commit != launch["revision"]
                or get_hermes_home().resolve() != Path(launch["profile"])):
            raise ValueError("AW Hermes Gateway launch identity differs")
        binding = Binding(path, launch, ctx, load)
        binding.identity = session_id
        binding.check()
        manager = getattr(ctx, "_manager", None)
        discovery = getattr(manager, "_discovery_lock", None)
        with discovery if discovery is not None else nullcontext(), _lock:
            if not _alive:
                raise ValueError("AW Hermes Gateway plugin was unloaded")
            known = _known.get(session_id)
            if known is not None and (known[0] != path or known[1] != launch):
                raise ValueError("Hermes session already belongs to another AW launch")
            _guard_manager(ctx)
            _known[session_id] = path, launch, ctx, load
            _retired.pop(session_id, None)
        return {"adapter": "hermes", "token": launch["token"],
                "hooks": len(launch["hooks"]), "gateway_pid": __import__("os").getpid()}

    def bind(params: dict, *, session_id: str | None = None) -> dict:
        if (not isinstance(session_id, str) or not session_id or len(session_id) > 512
                or set(params) != {"session_id", "launch_path", "token", "submission_id"}
                or params["session_id"] != session_id
                or not isinstance(params["launch_path"], str)
                or not isinstance(params["token"], str)
                or not isinstance(params["submission_id"], str)
                or not 1 <= len(params["submission_id"]) <= 512):
            raise ValueError("invalid AW Hermes Gateway session binding")
        path = params["launch_path"]
        launch = load(path)
        if (launch["token"] != params["token"]
                or get_version_info().commit != launch["revision"]
                or get_hermes_home().resolve() != Path(launch["profile"])):
            raise ValueError("AW Hermes Gateway launch identity differs")
        binding = Binding(path, launch, ctx, load)
        manager = getattr(ctx, "_manager", None)
        discovery = getattr(manager, "_discovery_lock", None)
        with discovery if discovery is not None else nullcontext(), _lock:
            if not _alive:
                raise ValueError("AW Hermes Gateway plugin was unloaded")
            active = _leases.get(session_id)
            if active is not None:
                if active[0].path != path or active[0].launch != launch:
                    raise ValueError("Hermes session already belongs to another AW launch")
                return {"adapter": "hermes", "token": launch["token"],
                        "hooks": len(launch["hooks"]), "gateway_pid": __import__("os").getpid(),
                        "lease": active[1],
                        "adopted": active[0].submission_id != params["submission_id"]}
            known = _known.get(session_id)
            if known is not None and (known[0] != path or known[1] != launch):
                raise ValueError("Hermes session already belongs to another AW launch")
            for identity, active in list(_bindings.items()):
                if not Path(active.path).exists():
                    _bindings.pop(identity, None)
            previous = _bindings.get(session_id)
            if previous is not None and previous.launch != launch:
                try:
                    previous.load(previous.path)
                except OSError:
                    pass
                else:
                    raise ValueError("Hermes session already belongs to another AW launch")
            binding.identity = session_id
            binding.submission_id = params["submission_id"]
            _install_guards(binding)
            _guard_manager(ctx)
            _bindings[session_id] = binding
            _known[session_id] = path, launch, ctx, load
            lease = uuid4().hex
            _leases[session_id] = (binding, lease)
        return {"adapter": "hermes", "token": launch["token"],
                "hooks": len(launch["hooks"]), "gateway_pid": __import__("os").getpid(),
                "lease": lease}

    def finish(params: dict, *, session_id: str | None = None) -> dict:
        if (not isinstance(params, dict) or set(params) != {"session_id", "token", "lease", "unadmitted"}
                or not isinstance(session_id, str) or params["session_id"] != session_id
                or not isinstance(params["token"], str) or not isinstance(params["lease"], str)
                or params["unadmitted"] is not True):
            raise ValueError("invalid AW Hermes Gateway finish request")
        with _lock:
            active = _leases.get(session_id)
            if active is None:
                return {"finished": True}
            if active[1] != params["lease"] or active[0].launch["token"] != params["token"]:
                raise ValueError("AW Hermes Gateway turn lease differs")
            _release(active[0])
        return {"finished": True}

    def compact(**values: Any) -> None:
        old, new = values.get("old_session_id"), values.get("session_id")
        if not all(isinstance(value, str) and value and len(value) <= 512
                   for value in (old, new)):
            return
        with _lock:
            binding = _bindings.get(old)
            if binding is None:
                return
            if new in _bindings and _bindings[new] is not binding:
                raise ValueError("Hermes compressed session belongs to another AW launch")
            _bindings[new] = binding

    def pre(**values: Any) -> dict | None:
        try:
            binding = _bound(values)
            if binding is None or not binding.before:
                return None
            binding.check()
            key = _identity(values, binding)
            effective = dict(values.get("args") or {})
            if not _json_native(values.get("args")):
                raise ValueError("Hermes tool arguments contain unsupported JSON values")
            expected = json.dumps(effective, sort_keys=True, allow_nan=False)
            replacement = None
            blocked = None
            approval = None
            with aw_skill.invocation(values.get("tool_name"), values.get("args")) as selected:
                for hook, spec in binding.before:
                    def invoke(**input_values: Any):
                        result, response = binding.invoke(hook, spec, "pre_tool_call", input_values)
                        if isinstance(result, dict) and result.get("action") == "modify":
                            try:
                                accepted = (response["returncode"] == 0 and
                                            json.loads(response["stdout"]).get("aw_checked_input") is True)
                            except (ValueError, TypeError, AttributeError):
                                accepted = False
                            if not accepted:
                                return {"action": "block", "message": "AW input replacement was not checked"}
                        return result

                    result = (aw_skill.callback(invoke, hook)(**values)
                              if hook.get("skill_check") else invoke(**values))
                    if isinstance(result, dict):
                        if result.get("action") == "block":
                            blocked = blocked or result
                        elif result.get("action") == "approve":
                            approval = approval or result
                        elif result.get("action") == "modify":
                            replacement = result
                            partial = result.get("args")
                            if isinstance(partial, dict) and partial:
                                effective.update(partial)
                                if not _json_native(effective):
                                    raise ValueError("Hermes tool arguments contain unsupported JSON values")
                                expected = json.dumps(effective, sort_keys=True, allow_nan=False)
                if blocked is None:
                    with _lock:
                        if len(_checked) > 4096:
                            _checked.pop(next(iter(_checked)))
                        _checked[key] = expected, selected
            if approval is not None and replacement is not None and blocked is None:
                return {"action": "block", "message": "AW input replacement conflicts with native approval"}
            return blocked or approval or replacement
        except (Exception, SystemExit) as error:
            print(f"AW Hermes Gateway pre-tool check failed: {error}", file=sys.stderr)
            return {"action": "block", "message": "AW Hermes tool check is unavailable"}

    def post(**values: Any) -> None:
        try:
            binding = _bound(values)
            if binding is None:
                return
            binding.check()
            for hook, spec in binding.after:
                binding.invoke(hook, spec, "post_tool_call", values)
        except (Exception, SystemExit) as error:
            print(f"AW Hermes Gateway post-tool report failed: {error}", file=sys.stderr)

    def transform(**values: Any) -> str | None:
        binding = None
        try:
            binding = _bound(values)
            if binding is None:
                return None
            if binding.replacement is None:
                return None
            _, spec = binding.replacement
            binding.check()
            payload = {"hook_event_name": "transform_tool_result", "cwd": binding.launch["cwd"],
                       "session_id": values.get("session_id"), "tool_name": values.get("tool_name"),
                       "tool_input": values.get("args"),
                       "extra": {**values, "result": values.get("result")}}
            from agent.shell_hooks import _spawn
            response = _spawn(spec, json.dumps(payload, ensure_ascii=False))
            if response["returncode"] != 0 or response["error"] or response["timed_out"]:
                raise ValueError("AW result replacement command failed")
            reply = json.loads(response["stdout"]) if response["stdout"].strip() else {}
            replacement = reply.get("aw_replacement")
            if not isinstance(replacement, str):
                raise ValueError("AW result replacement is missing")
            return replacement
        except (Exception, SystemExit) as error:
            print(f"AW Hermes Gateway result replacement failed: {error}", file=sys.stderr)
            return None
        finally:
            try:
                key = _identity(values, binding) if binding is not None else None
            except ValueError:
                pass
            else:
                if key is not None:
                    with _lock:
                        _checked.pop(key, None)

    ctx.register_hook("pre_tool_call", pre)
    ctx.register_hook("post_tool_call", post)
    ctx.register_hook("transform_tool_result", transform)
    ctx.register_hook("post_compaction", compact)
    ctx.register_rpc("bind", bind)
    ctx.register_rpc("prime", prime)
    ctx.register_rpc("finish", finish)
    register_retirement(ctx, _known, _retired, _leases, _lock)
    ctx.on_unload(_unload)
    _guard_turns()


def _guard_turns() -> None:
    global _turn_restore
    if _turn_restore is not None:
        return
    from gateway.session_authority import SessionAuthority
    original = SessionAuthority._execute_claimed

    async def execute_turn(authority: Any, ref: Any, live: Any, row: Any) -> Any:
        with _lock:
            known = _known.get(ref.session_id)
        manager = getattr(known[2], "_manager", None) if known is not None else None
        discovery = getattr(manager, "_discovery_lock", None)
        with discovery if discovery is not None else nullcontext(), _lock:
            active = _leases.get(ref.session_id)
            known = _known.get(ref.session_id)
            request_id = row.get("request_id") or row.get("admission_id")
            if known is not None:
                if not isinstance(request_id, str) or not 1 <= len(request_id) <= 512:
                    raise RuntimeError("AW Hermes claimed turn has no bounded submission identity")
                if active is None:
                    path, launch, ctx, load = known
                    binding = Binding(path, launch, ctx, load)
                    binding.identity = ref.session_id
                    binding.submission_id = request_id
                    binding.check()
                    _install_guards(binding)
                    _bindings[ref.session_id] = binding
                    _leases[ref.session_id] = (binding, uuid4().hex)
                elif active[0].submission_id != request_id:
                    raise RuntimeError("AW Hermes claimed turn differs from the bound submission")
        result = await original(authority, ref, live, row)
        # The Gateway owns admitted execution. A disconnected CLI can return
        # while this task continues; only the server's exit retires its binding.
        if result[1] != "settled_unknown":
            with _lock:
                active = _leases.get(ref.session_id)
                if active is not None and request_id == active[0].submission_id:
                    _release(active[0])
        return result

    SessionAuthority._execute_claimed = execute_turn
    _turn_restore = SessionAuthority, original, execute_turn


def _guard_manager(ctx: Any) -> None:
    global _manager_restore
    manager = getattr(ctx, "_manager", None)
    if manager is None or _manager_restore is not None:
        return
    original = manager.unload
    discovery = getattr(manager, "_discovery_lock", None)

    def unload(*args: Any, **kwargs: Any) -> Any:
        with discovery if discovery is not None else nullcontext(), _lock:
            if _known:
                raise RuntimeError("AW Hermes active turn or registered session prevents plugin unload")
            return original(*args, **kwargs)

    manager.unload = unload
    _manager_restore = manager, original, unload


def _install_guards(binding: Binding) -> None:
    global _skill_restore
    if _restores:
        if _skill_restore is None and any(hook.get("skill_check") for hook, _ in binding.before):
            _skill_restore = aw_skill.install()
        return
    import model_tools
    from agent import inline_tool_executors
    from hermes_cli import middleware, plugins
    from tools.registry import tool_error

    original_handle = model_tools.handle_function_call
    original_directives = plugins._dispatch_pre_tool_call_hooks
    original_execution = middleware.run_tool_execution_middleware
    original_coerce = model_tools.coerce_tool_args
    signature = inspect.signature(original_handle)

    def coerce(tool_name: str, arguments: dict) -> dict:
        effective = original_coerce(tool_name, arguments)
        expected = _expected_input.get()
        if expected is not None and (not _json_native(effective)
                                     or json.dumps(effective, sort_keys=True, allow_nan=False) != expected):
            raise _CheckedInputChanged("AW Hermes checked tool input changed during schema coercion")
        return effective

    def directives(tool_name: str, arguments: dict, **ids: Any) -> tuple[str | None, dict | None]:
        try:
            binding = _bound(ids)
            if binding is None or not binding.before:
                return original_directives(tool_name, arguments, **ids)
            if not _json_native(arguments):
                return "AW Hermes tool arguments contain unsupported JSON values", None
            blocked, changed = original_directives(tool_name, arguments, **ids)
            if blocked is not None:
                return blocked, changed
            key = _identity(ids, binding)
            with _lock:
                checked = _checked.get(key)
            effective = changed if changed is not None else arguments
            if (checked is None or not _json_native(effective)
                    or json.dumps(effective, sort_keys=True, allow_nan=False) != checked[0]):
                return "AW Hermes checked tool input changed", None
            return None, changed
        except (Exception, SystemExit) as error:
            print(f"AW Hermes Gateway pre-tool dispatch failed: {error}", file=sys.stderr)
            return "AW Hermes tool check is unavailable", None

    def handle(function_name: str, function_args: dict, *args: Any, **kwargs: Any) -> Any:
        values = signature.bind_partial(function_name, function_args, *args, **kwargs).arguments
        try:
            binding = _bound(values)
        except ValueError:
            return tool_error("AW Hermes compressed session has no verified lineage")
        if binding is None or not binding.before:
            return original_handle(function_name, function_args, *args, **kwargs)
        if not _json_native(function_args):
            return tool_error("AW Hermes tool arguments contain unsupported JSON values")
        try:
            key = _identity(values, binding)
        except ValueError:
            return tool_error("AW Hermes tool call identity is missing")
        bridged = _bridge_call.get() == key and function_name != "tool_call"
        if bridged:
            # The Tool Search bridge recursively dispatches the real tool under
            # the same call ID. Give that tool its own pre check and final guard.
            with _lock:
                _checked.pop(key, None)
            kwargs = {**kwargs, "skip_pre_tool_call_hook": False,
                      "skip_tool_execution_middleware": False}
            values["skip_pre_tool_call_hook"] = False
            values["skip_tool_execution_middleware"] = False
        with _lock:
            prior = _checked.get(key)
        if values.get("skip_pre_tool_call_hook") and prior is None:
            return tool_error("AW Hermes pre-tool check was bypassed")
        if values.get("skip_tool_execution_middleware"):
            if (prior is None or not values.get("skip_tool_request_middleware")
                    or json.dumps(function_args, sort_keys=True, allow_nan=False) != prior[0]):
                return tool_error("AW Hermes checked tool input changed")
        try:
            selected = prior[1] if prior else None
            bridge_token = None
            expected = _expected_input.set(prior[0] if prior else None)
            bridge_token = _bridge_call.set(key) if function_name == "tool_call" else None
            with aw_skill.invocation(function_name, function_args, selected=selected):
                try:
                    return original_handle(function_name, function_args, *args, **kwargs)
                except _CheckedInputChanged as error:
                    return tool_error(str(error))
        finally:
            if bridge_token is not None:
                _bridge_call.reset(bridge_token)
            _expected_input.reset(expected)
            with _lock:
                _checked.pop(key, None)

    def execution(tool_name: str, arguments: dict, next_call: Any, **context: Any) -> Any:
        try:
            binding = _bound(context)
        except ValueError:
            return tool_error("AW Hermes compressed session has no verified lineage")
        if binding is None or not binding.before:
            return original_execution(tool_name, arguments, next_call, **context)
        if not _json_native(arguments):
            return tool_error("AW Hermes tool arguments contain unsupported JSON values")
        try:
            key = _identity(context, binding)
        except ValueError:
            return tool_error("AW Hermes tool call identity is missing")
        with _lock:
            checked = _checked.get(key)
        # Hermes' outer middleware runs before its pre hook. Registry tools
        # enter a second middleware layer after pre, where args are frozen.
        if checked is None:
            return original_execution(tool_name, arguments, next_call, **context)
        if json.dumps(arguments, sort_keys=True, allow_nan=False) != checked[0]:
            return tool_error("AW Hermes checked tool input changed")

        def dispatch(effective: dict) -> Any:
            if (not _json_native(effective)
                    or json.dumps(effective, sort_keys=True, allow_nan=False) != checked[0]):
                return tool_error("AW Hermes checked tool input changed")
            return next_call(effective)

        token = _expected_input.set(checked[0])
        try:
            return original_execution(tool_name, arguments, dispatch, **context)
        finally:
            _expected_input.reset(token)

    original_inline_coerce = inline_tool_executors.coerce_tool_args
    original_coerced = inline_tool_executors._coerced
    inline_tool_executors.coerce_tool_args = coerce
    original_inline = dict(inline_tool_executors.INLINE_TOOL_EXECUTORS)

    def guard_inline(_name: str, executor: Any) -> Any:
        def guarded(agent: Any, arguments: dict, context: Any) -> Any:
            try:
                ids = inline_tool_executors.tool_hook_ids(
                    agent, context.effective_task_id, context.tool_call_id)
                binding = _bound(ids)
            except (Exception, SystemExit) as error:
                print(f"AW Hermes inline dispatch failed: {error}", file=sys.stderr)
                return tool_error("AW Hermes tool check is unavailable")
            if binding is None or not binding.before:
                return executor(agent, arguments, context)
            try:
                key = _identity(ids, binding)
                with _lock:
                    checked = _checked.get(key)
                if (checked is None or not _json_native(arguments)
                        or json.dumps(arguments, sort_keys=True, allow_nan=False) != checked[0]):
                    return tool_error("AW Hermes checked tool input changed")
            except (Exception, SystemExit) as error:
                print(f"AW Hermes inline dispatch failed: {error}", file=sys.stderr)
                return tool_error("AW Hermes tool check is unavailable")
            token = _expected_input.set(checked[0])
            try:
                return executor(agent, arguments, context)
            except _CheckedInputChanged as error:
                return tool_error(str(error))
            finally:
                _expected_input.reset(token)
        return guarded

    for name, executor in original_inline.items():
        inline_tool_executors.INLINE_TOOL_EXECUTORS[name] = guard_inline(name, executor)
    inline_tool_executors._coerced = (
        lambda name, executor: guard_inline(name, original_coerced(name, executor)))

    if any(hook.get("skill_check") for hook, _ in binding.before):
        _skill_restore = aw_skill.install()
    model_tools.handle_function_call = handle
    model_tools.coerce_tool_args = coerce
    plugins._dispatch_pre_tool_call_hooks = directives
    middleware.run_tool_execution_middleware = execution
    _restores.extend([(model_tools, "handle_function_call", original_handle, handle),
                      (model_tools, "coerce_tool_args", original_coerce, coerce),
                      (inline_tool_executors, "coerce_tool_args", original_inline_coerce, coerce),
                      (inline_tool_executors, "_coerced", original_coerced,
                       inline_tool_executors._coerced),
                      (plugins, "_dispatch_pre_tool_call_hooks", original_directives, directives),
                      (middleware, "run_tool_execution_middleware", original_execution, execution)])
    _restores.append((inline_tool_executors, "INLINE_TOOL_EXECUTORS", original_inline, None))


def _unload() -> None:
    global _skill_restore, _manager_restore, _turn_restore, _alive
    with _lock:
        _alive = False
        _bindings.clear()
        _roots.clear()
        _leases.clear()
        _known.clear()
        _retired.clear()
        _checked.clear()
        if _turn_restore is not None:
            authority, original, installed = _turn_restore
            if authority._execute_claimed is installed:
                authority._execute_claimed = original
            _turn_restore = None
        for module, name, original, installed in reversed(_restores):
            if name == "INLINE_TOOL_EXECUTORS":
                module.INLINE_TOOL_EXECUTORS.clear()
                module.INLINE_TOOL_EXECUTORS.update(original)
                continue
            if getattr(module, name) is installed:
                setattr(module, name, original)
        _restores.clear()
        if _skill_restore is not None:
            _skill_restore()
            _skill_restore = None
        if _manager_restore is not None:
            manager, original, installed = _manager_restore
            if manager.unload is installed:
                manager.unload = original
            _manager_restore = None
