"""Deterministic result scanner and observation log for public Qoder Hooks."""

import json
from pathlib import Path
import sys

request = json.load(sys.stdin)
reply = {
    "api_version": "aw-provider/v1alpha1",
    "request_id": request["request_id"],
    "status": "ok",
}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": "record", "events": ["tool.after"], "effects": ["observe"]},
        {
            "name": "scan",
            "events": ["tool.after"],
            "effects": ["observe", "replace_result"],
        },
    ]
elif request["method"] == "invoke":
    tool = request["event"]["tool"]
    projection = tool.get("result_projection")
    with Path(request["config"]["record_file"]).open("a") as log:
        log.write(json.dumps({"operation": request["operation"], "tool": tool}) + "\n")
    effects = []
    failure = None
    if request["operation"] == "scan":
        if Path(request["config"]["fail_file"]).is_file():
            failure = "fixture_provider_error"
        elif projection and "SYNTHETIC_RAW_SECRET" in projection["text"]:
            effects = [{"type": "replace_result", "text": "REVIEWED_SAFE_BODY"}]
        elif (
            isinstance(tool["result"], dict)
            and tool["result"].get("is_error")
            and "SYNTHETIC_RAW_SECRET" in tool["result"]["error"]
        ):
            failure = "unsupported_pii_result"
    if failure:
        reply.update(status="error", error_code=failure)
    else:
        reply.update(input_digest=request["input_digest"], effects=effects)
print(json.dumps(reply))
