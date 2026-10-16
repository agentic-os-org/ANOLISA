#!/usr/bin/env python3
"""Exercise the published Pi factory and tool loop through aw run without a model or cloud."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
from uuid import UUID

parser = argparse.ArgumentParser()
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--root", type=Path, required=True)
args = parser.parse_args()
root = args.root.resolve()
root.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
shutil.copyfile(fixture / "application.mjs", root / "application.mjs")
aw = str(args.aw.resolve())
node = shutil.which("node")
assert node


def call(command, *, input=None, env=None):
    with subprocess.Popen(command, cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, env=env, start_new_session=True) as proc:
        record = {"command": command, "cwd": str(root), "pid": proc.pid, "ports": [],
                  "deadline_seconds": 30, "stop": f"kill -TERM -- -{proc.pid}"}
        (root / "active-command.json").write_text(json.dumps(record))
        try:
            out, err = proc.communicate(input, timeout=30)
        finally:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait()
        return proc.returncode, out, err


def provider(state, mode, protocol="boolean"):
    value = {"transport": {"type": "stdio", "location": "agent",
             "argv": [shutil.which("python3"), str(fixture / "policy.py"), str(state), mode]},
             "timeout_ms": 2000, "max_output_bytes": 4096 if protocol == "structured" else 1024}
    if protocol != "boolean":
        value.update(protocol="aw-provider/v1alpha1" if protocol == "structured" else "native-hook/v1alpha1", config={})
    return value


def configure(state, extra):
    document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "pi-smoke"}, "spec": {
        "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(state / "state")},
        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
        "audit": {"enabled": True, "payload": "metadata_only"},
        "agents": {"pi-test": {"adapter": "pi", "argv": [node, str(root / "application.mjs")]}},
        "providers": {}, "events": {}}}
    for event, label in [("tool.before", "before"), ("tool.after", "after")]:
        mode = "error" if extra.get("PI_POLICY_ERROR" if label == "before" else "PI_AFTER_ERROR") else "wait" if extra.get("PI_WAIT") else "normal"
        protocol = "boolean"
        if extra.get("PI_STRUCTURED") or (label == "after" and extra.get("PI_AW_REPLACE")):
            protocol = "structured"
        if label == "after" and extra.get("PI_AW_REPLACE"):
            mode = "replace"
        native = extra.get("PI_NATIVE_BEFORE" if label == "before" else "PI_NATIVE_AFTER")
        if native:
            protocol, mode = "native", native
        document["spec"]["providers"][label] = provider(state, mode, protocol)
        step = {"id": label, "provider": label, "on_error": "block" if label == "before" else "report"}
        if protocol == "native":
            step["native"] = {}
        else:
            step["effects"] = ["observe", "block"] if label == "before" else ["observe", "replace_result"] if extra.get("PI_AW_REPLACE") else ["observe"]
            if protocol == "structured":
                step["operation"] = "check" if label == "before" else "record"
        document["spec"]["events"][event] = {"enabled": True, "required": True, "steps": [step]}
    if extra.get("PI_MULTI_PRE"):
        document["spec"]["providers"]["tail"] = provider(state, "normal")
        document["spec"]["events"]["tool.before"]["steps"].append(
            {"id": "tail", "provider": "tail", "effects": ["observe"], "on_error": "block"})
    return document


results = []
cases = {
    "late-before-denial": {"PI_LATE_CALLBACK": "beforeToolCall", "PI_DENY": "1"},
    "late-after-replacement": {"PI_LATE_CALLBACK": "afterToolCall", "PI_AW_REPLACE": "1"},
    "late-before-replacement-session": {"PI_LATE_CALLBACK": "beforeToolCall", "PI_DENY": "1", "PI_FACTORY_MODE": "new"},
    "late-after-replacement-session": {"PI_LATE_CALLBACK": "afterToolCall", "PI_AW_REPLACE": "1", "PI_FACTORY_MODE": "new"},
    "allow": {}, "deny": {"PI_DENY": "1"}, "tool-error": {"PI_FAIL": "1"},
    "native-mutation": {"PI_MUTATE": "1"}, "native-replacement": {"PI_REPLACE": "1"},
    "two-sessions-parallel": {"PI_TWO_SESSIONS": "1", "PI_PARALLEL": "1"},
    "before-error-block": {"PI_POLICY_ERROR": "1"}, "after-error-report": {"PI_AFTER_ERROR": "1"},
    "await-both-hooks": {"PI_WAIT": "1"},
    **{f"factory-{mode}": {"PI_FACTORY_MODE": mode} for mode in ["new", "resume", "fork", "import"]},
    "block-reason-model": {"PI_DENY": "1", "PI_STRUCTURED": "1"},
    "aw-replacement-metadata": {"PI_AW_REPLACE": "1", "PI_METADATA": "1"},
    "aw-replacement-parallel": {"PI_AW_REPLACE": "1", "PI_METADATA": "1", "PI_PARALLEL": "1"},
    "aw-replacement-error-neutral": {"PI_AW_REPLACE": "1", "PI_METADATA": "1", "PI_FAIL": "1"},
    "aw-replacement-multiblock-neutral": {"PI_AW_REPLACE": "1", "PI_MULTI": "1"},
    "aw-replacement-image-neutral": {"PI_AW_REPLACE": "1", "PI_IMAGE": "1"},
    "native-invalid-before": {"PI_NATIVE_BEFORE": "native-invalid"},
    "native-invalid-after-neutral": {"PI_NATIVE_AFTER": "native-invalid"},
    "native-forged-after-neutral": {"PI_NATIVE_AFTER": "native-forged"},
    "native-empty-neutral": {"PI_NATIVE_BEFORE": "native-empty", "PI_NATIVE_AFTER": "native-empty"},
    "native-block-reason-model": {"PI_NATIVE_BEFORE": "native-block"},
    "native-after-exception": {"PI_NATIVE_AFTER_ERROR": "1"},
    "native-error-cleared-replaced": {"PI_FAIL": "1", "PI_CLEAR_ERROR": "1", "PI_METADATA": "1", "PI_AW_REPLACE": "1"},
    "native-nullish-success": {"PI_NULLISH": "1", "PI_AW_REPLACE": "1"},
    "native-nullish-error": {"PI_NULLISH": "1", "PI_FAIL": "1", "PI_AW_REPLACE": "1"},
    "reused-native-id": {"PI_REUSE_ID": "1"},
    "early-denial-forty": {"PI_DENY": "1", "PI_ROUNDS": "40", "PI_REUSE_ID": "1", "PI_MULTI_PRE": "1", "PI_CHECK_LIVE": "1"},
    "early-error-block": {"PI_POLICY_ERROR": "1", "PI_MULTI_PRE": "1", "PI_CHECK_LIVE": "1"},
    "early-native-invalid-block": {"PI_NATIVE_BEFORE": "native-invalid", "PI_MULTI_PRE": "1", "PI_CHECK_LIVE": "1"},
}
for case, extra in cases.items():
    state = Path(tempfile.mkdtemp(prefix="p", dir=root))
    if len(os.fsencode(state / "state/aw.sock")) >= 108:
        raise ValueError("Choose a shorter --root: Linux Unix socket path must be under 108 bytes")
    config = state / "aw.json"
    config.write_text(json.dumps(configure(state, extra)))
    env = {**os.environ, "PI_TEST_ROOT": str(state), **extra}
    try:
        code, out, err = call([aw, "run", "--config", str(config), "--agent", "pi-test"], env=env)
        (state / "run.stdout").write_text(out)
        (state / "run.stderr").write_text(err)
        assert code == 0, (case, code, err)
        result = json.loads(out)
        events = [json.loads(line) for line in (state / "policy-events.jsonl").read_text().splitlines()]
        before = [event for event in events if event["name"] == "tool.before"]
        after = [event for event in events if event["name"] == "tool.after"]
        denied = bool(extra.get("PI_DENY") or extra.get("PI_MUTATE") or extra.get("PI_POLICY_ERROR") or extra.get("PI_NATIVE_BEFORE") in {"native-invalid", "native-block"})
        assert bool(result["sideEffects"]) != denied, (case, result)
        expected = 40 if extra.get("PI_ROUNDS") else 4 if extra.get("PI_TWO_SESSIONS") and extra.get("PI_PARALLEL") else 2 if extra.get("PI_PARALLEL") or extra.get("PI_FACTORY_MODE") or extra.get("PI_REUSE_ID") else 1
        assert len(before) == expected, (case, len(before), expected)
        assert len(after) == (0 if denied or extra.get("PI_NATIVE_AFTER_ERROR") else expected)
        if extra.get("PI_LATE_CALLBACK"):
            assert result["callbackMutations"] == [{"key": extra["PI_LATE_CALLBACK"], "rejected": True}] * expected
        assert result["beforeCalls"] == expected
        assert result["afterCalls"] == (0 if denied else expected)
        assert len({event["tool"]["call_id"] for event in before}) == expected
        for event in events:
            invocation = event["tool"]["call_id"]
            assert UUID(invocation).version == 4 and str(UUID(invocation)) == invocation
            assert invocation == event["native"]["invocation_id"]
            assert invocation != event["native"]["tool_call"]["id"]
        for event in after:
            assert any(pre["session_id"] == event["session_id"] and pre["tool"]["call_id"] == event["tool"]["call_id"] for pre in before)
        tool_results = [message for message in result["messages"] if message["role"] == "toolResult"]
        if case.startswith("factory-"):
            assert len(result["sessions"]) == 2
            assert (result["sessions"][0] == result["sessions"][1]) == (case == "factory-resume")
        if extra.get("PI_FACTORY_MODE") or extra.get("PI_TWO_SESSIONS") or extra.get("PI_REUSE_ID"):
            assert {event["session_id"] for event in events} == set(result["sessions"])
        if extra.get("PI_REUSE_ID"):
            assert len(set(result["sessions"])) == 1
            assert {event["native"]["tool_call"]["id"] for event in events} == {"reused-native-id"}
        if extra.get("PI_FAIL"):
            assert after[0]["tool"]["result"]["isError"] == (not bool(extra.get("PI_CLEAR_ERROR")))
            assert tool_results[-1]["isError"] == (not bool(extra.get("PI_CLEAR_ERROR")))
        if case == "native-replacement":
            assert after[0]["tool"]["result"]["content"][0]["text"] == "replacement"
            assert after[0]["tool"]["result"]["isError"] is True
        if extra.get("PI_METADATA"):
            for message in tool_results:
                assert message["details"] == {"native": True, "id": message["toolCallId"]}
                assert message["usage"]["totalTokens"] == 0
        if extra.get("PI_AW_REPLACE") and not ((extra.get("PI_FAIL") and not extra.get("PI_CLEAR_ERROR")) or extra.get("PI_MULTI") or extra.get("PI_IMAGE")):
            assert all(message["content"][0]["text"] == "compact" for message in tool_results)
            assert result["modelToolResults"][-1]["content"][0]["text"] == "compact"
            assert all(message["isError"] is False for message in tool_results)
        if extra.get("PI_NULLISH"):
            assert tool_results[0]["details"] == ({} if extra.get("PI_FAIL") else {"id": "call-0"})
            assert "usage" not in tool_results[0]
        if extra.get("PI_MULTI"):
            assert len(tool_results[0]["content"]) == 2
        if extra.get("PI_IMAGE"):
            assert any(block["type"] == "image" for block in tool_results[0]["content"])
        if case == "block-reason-model":
            assert result["modelToolResults"][-1]["content"][0]["text"] == "Tokenless: retry the suggested command"
        if case == "native-block-reason-model":
            assert result["modelToolResults"][-1]["content"][0]["text"] == "Native policy denied this call"
        if extra.get("PI_NATIVE_AFTER") in {"native-invalid", "native-forged"}:
            assert tool_results[0]["content"][0]["text"] == "tool okay"
            assert tool_results[0]["isError"] is False
            assert "aw hook failed" in err
        if extra.get("PI_PARALLEL"):
            expected_order = [f"call-{round}-{suffix}" for round in range(2 if extra.get("PI_TWO_SESSIONS") else 1) for suffix in ["fast", "slow"]]
            assert [event["native"]["tool_call"]["id"] for event in after] == expected_order
        if extra.get("PI_MUTATE"):
            assert before[0]["tool"]["input"]["deny"] is True
        if extra.get("PI_WAIT"):
            assert (state / "tool.after-waited").exists()
        if extra.get("PI_AFTER_ERROR"):
            assert tool_results[0]["isError"] is False and "aw hook failed" in err
        if extra.get("PI_NATIVE_AFTER_ERROR"):
            assert tool_results[0]["isError"] is True and tool_results[0]["content"][0]["text"] == "native after failed"
        if extra.get("PI_CHECK_LIVE"):
            assert result["liveCounts"] == [0] * expected
        code, out, err = call([aw, "status", "--config", str(config)])
        assert code == 0, err
        status = json.loads(out)["result"]
        assert status["bindings"] == 0 and status["events"] == 0
        assert not list((state / "state").glob("launch-*"))
        audits = []
        for chain in (state / "state/journal").glob("*.jsonl"):
            code, out, err = call([aw, "request", "--socket", str(state / "state/aw.sock")],
                                  input=json.dumps({"method": "audit", "key": chain.stem}))
            assert code == 0, err
            audit = json.loads(out)["result"]
            assert audit["terminal"] is True
            audits.append(audit)
        assert audits, "Missing durable journal"
        (state / "verified-audits.json").write_text(json.dumps(audits, indent=2))
        results.append({"case": case, "artifacts": str(state), "passed": True, "before": len(before), "after": len(after), "sessions": result["sessions"]})
    finally:
        code, out, err = call([aw, "stop", "--config", str(config)])
        (state / "stop.json").write_text(json.dumps({"code": code, "out": out, "err": err}))
        deadline = time.monotonic() + 5
        while (state / "state/aw.sock").exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert not (state / "state/aw.sock").exists(), "Owned service socket still exists"
(root / "public-smoke-results.json").write_text(json.dumps(results, indent=2))
print(json.dumps(results, indent=2))
