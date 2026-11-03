"""Drive the stock QwenPaw HTTP entrypoint and capture the next model request."""

import json
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
import time

from runtime import await_condition, save
from native_app_config import ready, request


def turn(root, output, port, name):
    save(root / "tool-call.json", {"name":"Skill", "arguments":{"skill":name}})
    (root / "tool-call-id.txt").write_text(name)
    (root / "model-requests.jsonl").unlink(missing_ok=True)
    payload = {"session_id":name, "user_id":"aw-skill-test", "input":[
        {"role":"user", "content":[{"type":"text", "text":"Load the requested Skill once."}]}]}
    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(request, port, "console/chat", payload)
        await_condition(future.done, 60, "QwenPaw Skill turn")
        events = future.result()
    (output / (name + ".sse")).write_bytes(events)
    frames = [json.loads(line[6:]) for line in events.splitlines() if line.startswith(b"data: {")]
    completed = [frame for frame in frames if frame.get("object") == "response" and frame.get("status") == "completed"]
    assert len(completed) == 1, "native turn did not complete"
    results = [item for item in completed[0]["output"] if item["type"] == "plugin_call_output"]
    assert len(results) == 1, "native Skill call missing"
    model = [json.loads(line) for line in (root / "model-requests.jsonl").read_text().splitlines()]
    save(output / (name + "-model.json"), model)
    return results[0]["content"][0]["data"], model
