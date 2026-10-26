#!/usr/bin/env python3
"""Synthetic optional view endpoint; never a tool event Provider."""

import json
import os
from pathlib import Path
import sys
import time

root, mode = Path(sys.argv[1]), sys.argv[2]
request = json.load(sys.stdin)
assert request["protocol"] == "aw-provider-view/v1alpha1"
with (root / "views.jsonl").open("a") as log:
    log.write(json.dumps(request) + "\n")
stat = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()
(root / f"view-{os.getpid()}.pid").write_text(f"{os.getpid()} {stat[19]}")
reply = {key: request[key] for key in (
    "protocol", "operation", "request_id", "provider_id", "config_revision"
)}
reply.update(status="ok", observed_at_ms=1000, code=None, data=None)
if mode == "timeout":
    time.sleep(10)
if mode == "overflow":
    print("x" * (1024 * 1024 + 1))
    sys.exit(0)
if mode == "failure":
    print("private stderr should not escape", file=sys.stderr)
    sys.exit(1)
if mode == "identity":
    reply["provider_id"] = "wrong"
if request["operation"] == "describe":
    reply["data"] = {"implementation": "tokenless", "version": None,
                     "views": [] if mode == "unsupported" else ["metrics.summary"]}
else:
    assert request["view"] == "metrics.summary"
    assert request["parameters"] == {"limit": 200}
    total = dict(records=1, before_tokens_estimated=100,
                 after_tokens_estimated=50, saved_tokens_estimated=50,
                 saved_percent=50.0)
    reply["data"] = dict(total, scope="component_store_recent_records",
                         requested_limit=200, range_start=None, range_end=None,
                         recording_state="unknown", adoption="unverified",
                         by_operation={"compress-response": total})
if mode == "private":
    reply["data"]["private_body"] = "must not escape"
print(json.dumps(reply))
