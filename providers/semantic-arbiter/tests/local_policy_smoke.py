#!/usr/bin/env python3
"""Drive the real Provider without an Agent, credentials, HTTP server or model replies."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from typing import Any


def encode(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def save(path: Path, value: object) -> None:
    path.write_bytes(encode(value) + b"\n")


def invoke(binary: Path, workspace: Path, request: dict[str, Any], output: Path) -> dict[str, Any]:
    bound = dict(request)
    bound["input_digest"] = "sha256:" + hashlib.sha256(encode(request)).hexdigest()
    save(output.with_suffix(".request.json"), bound)
    completed = subprocess.run(
        [str(binary)], input=encode(bound), cwd=workspace,
        env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"},
        capture_output=True, timeout=10, check=True,
    )
    output.with_suffix(".stdout.json").write_bytes(completed.stdout)
    output.with_suffix(".stderr.json").write_bytes(completed.stderr)
    response = json.loads(completed.stdout)
    diagnostic = json.loads(completed.stderr)
    assert response["status"] == "ok", response
    assert response["effects"] == [], response
    assert response["input_digest"] == bound["input_digest"], response
    return diagnostic


def run_case(
    binary: Path, output: Path, name: str, initial: dict[str, str], later: dict[str, str],
    tool: str, tool_input: dict[str, Any], scenario: str, expected: tuple[str, str | None, str, str],
) -> dict[str, Any]:
    case = output / name
    case.mkdir(mode=0o700)
    workspace = case / "workspace"
    records = case / "records"
    workspace.mkdir(mode=0o700)
    records.mkdir(mode=0o700)
    for path, content in initial.items():
        target = workspace / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
    config = {
        "task": {"allowed_resource_ids": [], "preserve_resource_ids": []},
        "adapter": "qoder", "workspace_path": str(workspace), "workspace_id": "smoke-workspace",
        "tools": {"Write": {"operation": "replace", "path_pointer": "/file_path"},
                  "Edit": {"operation": "replace", "path_pointer": "/file_path"},
                  "Delete": {"operation": "delete", "path_pointer": "/file_path"},
                  "Bash": {"command_pointer": "/command"}},
        "resources": {}, "scenarios": [scenario],
        "model": {"config_revision": "aw_request", "backend": {
            "kind": "bailian", "endpoint_url": "http://127.0.0.1:9",
            "model": "unused-no-model", "key_env": "SMOKE_KEY_INTENTIONALLY_ABSENT"},
            "timeout_ms": 3000, "max_response_bytes": 4096, "transport": "loopback_test"},
        "record_dir": str(records), "max_sessions": 10,
        "max_session_bytes": 16 * 1024 * 1024, "max_storage_bytes": 32 * 1024 * 1024,
    }
    request = {
        "api_version": "aw-provider/v1alpha1", "request_id": name, "method": "invoke",
        "operation": "capture_task_scope", "config_revision": "a" * 64, "budget_ms": 3000,
        "allowed_effects": ["observe"], "config": config,
        "event": {"name": "input.submit", "agent": {
            "adapter": "qoder", "binding_id": "smoke-binding", "instance_id": "smoke-instance"},
            "session_id": name, "turn_id": "turn-one",
            "input": {"text": "Update the greeting file to the requested contents; retain other results."},
            "native": {"cwd": str(workspace)}},
    }
    assert invoke(binary, workspace, request, case / "submit")["code"] == "task_captured"
    for path, content in later.items():
        target = workspace / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
    event = request["event"]
    event.pop("turn_id")
    event.pop("input")
    event["name"] = "tool.before"
    event["tool"] = {"name": tool.lower(), "native_name": tool, "call_id": "call-one",
                     "input": tool_input, "result": None}
    request["operation"] = "evaluate_task_scope"
    request["decision"] = True
    assert invoke(binary, workspace, request, case / "call")["code"] == "shadow_evaluated"
    logs = list(records.glob("session-*/calls.jsonl"))
    assert len(logs) == 1
    stages = {line["stage"]: line["data"] for line in map(json.loads, logs[0].read_text().splitlines())}
    result = stages["result"]["evaluation"]
    trace = result["judgments"][0]
    answer = result["answers"][0]["value"]
    actual = (trace["route"], trace["policy_id"], result["outcome"]["control"]["candidate"], answer)
    assert actual == expected, (name, actual, expected)
    obligations = [item["action"]["kind"] for item in result["outcome"]["obligations"]]
    assert obligations == (["snapshot", "audit"] if expected[3] == "needs_snapshot" else ["audit"])
    assert result["outcome"]["control"]["subject"] == []
    if expected[0] == "local_policy":
        assert "model_request" not in stages
        assert result["model_called"] is False
    else:
        assert stages["model_result"] == {"status": "error", "error": "missing_credentials"}
        assert trace["model_used"] is False
    assert stages["evaluation"]["candidate"]["native_tool"]["input"] == tool_input
    for path, content in {**initial, **later}.items():
        assert (workspace / path).read_text() == content
    assert sorted(str(path.relative_to(workspace)) for path in workspace.rglob("*") if path.is_file()) == sorted({**initial, **later})
    summary = {"case": name, "surface": "direct_provider_process_not_agent_e2e",
               "route": trace["route"], "policy_id": trace["policy_id"],
               "candidate": result["outcome"]["control"]["candidate"],
               "baseline_reason": trace["baseline_reason"], "model_used": trace["model_used"],
               "model_result": stages.get("model_result"), "answer": answer,
               "obligations": obligations, "local_context": stages["evaluation"]["local_context"],
               "effects": [], "log": str(logs[0])}
    save(case / "summary.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-parent", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    parent = args.output_parent.resolve(strict=True)
    output = Path(tempfile.mkdtemp(prefix="local-policy-smoke-", dir=parent))
    cases = [
        ("preexisting-overwrite", {"greeting.txt": "old\n"}, {}, "Write",
         {"file_path": "greeting.txt", "content": "new\n"}, "snapshot",
         ("local_policy", "protect_preexisting_bytes", "allow", "needs_snapshot")),
        ("nested-preexisting-overwrite", {"src/greeting.txt": "old\n"}, {}, "Write",
         {"file_path": "src/greeting.txt", "content": "new\n"}, "snapshot",
         ("local_policy", "protect_preexisting_bytes", "allow", "needs_snapshot")),
        ("preexisting-delete", {"greeting.txt": "old\n"}, {}, "Delete",
         {"file_path": "greeting.txt"}, "snapshot",
         ("local_policy", "protect_preexisting_bytes", "allow", "needs_snapshot")),
        ("unchanged-write", {"greeting.txt": "old\n"}, {}, "Write",
         {"file_path": "greeting.txt", "content": "old\n"}, "snapshot",
         ("local_policy", "no_current_bytes_lost", "allow", "allow")),
        ("unchanged-edit", {"greeting.txt": "old\n"}, {}, "Edit",
         {"file_path": "greeting.txt", "old_string": "old", "new_string": "old"}, "snapshot",
         ("local_policy", "no_current_bytes_lost", "allow", "allow")),
        ("new-file", {}, {}, "Write", {"file_path": "greeting.txt", "content": "new\n"},
         "snapshot", ("local_policy", "no_current_bytes_lost", "allow", "allow")),
        ("session-created-file", {}, {"greeting.txt": "intermediate\n"}, "Write",
         {"file_path": "greeting.txt", "content": "new\n"}, "snapshot",
         ("model_fallback", None, "nop", "unknown")),
        ("directory-delete", {"drafts/greeting.txt": "old\n"}, {}, "Delete",
         {"file_path": "drafts"}, "snapshot", ("model_fallback", None, "nop", "unknown")),
        ("readonly-scope", {}, {}, "Bash", {"command": "pwd"}, "task_scope",
         ("model_fallback", None, "nop", "unknown")),
        ("readonly-snapshot", {}, {}, "Bash", {"command": "git status --short"}, "snapshot",
         ("local_policy", "no_loss_read_only", "allow", "allow")),
        ("readonly-preservation", {}, {}, "Bash", {"command": "ls -la"}, "preservation",
         ("local_policy", "preserve_no_mutation", "allow", "allow")),
        ("sensitive-read-scope", {".env": "SECRET=synthetic\n"}, {}, "Bash",
         {"command": "cat .env"}, "task_scope",
         ("model_fallback", None, "nop", "unknown")),
        ("new-file-preservation", {}, {}, "Write",
         {"file_path": "greeting.txt", "content": "new\n"}, "preservation",
         ("local_policy", "preserve_absent_target", "allow", "allow")),
        ("working-state-snapshot", {}, {}, "Bash",
         {"command": "git reset --hard HEAD"}, "snapshot",
         ("local_policy", "protect_working_state", "allow", "needs_snapshot")),
        ("custom-tool-scope", {}, {}, "CustomTool", {"destination": "draft", "text": "hello"},
         "task_scope", ("model_fallback", None, "nop", "unknown")),
        ("test-change-signal", {"tests/test_greeting.py": "assert True\n"}, {}, "Write",
         {"file_path": "tests/test_greeting.py", "content": "assert 1 == 1\n"}, "task_scope",
         ("model_fallback", None, "nop", "unknown")),
    ]
    results = [run_case(binary, output, *case) for case in cases]
    summary = {"surface": "direct_provider_process_not_agent_e2e", "cases": results,
               "real_model_calls": 0, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    save(output / "summary.json", summary)
    print(json.dumps({"passed": len(results), "real_model_calls": 0, "output": str(output)}))


if __name__ == "__main__":
    main()
