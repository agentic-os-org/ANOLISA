"""Bounded local Hook fixture; a marker proves the tool awaited policy completion."""

import json
import os
import signal
from pathlib import Path
import sys
import time


def main() -> None:
    event = json.load(sys.stdin)
    mode = os.environ.get("CLAUDE_TEST_MODE", "allow")
    if mode.startswith("native-hook-") and "hook_event_name" in event:
        assert event["hook_event_name"] == "PreToolUse"
        (Path.cwd() / "native-input.json").write_text(json.dumps(event))
        if mode == "native-hook-deny-exit2":
            print("native command veto", file=sys.stderr)
            raise SystemExit(2)
        print("native-local-diagnostic", file=sys.stderr)
        specific = {"hookEventName": "PreToolUse"}
        if mode == "native-hook-invalid-chain":
            counter = Path.cwd() / "native-count"
            count = int(counter.read_text()) + 1 if counter.exists() else 1
            counter.write_text(str(count))
            if count > 34:
                print("{}")
                return
            if count % 2:
                print("{")
                return
            specific["updatedInput"] = {"value": "rewritten"}
        if mode.startswith("native-hook-deny"):
            specific["permissionDecision"] = "deny"
            if mode != "native-hook-deny-no-reason":
                specific["permissionDecisionReason"] = "native command veto"
        if mode.endswith("rewrite"):
            specific["updatedInput"] = {"value": "rewritten"}
        print(json.dumps({} if mode == "native-hook-neutral" else {"hookSpecificOutput": specific}))
        return
    if mode == "hook-failure":
        raise SystemExit(9)
    with (Path.cwd() / "policy-events.jsonl").open("a") as log:
        log.write(json.dumps(event) + "\n")
    time.sleep(0.1)
    if mode == "many-denials" and event["name"] == "tool.before":
        counter = Path.cwd() / "before-count"
        count = int(counter.read_text()) + 1 if counter.exists() else 1
        counter.write_text(str(count))
        if count <= 34:
            if count % 3 == 1:
                raise SystemExit(9)
            if count % 3 == 0:
                os.kill(os.getpid(), signal.SIGTERM)
            print("true")
            return
    if mode == "bash-pretool-deny" and "TOKENLESS_AGENT_ID=" in event["tool"]["input"].get(
        "command", ""
    ):
        print("true")
        return
    value = event["tool"]["input"].get("value")
    if event["name"] == "tool.before" and value:
        (Path.cwd() / f"policy-{value}").touch()
    if (mode == "before-report-failure" and event["name"] == "tool.before") or (
        mode in {"after-report-failure", "bash-compress-observer-failure"}
        and event["name"] == "tool.after"
    ):
        raise SystemExit(9)
    print(json.dumps(event["name"] == "tool.after" or mode in {"deny", "native-allow-aw-deny"}))


if __name__ == "__main__":
    main()
