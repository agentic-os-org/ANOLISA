"""Register AW callbacks through the Hermes native shell-hook bridge."""

import json
import os
from pathlib import Path
import shlex
import stat
import subprocess
import sys
import tempfile
from typing import Any


LIFECYCLE_EVENTS = {
    "session.start": "on_session_start",
    "turn.stop": "on_session_end",
    "session.end": "on_session_finalize",
}
EVENTS = {**LIFECYCLE_EVENTS, "tool.before": "pre_tool_call", "tool.after": "post_tool_call"}


def callback_event(hook: dict[str, Any], final_result: bool) -> str:
    """Select the native registry entry for one managed policy step."""
    if hook.get("replace_result"):
        if hook["event"] != "tool.after":
            raise ValueError("AW replacements require a completed tool")
        if not final_result:
            return "transform_tool_result"
    return EVENTS[hook["event"]]


def _read_private_json(path: str) -> Any:
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as handle:
        metadata = os.fstat(handle.fileno())
        if (not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or stat.S_IMODE(metadata.st_mode) != 0o600
                or metadata.st_size > 1024 * 1024):
            raise ValueError("unsafe AW Hermes private file: " + path)
        data = handle.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024:
            raise ValueError("oversized AW Hermes private file: " + path)
    return json.loads(data)


def _read_launch(path: str) -> dict[str, Any]:
    launch = _read_private_json(path)
    if launch.get("schema") != "aw-hermes/v1alpha1":
        raise ValueError("unsupported AW Hermes launch schema")
    for key in ("binary", "binding", "profile", "cwd", "ready_path"):
        if not isinstance(launch.get(key), str) or not Path(launch[key]).is_absolute():
            raise ValueError("AW Hermes launch paths must be absolute")
    if not isinstance(launch.get("hooks"), list):
        raise ValueError("AW Hermes launch hooks must be a list")
    if not isinstance(launch.get("token"), str) or not launch["token"]:
        raise ValueError("AW Hermes launch has no readiness token")
    revision = launch.get("revision")
    if (not isinstance(revision, str) or len(revision) not in (40, 64)
            or any(char not in "0123456789abcdefABCDEF" for char in revision)):
        raise ValueError("AW Hermes launch revision must be a full Git object ID")
    return launch


def _gateway_run(argv: list[str]) -> bool:
    from hermes_cli._parser import build_top_level_parser, command_argv

    if command_argv(argv)[:1] != ["gateway"]:
        return False
    from hermes_cli.subcommands.gateway import build_gateway_parser

    parser, subcommands, _ = build_top_level_parser()
    build_gateway_parser(subcommands, cmd_gateway=None, cmd_proxy=None, cmd_gateway_enroll=None)
    args = parser.parse_args(argv)
    return args.command == "gateway" and args.gateway_command in (None, "run")


def register(ctx: Any) -> None:
    """Register managed hooks or the opt-in native Gateway connection."""
    session = sys.modules.get("_aw_hermes_session")
    if session is None:
        if hasattr(ctx, "register_rpc"):
            from . import aw_gateway
            config = os.environ.get("AW_HERMES_GATEWAY_CONFIG")
            if config is not None and not _gateway_run(sys.argv[1:]):
                config = None
            global start_sdk
            start_sdk = aw_gateway.register(ctx, _read_launch, gateway_config=config)
        return

    from agent.shell_hooks import ShellHookSpec, _make_callback
    from .aw_identity import runtime_revision
    from hermes_constants import get_hermes_home

    launch = _read_launch(session.launch_path)
    if runtime_revision() != launch["revision"]:
        raise ValueError("AW Hermes checkout changed after launch preparation")
    if get_hermes_home().resolve() != Path(launch["profile"]):
        raise ValueError("AW Hermes launch profile changed")
    if Path.cwd().resolve() != Path(launch["cwd"]):
        raise ValueError("AW Hermes launch working directory changed")

    callbacks = []
    seen = set()
    for hook in launch["hooks"]:
        event = hook.get("event")
        step = hook.get("step")
        action = hook.get("on_error")
        timeout = hook.get("timeout")
        if (event not in EVENTS or not isinstance(step, str) or not step
                or (action not in ("report", "block") and not (event == "tool.after" and action == "withhold_result" and launch.get("final_result")))
                or isinstance(timeout, bool) or not isinstance(timeout, int)
                or not 1 <= timeout <= 300 or (event, step) in seen):
            raise ValueError("invalid AW Hermes callback registration")
        seen.add((event, step))
        native_event = callback_event(hook, launch.get("final_result", False))
        if event == "tool.after" and launch.get("final_result"):
            # Receipt and discovery still attest the registered Hook. Evaluation occurs
            # after other plugins' transforms at the verified model-history boundary.
            from .aw_result import remember_post
            callbacks.append((native_event, remember_post))
            continue
        command_args = [
            launch["binary"], "hook", "--binding", launch["binding"],
            "--adapter", "hermes", "--event", event, "--step", step, "--on-error", action,
        ]
        if event in LIFECYCLE_EVENTS:
            callbacks.append((native_event, _lifecycle_callback(event, command_args, timeout)))
            continue
        command = shlex.join(command_args)
        if native_event == "transform_tool_result":
            callbacks.append((native_event, _replacement_callback(launch, hook)))
            continue
        spec = ShellHookSpec(
            event=native_event, command=command, timeout=timeout,
            fail_closed=action == "block",
        )
        callback = _input_callback(spec, session) if event == "tool.before" else _make_callback(spec)
        if getattr(session, "skills", None) is not None and event == "tool.before":
            callback = (session.skills.dynamic_callback(
                spec, lambda response, result: _record_input(response, result, session)
            ) if launch.get("reloadable")
                        else session.skills.callback(callback, hook))
        callbacks.append((native_event, callback))

    # Hermes owns callback order, tool concurrency, timeout handling and the
    # interpretation of stdout/exit 2. AW does not introduce another scheduler.
    for event, callback in callbacks:
        ctx.register_hook(event, callback)
    if launch.get("final_result"):
        from . import aw_result

        def final(ids, name, args, result, outcome):
            payload = aw_result.payload(launch["cwd"], ids, name, args, result, outcome)
            return aw_result.evaluate(launch, launch["binding"], payload)

        restores = aw_result.install(final)

        def restore():
            for module, name, original, installed in reversed(restores):
                if getattr(module, name) is installed:
                    setattr(module, name, original)

        ctx.on_unload(restore)
    receipt = {"version": 1, "adapter": "hermes", "token": launch["token"],
               "pid": os.getpid(), "hooks": len(callbacks)}
    with tempfile.NamedTemporaryFile(mode="w", dir=Path(launch["ready_path"]).parent) as handle:
        json.dump(receipt, handle)
        handle.flush()
        os.fsync(handle.fileno())
        try:
            os.link(handle.name, launch["ready_path"])
        except FileExistsError:
            # Native rediscovery replaces callbacks but retains this session's receipt.
            if _read_private_json(launch["ready_path"]) != receipt:
                raise ValueError("AW Hermes readiness receipt belongs to another registration") from None


def _lifecycle_callback(event: str, command: list[str], timeout: int) -> Any:
    def callback(session_id: str = "", **native: Any) -> None:
        if not session_id:
            return
        payload = {"hook_event_name": LIFECYCLE_EVENTS[event], "session_id": session_id,
                   "cwd": os.getcwd()}
        if event == "turn.stop":
            turn = native.get("turn_id")
            reason = native.get("turn_exit_reason")
            # Legacy CLI shutdown also emits on_session_end, without a turn.
            if not isinstance(turn, str) or not turn or not isinstance(reason, str):
                return
            payload["turn_id"] = turn
            # Diagnostic suffixes may include application error text.
            payload["reason"] = reason.partition("(")[0]
        elif event == "session.end":
            payload["reason"] = native.get("reason") or "unknown"
        try:
            subprocess.run(command, input=json.dumps(payload).encode(), stdout=subprocess.DEVNULL,
                           timeout=timeout, check=True)
        except (OSError, subprocess.SubprocessError) as error:
            sys.stderr.write(f"AW Hermes {event} observation failed: {type(error).__name__}\n")

    return callback


def _input_callback(spec: Any, session: Any) -> Any:
    def callback(**kwargs: Any) -> Any:
        from agent.shell_hooks import _evaluate_result, _serialize_payload, _spawn
        response = _spawn(spec, _serialize_payload(spec.event, kwargs))
        result = _evaluate_result(spec, response)
        _record_input(response, result, session)
        return result
    return callback


def _record_input(response: Any, result: Any, session: Any) -> None:
    if isinstance(result, dict) and result.get("action") == "modify":
        raw = json.loads(response["stdout"])
        if response["returncode"] == 0 and raw.get("aw_checked_input") is True:
            cell = session.rewritten_input.get()
            if cell is not None:
                cell["input"] = json.dumps(result["args"], sort_keys=True)


def _replacement_callback(launch: dict[str, Any], hook: dict[str, Any]) -> Any:
    """Use Hermes' final model-bound string seam, preserving its native deadline."""
    argv = [launch["binary"], "hook", "--binding", launch["binding"],
            "--adapter", "hermes", "--event", "tool.after", "--step", hook["step"],
            "--on-error", hook["on_error"]]

    def callback(tool_name: str = "", args: Any = None, result: str = "",
                 session_id: str = "", **kwargs: Any) -> str | None:
        payload = {"hook_event_name": "transform_tool_result", "cwd": str(Path.cwd()),
                   "session_id": session_id, "tool_name": tool_name, "tool_input": args,
                   "extra": {**kwargs, "result": result}}
        try:
            completed = subprocess.run(argv, input=json.dumps(payload).encode(),
                capture_output=True, timeout=hook["timeout"], check=False)
            if completed.returncode != 0:
                print("AW Hermes result replacement failed; original result retained", file=sys.stderr)
                return None
            reply = json.loads(completed.stdout) if completed.stdout.strip() else {}
            replacement = reply.get("aw_replacement")
            return replacement if isinstance(replacement, str) else None
        except (OSError, subprocess.TimeoutExpired, ValueError, TypeError):
            print("AW Hermes result replacement failed; original result retained", file=sys.stderr)
            return None

    return callback
