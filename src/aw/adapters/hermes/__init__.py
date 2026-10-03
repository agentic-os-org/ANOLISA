"""Register AW callbacks through the pinned Hermes native shell-hook bridge."""

import json
import os
from pathlib import Path
import shlex
import stat
import tempfile
from typing import Any


REVISION = "952c941e741e922a9be8fc403c8944c6e96318bb"
EVENTS = {"tool.before": "pre_tool_call", "tool.after": "post_tool_call"}


def _read_launch(path: str) -> dict[str, Any]:
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as handle:
        metadata = os.fstat(handle.fileno())
        if (not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or stat.S_IMODE(metadata.st_mode) != 0o600
                or metadata.st_size > 1024 * 1024):
            raise ValueError("unsafe AW Hermes launch file")
        data = handle.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024:
            raise ValueError("oversized AW Hermes launch file")
    launch = json.loads(data)
    if launch.get("schema") != "aw-hermes/v1alpha1":
        raise ValueError("unsupported AW Hermes launch schema")
    for key in ("binary", "binding", "profile", "cwd", "ready_path"):
        if not isinstance(launch.get(key), str) or not Path(launch[key]).is_absolute():
            raise ValueError("AW Hermes launch paths must be absolute")
    if not isinstance(launch.get("hooks"), list) or not launch["hooks"]:
        raise ValueError("AW Hermes launch has no hooks")
    if not isinstance(launch.get("token"), str) or not launch["token"]:
        raise ValueError("AW Hermes launch has no readiness token")
    return launch


def register(ctx: Any) -> None:
    """An installed plugin is inert unless this process was launched by AW."""
    path = os.environ.get("AW_HERMES_LAUNCH")
    if not path:
        return

    from agent.shell_hooks import ShellHookSpec, _make_callback
    from hermes_constants import get_hermes_home
    from hermes_cli.version_info import get_version_info

    launch = _read_launch(path)
    if get_version_info().commit != REVISION:
        raise ValueError("AW requires Hermes revision " + REVISION)
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
                or action not in ("report", "block")
                or isinstance(timeout, bool) or not isinstance(timeout, int)
                or not 1 <= timeout <= 300 or (event, step) in seen):
            raise ValueError("invalid AW Hermes callback registration")
        seen.add((event, step))
        command = shlex.join([
            launch["binary"], "hook", "--binding", launch["binding"],
            "--adapter", "hermes", "--event", event, "--step", step, "--on-error", action,
        ])
        spec = ShellHookSpec(
            event=EVENTS[event], command=command, timeout=timeout,
            fail_closed=action == "block",
        )
        callbacks.append((EVENTS[event], _make_callback(spec)))

    # Hermes owns callback order, tool concurrency, timeout handling and the
    # interpretation of stdout/exit 2. AW does not introduce another scheduler.
    for event, callback in callbacks:
        ctx.register_hook(event, callback)
    with tempfile.NamedTemporaryFile(mode="w", dir=Path(launch["ready_path"]).parent) as handle:
        json.dump({"version": 1, "adapter": "hermes", "token": launch["token"],
                   "pid": os.getpid(), "hooks": len(callbacks)}, handle)
        handle.flush()
        os.fsync(handle.fileno())
        os.link(handle.name, launch["ready_path"])
