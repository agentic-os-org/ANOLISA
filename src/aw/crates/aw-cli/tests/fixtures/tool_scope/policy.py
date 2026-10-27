"""Record actual policy dispatch so selector skips cannot hide process execution."""

import json
import sys
from pathlib import Path

root, label, behavior = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
request = json.load(sys.stdin)
if behavior == "native-error":
    with (root / "scope-invocations.jsonl").open("a") as output:
        output.write(json.dumps({"label": label, "event": request}) + "\n")
    raise SystemExit(7)

reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"],
         "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]},
        {"name": "record", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif request["method"] == "invoke":
    with (root / "scope-invocations.jsonl").open("a") as output:
        output.write(json.dumps({"label": label, "event": request["event"]}) + "\n")
    reply["input_digest"] = request["input_digest"]
    reply["effects"] = []
    if behavior == "deny":
        reply["effects"] = [{"type": "block", "reason_code": "fixture_denial"}]
    elif behavior == "replace":
        reply["effects"] = [{"type": "replace_result", "text": "compact"}]
print(json.dumps(reply))
