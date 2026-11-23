#!/usr/bin/python3
"""Public CLI fixture: record literal argv, then emit a configured result."""

import json
import os
from pathlib import Path
import re
import sys
import time

root = Path(__file__).resolve().parent
stat = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()
(root / "cli.pid").write_text(f"{os.getpid()} {stat[19]}")
(root / "argv.json").write_text(json.dumps(sys.argv[1:]))
stdin = sys.stdin.buffer.read()
(root / "stdin.bin").write_bytes(stdin)
case = json.loads((root / "case.json").read_text())
if case.get("record_health_environment"):
    (root / "environment.json").write_text(json.dumps({
        key: os.environ.get(key) for key in ["AGENT_SEC_DAEMON_SOCKET",
        "AGENT_SEC_HEALTH_TIMEOUT_SECONDS", "AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK"]
    }))
if "scan-pii" in sys.argv:
    text = stdin.decode()
    (root / "stdin.txt").write_text(text)
    with (root / "scans.jsonl").open("a") as scans:
        scans.write(json.dumps({"text": text, "argv": sys.argv[1:]}) + "\n")
    source = sys.argv[sys.argv.index("--source") + 1]
    if case.get("pii"):
        risk = re.search(case.get("pii_pattern", "SENSITIVE"), text) is not None
        case["stdout"] = {"ok": True, "verdict": "deny" if risk else "pass",
                          "summary": {"source": source, "truncated": False,
                                      "bytes_scanned": len(text.encode())}}
        summary = case["stdout"]["summary"]
        if "v1_ruleset_sha256" in case:
            ruleset_hash = case["v1_ruleset_sha256"]
            if "--redact-output" in sys.argv:
                ruleset_hash = case.get("v1_redaction_sha256", ruleset_hash)
            summary["custom_rules"] = {
                "status": "loaded", "runtime_error_count": 0, "budget_exhausted": False,
                "truncated": False, "ruleset_sha256": ruleset_hash,
            }
        else:
            summary["ruleset_id"] = case.get("ruleset_id", "fixture-v2")
        if "--redact-output" in sys.argv:
            case["stdout"]["redacted_text"] = re.sub(
                case.get("pii_pattern", "SENSITIVE"), "[MASKED]", text
            )
            case.update(case.get("redactions_by_text", {}).get(text, case.get("redaction", {})))
time.sleep(case.get("sleep", 0))
sys.stderr.write(case.get("stderr", "") + "e" * case.get("stderr_repeat", 0))
sys.stderr.flush()
sys.stdout.write(case.get("raw", json.dumps(case.get("stdout"))))
sys.stdout.write("o" * case.get("stdout_repeat", 0))
sys.stdout.flush()
sys.exit(case.get("exit", 0))
