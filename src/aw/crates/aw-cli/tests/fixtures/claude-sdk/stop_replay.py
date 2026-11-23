"""Drive actual Claude bridge cache eviction against the native AW callback transport."""

import asyncio
import json
from pathlib import Path
import sys
from types import SimpleNamespace

from submission_contract import NativeBridge

REASON = "Wait for confirmation · 请先确认"


async def run(binary, binding, root, on_error, blocked):
    bridge = NativeBridge({
        "executable": binary,
        "binding": binding,
        "hooks": [{"event": "turn.stop", "step": "check", "on_error": on_error,
                   "replace_result": False, "budget_ms": 5000}],
    }, launch_bound=False)
    bridge.turn_id = "turn"

    async def handle(frame):
        return await bridge.stop_callback(frame["request"]["input"], None, {})

    query = SimpleNamespace(_handle_control_request=handle)
    bridge.bind_stop_deliveries(query)

    async def deliver(delivery, *, active=False):
        return await query._handle_control_request({
            "request_id": delivery,
            "request": {"subtype": "hook_callback", "input": {
                "hook_event_name": "Stop", "session_id": "session",
                "cwd": str(root), "stop_hook_active": active,
                "veto": blocked and delivery == "original",
            }},
        })

    expected = {"decision": "block", "reason": REASON} if blocked else {}
    assert await deliver("original") == expected
    for index in range(64):
        assert await deliver(f"other-{index}") == {}
    assert len(bridge._stop_replies) == 64 and "original" not in bridge._stop_replies
    assert await deliver("original") == expected
    assert (root / "calls").read_text().splitlines().count("original") == 1

    # A changed delivery payload cannot retrieve the first input's receipt.
    bridge._stop_replies.clear()
    changed = await deliver("original", active=True)
    if blocked:
        assert changed == {}, changed
    else:
        assert changed == {"decision": "block", "reason": "AW turn stop check failed"}, changed
    assert (root / "calls").read_text().splitlines().count("original") == 1

    for journal in (root / "state").rglob("*.jsonl"):
        assert REASON not in journal.read_text()
    print(json.dumps({"blocked": blocked, "cached_replay": expected, "original_calls": 1}))


if __name__ == "__main__":
    binary, binding, root, mode = sys.argv[1:]
    asyncio.run(run(binary, binding, Path(root), mode, mode == "report"))
