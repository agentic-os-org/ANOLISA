"""Bridge Cosh's native approval and execution handoff to the Rust PEP."""

import json
import os
import shlex
import shutil
import subprocess
from typing import Any

PERMISSIONS_ENV = "AGENT_SEC_COSH_SANDBOX_PERMISSIONS"


def prepare_authorized(input_data: dict[str, Any], command: str) -> dict[str, Any]:
    """Fail closed once the operator has enabled policy authorization."""
    try:
        permissions = os.environ[PERMISSIONS_ENV]
        if not os.path.isabs(permissions):
            raise ValueError(f"{PERMISSIONS_ENV} must name an absolute permission file")
        executable = shutil.which("agent-sec-cli")
        if executable is None:
            raise ValueError("agent-sec-cli is required for sandbox authorization")
        payload = dict(input_data)
        payload["tool_input"] = {"command": command}
        result = subprocess.run(
            [executable, "sandbox", "hook", "prepare", "--permissions", permissions],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=55,
            check=False,
        )
        if result.returncode:
            raise ValueError(
                result.stderr.strip()[:2000] or "sandbox authorization failed"
            )
        output = json.loads(result.stdout)
        if not isinstance(output, dict) or output.get("decision") not in {
            "allow",
            "ask",
            "block",
        }:
            raise ValueError("invalid sandbox authorization response")
        if output["decision"] != "block":
            wrapped = (
                output.get("hookSpecificOutput", {})
                .get("tool_input", {})
                .get("command")
            )
            if not isinstance(wrapped, str) or not wrapped:
                raise ValueError("sandbox authorization omitted the execution handoff")
        return output
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        message = f"Sandbox authorization blocked execution: {error}"
        return {"decision": "block", "reason": message, "systemMessage": message}


def finish_authorized(input_data: dict[str, Any]) -> dict[str, Any]:
    """Finish once after the foreground tool returns, including on timeout."""
    if PERMISSIONS_ENV not in os.environ:
        return {}
    try:
        parts = shlex.split(input_data.get("tool_input", {}).get("command", ""))
    except ValueError:
        return {}
    if len(parts) != 7 or parts[3:6] != ["sandbox", "hook", "execute"]:
        return {}
    executable = shutil.which("agent-sec-cli")
    socket = (
        os.environ.get("AGENT_SEC_DAEMON_SOCKET") or "/run/agent-sec-core/daemon.sock"
    )
    if executable is None or parts[:3] != [
        os.path.realpath(executable),
        "--socket",
        socket,
    ]:
        return {}
    try:
        result = subprocess.run(
            [executable, "--socket", socket, "sandbox", "hook", "finish", parts[6]],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        if result.returncode:
            raise ValueError(
                result.stderr.strip()[:2000] or "sandbox handoff cleanup failed"
            )
        return {}
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        return {
            "systemMessage": f"Sandbox receipt failed; do not rerun the command: {error}"
        }
