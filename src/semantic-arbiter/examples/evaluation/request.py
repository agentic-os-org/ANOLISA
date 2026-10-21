"""Emit a fresh synthetic evaluation request, never refresh production evidence."""

import argparse
import json
from pathlib import Path
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "case",
    choices=["preservation-allow", "task-scope-allow", "snapshot", "preserve", "missing-evidence"],
)
args = parser.parse_args()
source = Path(__file__).resolve().parent.parent / f"{args.case}.json"
request = json.loads(source.read_text(encoding="utf-8"))
now_ms = time.time_ns() // 1_000_000
request["api_version"] = "semantic-arbiter/v1alpha2"
request["mode"] = "model_evaluation"
del request["check_at_ms"]
request["budget"]["opened_at_ms"] = now_ms
request["budget"]["deadline_ms"] = now_ms + 30_000
for evidence in request["evidence"]:
    evidence["captured_at_ms"] = now_ms
    evidence["valid_until_ms"] = now_ms + 30_000
print(json.dumps(request, ensure_ascii=False))
