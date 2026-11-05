"""Cancel stock native entrypoints while a real CLI waits on the paused daemon."""

from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path

from runtime import await_condition, save


def scanners(root):
    socket = str(root / "sec-runtime/daemon.sock").encode()
    found = []
    for path in Path("/proc").glob("[0-9]*/cmdline"):
        try:
            argv = path.read_bytes().split(b"\0")
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if socket in argv and b"scan-prompt" in argv:
            found.append(int(path.parent.name))
    return found


def cancel_sdk(root, output):
    entered = []
    def scanning():
        entered[:] = scanners(root)
        return bool(entered)
    await_condition(scanning, 30, "real submission CLI entered")
    (root / "work/cancel.request").touch()
    save(output / "cancel.json", {"requested": True, "scanner_pids": entered})


def cancel_app(root, output, request, port, body):
    with ThreadPoolExecutor(max_workers=1) as pool:
        pending = pool.submit(request, port, "console/chat", body)
        entered = []
        def scanning():
            assert not pending.done(), "native turn ended before cancellation"
            entered[:] = scanners(root)
            return bool(entered)
        await_condition(scanning, 30, "real submission CLI entered")
        result = json.loads(request(port, "console/chat/stop?chat_id=" + body["session_id"], {}))
        assert result["stopped"] is True
        save(output / "cancel.json", {"requested": True, "scanner_pids": entered, "native": result})
        return pending.result(timeout=30)


def verify_cancelled(root):
    records = [json.loads(line)["record"]
               for path in (root / "state/journal").glob("*.jsonl")
               for line in path.read_text().splitlines()]
    assert any(record.get("phase") == "closed" and record.get("status") == "cancelled"
               for record in records), "submission did not close as cancelled"
