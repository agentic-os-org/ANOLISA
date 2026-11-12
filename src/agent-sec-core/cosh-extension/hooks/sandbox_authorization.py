"""Bridge Cosh's native approval and execution handoff to the Rust PEP."""

import importlib
import json
import os
import shlex
import shutil
import subprocess
from typing import Any

ALL_SHELL_ENV = "AGENT_SEC_COSH_SANDBOX_ALL_SHELL"
# Leave time for Python startup and error projection inside the 60 s / 15 s hooks.
PREPARE_TIMEOUT = 55
FINISH_TIMEOUT = 10


def all_shell_enabled() -> bool:
    """Reject ambiguous security configuration instead of silently disabling it."""
    value = os.environ.get(ALL_SHELL_ENV)
    if value is None:
        return False
    normalized = value.strip().lower()
    if normalized in ("1", "true"):
        return True
    if normalized in ("0", "false"):
        return False
    raise ValueError(f"{ALL_SHELL_ENV} must be 1/true or 0/false")


def authorization_error(error: object) -> dict[str, Any]:
    """Expose a failure without handing the original command back for execution."""
    message = f"Sandbox authorization blocked execution: {error}"
    return {"decision": "block", "reason": message, "systemMessage": message}


def prepare_authorized(input_data: dict[str, Any], command: str) -> dict[str, Any]:
    """Fail closed once the operator has enabled policy authorization."""
    try:
        if os.environ.get("COSH_HOOK_EMPTY_OUTPUT_POLICY") != "block":
            raise ValueError(
                "Cosh host does not enforce nonempty hook output; "
                "upgrade Cosh NG to a matching build"
            )
        paths = importlib.import_module("sandbox_paths")
        permissions = getattr(paths, "DEFAULT_PERMISSIONS_TEMPLATE", None)
        if not isinstance(permissions, str) or not os.path.isabs(permissions):
            raise ValueError("installed sandbox permission template path is invalid")
        # TODO(sec-core maintainers): Pin prepare/finish to a trusted installed CLI;
        # inherited PATH can select a workspace executable.
        executable = shutil.which("agent-sec-cli")
        if executable is None:
            raise ValueError("agent-sec-cli is required for sandbox authorization")
        payload = dict(input_data)
        payload["tool_input"] = {"command": command}
        result = subprocess.run(
            [
                executable,
                "sandbox",
                "hook",
                "prepare",
                "--permissions",
                permissions,
            ],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=PREPARE_TIMEOUT,
            check=False,
        )
        if result.returncode:
            raise ValueError(
                result.stderr.strip()[:2000] or "sandbox authorization failed"
            )
        output = json.loads(result.stdout)
        if not isinstance(output, dict) or output.get("decision") not in (
            "allow",
            "ask",
            "block",
        ):
            raise ValueError("invalid sandbox authorization response")
        for key in ("reason", "systemMessage"):
            if not isinstance(output.get(key), str) or not output[key].strip():
                raise ValueError(f"sandbox authorization omitted {key}")
        projected = {
            key: output[key] for key in ("decision", "reason", "systemMessage")
        }
        if output["decision"] != "block":
            specific = output.get("hookSpecificOutput")
            tool_input = (
                specific.get("tool_input") if isinstance(specific, dict) else None
            )
            wrapped = (
                tool_input.get("command") if isinstance(tool_input, dict) else None
            )
            if not isinstance(wrapped, str) or not wrapped:
                raise ValueError("sandbox authorization omitted the execution handoff")
            parts = shlex.split(wrapped)
            socket = (
                os.environ.get("AGENT_SEC_DAEMON_SOCKET")
                or "/run/agent-sec-core/daemon.sock"
            )
            expected = [
                os.path.realpath(executable),
                "--socket",
                socket,
                "sandbox",
                "hook",
                "execute",
            ]
            if len(parts) != 7 or parts[:6] != expected or not parts[6]:
                raise ValueError("invalid sandbox execution handoff")
            # Requote the parsed argv: shell expansions must never survive CLI output validation.
            projected["hookSpecificOutput"] = {
                "tool_input": {"command": shlex.join(parts)}
            }
        return projected
    except (
        ImportError,
        SyntaxError,
        OSError,
        ValueError,
        KeyError,
        subprocess.SubprocessError,
    ) as error:
        return authorization_error(error)


def finish_authorized(input_data: dict[str, Any]) -> dict[str, Any]:
    """Finish once after the foreground tool returns, including on timeout."""
    try:
        tool_input = input_data.get("tool_input", {})
        if not isinstance(tool_input, dict):
            raise TypeError("receipt requires a shell input object")
        command = tool_input.get("command", "")
        if not isinstance(command, str):
            raise TypeError("receipt requires a string shell command")
        parts = shlex.split(command)
    except (TypeError, ValueError) as error:
        return {
            "systemMessage": f"Sandbox receipt failed; do not rerun the command: {error}"
        }
    if len(parts) != 7 or parts[3:6] != ["sandbox", "hook", "execute"]:
        return {}
    executable = shutil.which("agent-sec-cli")
    socket = (
        os.environ.get("AGENT_SEC_DAEMON_SOCKET") or "/run/agent-sec-core/daemon.sock"
    )
    if parts[:3] != [
        os.path.realpath(executable) if executable else parts[0],
        "--socket",
        socket,
    ]:
        return {}
    try:
        if executable is None:
            raise ValueError("agent-sec-cli is required to finish the sandbox handoff")
        result = subprocess.run(
            [executable, "--socket", socket, "sandbox", "hook", "finish", parts[6]],
            capture_output=True,
            text=True,
            timeout=FINISH_TIMEOUT,
            check=False,
        )
        if result.returncode:
            raise ValueError(
                result.stderr.strip()[:2000] or "sandbox handoff cleanup failed"
            )
        if result.stdout.strip():
            raise ValueError("invalid sandbox finish response")
        return {}
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        return {
            "systemMessage": f"Sandbox receipt failed; do not rerun the command: {error}"
        }
