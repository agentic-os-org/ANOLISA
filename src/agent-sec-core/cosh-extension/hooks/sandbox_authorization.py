"""Bridge Cosh's native approval and execution handoff to the Rust PEP."""

import importlib
import json
import os
import shlex
import shutil
import subprocess
import tempfile
import time
import unicodedata
from typing import Any

ALL_SHELL_ENV = "AGENT_SEC_COSH_SANDBOX_ALL_SHELL"
SOURCE_ENV = "AGENT_SEC_COSH_SANDBOX_PERMISSION_SOURCE"
RUNTIME_ENV = "AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS"
# Leave time for Python startup and error projection inside the 60 s / 15 s hooks.
PREPARE_TIMEOUT = 55
INTENT_TIMEOUT = 40
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


def _display(value: str) -> str:
    return "".join(
        f"\\u{ord(char):04x}" if unicodedata.category(char).startswith("C") else char
        for char in value
    )


def _notes(value: object) -> list[str]:
    if (
        not isinstance(value, list)
        or len(value) > 128
        or any(
            not isinstance(note, str)
            or not note.strip()
            or len(note.encode("utf-8")) > 4096
            or any(unicodedata.category(char) == "Cc" for char in note)
            for note in value
        )
    ):
        raise ValueError("invalid command intent explanation")
    return value


def _propose(
    executable: str, command: str, cwd: str, deadline: float
) -> dict[str, Any]:
    budget = min(INTENT_TIMEOUT, deadline - time.monotonic())
    if budget <= 2:
        raise ValueError("command intent time budget exhausted")
    result = subprocess.run(
        [
            executable,
            "--timeout-ms",
            str(int((budget - 2) * 1000)),
            "intent",
            "propose-command",
            "--command",
            command,
            "--cwd",
            cwd,
            "--format",
            "json",
        ],
        capture_output=True,
        text=True,
        timeout=budget,
        check=False,
    )
    if result.returncode:
        raise ValueError(
            result.stderr.strip()[:2000] or "command intent generation failed"
        )
    if len(result.stdout.encode("utf-8")) > 512 * 1024:
        raise ValueError("command intent response exceeds its size limit")
    proposal = json.loads(result.stdout)
    if not isinstance(proposal, dict):
        raise ValueError("invalid command intent response")
    if proposal.get("outcome") == "needs_clarification":
        reasons = _notes(proposal.get("reasons"))
        if not reasons or set(proposal) != {"outcome", "reasons"}:
            raise ValueError("invalid command intent clarification")
        raise ValueError(
            "command needs clarification: " + "; ".join(map(_display, reasons))
        )
    if proposal.get("outcome") != "candidate" or set(proposal) != {
        "outcome",
        "policy",
        "caveats",
    }:
        raise ValueError("invalid command intent outcome")
    if (
        not isinstance(proposal["policy"], str)
        or len(proposal["policy"].encode("utf-8")) > 256 * 1024
    ):
        raise ValueError("invalid command intent Cedar candidate")
    _notes(proposal["caveats"])
    return proposal


def authorization_error(error: object) -> dict[str, Any]:
    """Expose a failure without handing the original command back for execution."""
    message = f"Sandbox authorization blocked execution: {error}"
    return {"decision": "block", "reason": message, "systemMessage": message}


def prepare_authorized(input_data: dict[str, Any], command: str) -> dict[str, Any]:
    """Fail closed once the operator has enabled policy authorization."""
    deadline = time.monotonic() + PREPARE_TIMEOUT
    temporary = None
    caveats = []
    try:
        if os.environ.get("COSH_HOOK_EMPTY_OUTPUT_POLICY") != "block":
            raise ValueError(
                "Cosh host does not enforce nonempty hook output; "
                "upgrade Cosh NG to a matching build"
            )
        source = os.environ.get(SOURCE_ENV, "fixed")
        if source == "fixed":
            paths = importlib.import_module("sandbox_paths")
            permissions = getattr(paths, "DEFAULT_PERMISSIONS_TEMPLATE", None)
            if not isinstance(permissions, str) or not os.path.isabs(permissions):
                raise ValueError(
                    "installed sandbox permission template path is invalid"
                )
        # TODO(sec-core maintainers): Pin prepare/finish to a trusted installed CLI;
        # inherited PATH can select a workspace executable.
        executable = shutil.which("agent-sec-cli")
        if executable is None:
            raise ValueError("agent-sec-cli is required for sandbox authorization")
        payload = dict(input_data)
        payload["tool_input"] = {"command": command}
        extra = []
        if source == "command":
            preset = os.environ.get(RUNTIME_ENV, "")
            if not os.path.isabs(preset):
                raise ValueError(
                    f"{RUNTIME_ENV} must name an absolute Cedar runtime preset"
                )
            cwd = input_data.get("cwd")
            if (
                not isinstance(cwd, str)
                or not os.path.isabs(cwd)
                or not os.path.isdir(cwd)
            ):
                raise ValueError("command intent requires an existing absolute cwd")
            cwd = os.path.realpath(cwd)
            # The intent service and authorization see exactly the same unwrapped invocation.
            payload = {
                key: input_data[key]
                for key in ("session_id", "run_id")
                if key in input_data
            }
            payload.update(cwd=cwd, tool_input={"command": command})
            proposal = _propose(executable, command, cwd, deadline)
            caveats = proposal["caveats"]
            temporary = tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", suffix=".cedar"
            )
            temporary.write(proposal["policy"])
            temporary.flush()
            permissions = temporary.name
            extra = ["--runtime-permissions", preset]
        elif source != "fixed":
            raise ValueError(f"{SOURCE_ENV} must be fixed or command")
        remaining = deadline - time.monotonic()
        if remaining <= 1:
            raise ValueError("sandbox authorization time budget exhausted")
        # FinishRun, evaluate and accept/report share the remaining subprocess budget.
        rpc_ms = max(1, int(min(5, (remaining - 1) / 3) * 1000))
        result = subprocess.run(
            [
                executable,
                "sandbox",
                "hook",
                "prepare",
                "--permissions",
                permissions,
                *extra,
                "--timeout-ms",
                str(rpc_ms),
            ],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=remaining,
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
        if source == "command":
            notice = "Command permission candidate (not approval)."
            if not proposal["policy"].strip():
                notice += " Empty candidate: no business permissions requested."
            for caveat in caveats:
                notice += "\nCandidate caveat: " + _display(caveat)
            for key in ("reason", "systemMessage"):
                projected[key] += "\n" + notice
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
        message = _display(str(error))
        for caveat in caveats:
            message += "\nCandidate caveat: " + _display(caveat)
        return authorization_error(message)
    finally:
        if temporary is not None:
            temporary.close()


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
            [
                executable,
                "--socket",
                socket,
                "sandbox",
                "hook",
                "finish",
                parts[6],
                "--timeout-ms",
                "8000",
            ],
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
