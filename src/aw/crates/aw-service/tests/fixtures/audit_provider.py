"""Deterministic PostTool policy with independently controlled Provider failures."""

import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "observe", "events": ["tool.before", "security.violation"], "effects": ["observe"]},
        {"name": "post_tool", "events": ["tool.after"], "effects": ["replace_result"]},
    ]
elif request["method"] == "invoke":
    with (root / "invocations.jsonl").open("a") as output:
        output.write(json.dumps(request["event"]) + "\n")
    if (root / "fail-provider").exists():
        reply.update(status="error", error_code="fixture_failure")
    else:
        projection = request["event"].get("tool", {}).get("result_projection", {})
        effects = []
        if projection.get("status") == "success":
            effects = [{"type": "replace_result", "text": "compressed fixture result"}]
        reply.update(input_digest=request["input_digest"], effects=effects)
json.dump(reply, sys.stdout)
