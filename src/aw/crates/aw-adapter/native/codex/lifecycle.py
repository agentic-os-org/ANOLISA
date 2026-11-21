"""Dispatch read-only lifecycle routes from a trusted Codex native command."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main() -> None:
    event = sys.argv[1]
    if event not in {"session.start", "turn.stop"}:
        raise ValueError("unsupported Codex lifecycle event")
    config = json.loads(Path(os.environ["AW_CODEX_CONFIG"]).read_bytes())
    route = config["chains"].get(event)
    if route is None:
        return
    data = sys.stdin.buffer.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("Codex lifecycle payload exceeds the native input limit")
    native = json.loads(data)
    if event == "turn.stop" and native.get("stop_hook_active") is True:
        return
    deadline = time.monotonic() + (route["budget_ms"] + 2000) / 1000
    for step in route["steps"]:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Codex lifecycle callback budget exhausted")
        subprocess.run(
            [
                os.environ["AW_CODEX_BINARY"], "hook", "--adapter", "codex", "--binding",
                config["binding"], "--event", event, "--step", step["id"], "--on-error", "report",
            ],
            input=data, stdout=subprocess.DEVNULL, timeout=remaining, check=True,
        )


if __name__ == "__main__":
    main()
