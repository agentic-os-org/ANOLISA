"""Read Codex's public inventory without creating a thread or changing hook trust."""

import json
import os
import selectors
import signal
import subprocess
import sys
import time


def probe():
    expected = json.loads(sys.argv[2])
    child = subprocess.Popen(
        [sys.argv[1], "app-server"], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
    )
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ, "stdout")
    selector.register(child.stderr, selectors.EVENT_READ, "stderr")
    deadline = time.monotonic() + 8
    buffered = b""
    counts = {"stdout": 0, "stderr": 0}
    replies = {}

    def send(frame):
        child.stdin.write(json.dumps(frame).encode() + b"\n")
        child.stdin.flush()

    def interrupted(_signal, _frame):
        raise InterruptedError("Codex inventory probe cancelled")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        send({"id": 0, "method": "initialize", "params": {
            "clientInfo": {"name": "aw_hook_inventory", "version": "0.1.0"},
            "capabilities": {"experimentalApi": True},
        }})
        while time.monotonic() < deadline:
            for key, _ in selector.select(0.05):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    if key.data == "stdout":
                        raise RuntimeError("Codex inventory transport closed")
                    continue
                counts[key.data] += len(chunk)
                if counts[key.data] > 1024 * 1024:
                    raise ValueError("Codex inventory output exceeded limit")
                if key.data == "stderr":
                    continue
                buffered += chunk
                while b"\n" in buffered:
                    line, buffered = buffered.split(b"\n", 1)
                    response = json.loads(line)
                    request = response.get("id")
                    if request not in (0, 1, 2):
                        continue
                    if "error" in response:
                        raise RuntimeError("Codex rejected inventory request")
                    replies[request] = response["result"]
                    if request == 0:
                        send({"method": "initialized"})
                        send({"id": 1, "method": "hooks/list", "params": {"cwds": [os.getcwd()]}})
                        send({"id": 2, "method": "config/read", "params": {"includeLayers": False, "cwd": os.getcwd()}})
                if 1 in replies and 2 in replies:
                    rows = replies[1]["data"]
                    if len(rows) != 1 or rows[0]["cwd"] != os.getcwd():
                        raise ValueError("Codex inventory cwd mismatch")
                    features = replies[2]["config"].get("features") or {}
                    events = {"tool.before": "preToolUse", "tool.after": "postToolUse"}
                    hooks = {event: [h for h in rows[0]["hooks"]
                                    if h.get("command") == command and h["eventName"] == events[event]]
                             for event, command in expected.items()}
                    return {"hooks": hooks, "errors": bool(rows[0]["errors"]),
                            "enabled": features.get("hooks", features.get("codex_hooks", True)) is True}
        raise TimeoutError("Codex inventory deadline exceeded")
    finally:
        selector.close()
        child.stdin.close()
        try:
            child.wait(timeout=0.15)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        child.stdout.close()
        child.stderr.close()


if __name__ == "__main__":
    try:
        print(json.dumps(probe()))
    except Exception as error:
        # Native config and diagnostics may contain credentials; report only the boundary.
        print(f"Codex hook inventory failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)
