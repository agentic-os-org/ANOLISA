"""Check Claude native late Post adoption and fail-closed final-gate admission."""

import argparse
import json
import os
from pathlib import Path
import shutil
import platform
import socket
import subprocess
import sys
import tempfile

from test_claude_sdk_native import document

FIXTURES = Path(__file__).resolve().parents[1] / "crates/aw-cli/tests/fixtures"
sys.path.insert(0, str(FIXTURES / "sec_core"))
from runtime import Processes, await_condition, save
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from native_cleanup import own_children, reap_children

RELEASES = {"0.2.88": "2.1.161 (Claude Code)", "0.2.182": "2.1.325 (Claude Code)"}


def run(aw, output, mode, node=None):
    output.mkdir()
    with tempfile.TemporaryDirectory(prefix="aw-cfg-") as temporary:
        case = Path(temporary)
        work = case / "work"
        work.mkdir()
        environment = {**os.environ, "CLAUDE_CONFIG_DIR": str(case / "profile"),
                       "CLAUDE_FINAL_CASE": mode,
                       "CLAUDE_TEST_CONFIG": str(case / "aw.json"), "CLAUDE_TEST_AW": str(aw), "CLAUDE_TEST_MODE": "allow",
                       "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}
        worker = FIXTURES / "claude-sdk/final_native.py"
        config = case / "aw.json"
        value = document(sys.executable, worker, case / "state", "allow")
        if node:
            name = "typescript-before.mjs" if mode.startswith("ts-before-") else "final_typescript.mjs"
            value["spec"]["agents"]["claude"]["argv"] = [str(node), str(worker.with_name(name))]
        value["spec"]["daemon"]["startup"] = "external"
        if mode.startswith("ts-before-"):
            value["spec"]["execution"]["default_event_budget_ms"] = 1000
            tail = dict(value["spec"]["events"]["tool.before"]["steps"][0], id="tail")
            value["spec"]["events"]["tool.before"]["steps"].append(tail)
        if mode == "required-final" or "-gate-" in mode:
            value["spec"]["events"]["security.violation"] = {
                "enabled": True, "required": True, "steps": [{"id": "final",
                    "provider": "policy", "effects": ["observe", "block"], "on_error": "block"}]}
            value["spec"]["events"]["security.violation"]["steps"].append({
                "id": "final-tail", "provider": "policy", "effects": ["observe", "block"], "on_error": "block"})
            if mode == "required-final":
                value["spec"]["events"]["security.violation"]["steps"][0]["on_error"] = "report"
        save(config, value)
        processes = Processes(case, output, environment)
        result = None
        try:
            processes.start("service", [str(aw), "serve", "--config", str(config),
                                        "--state-dir", str(case / "state")])
            await_condition(lambda: (case / "state/aw.sock").exists(), 10, "AW readiness")
            process = processes.start("worker", [str(aw), "run", "--config", str(config), "--agent", "claude"])
            code = processes.wait(process, 240 if os.environ.get("CLAUDE_HOSTED_SETTINGS") else 90)
            if mode == "required-final":
                log = (output / "worker.log").read_text()
                assert code != 0 and "Claude final gate requires a structured policy with on_error: block" in log, log
                assert not (work / "server.json").exists() and not (work / "effect").exists()
                result = {"case": mode, "passed": True, "final_gate": "fail_closed",
                          "worker_started": False, "model_requests": 0}
            else:
                assert code == 0, (output / "worker.log").read_text()
                result = json.loads((work / "result.json").read_text())
                releases = {"0.3.325": "2.1.325 (Claude Code)"} if node else RELEASES
                assert result["sdk"] in releases, result["sdk"]
                assert result["cli"] == releases[result["sdk"]], result["cli"]
                events = [json.loads(line) for line in (work / "policy-events.jsonl").read_text().splitlines()]
                after = [event for event in events if event["name"] == "tool.after"]
                tool_count = 3 if mode.endswith("-history") else (2 if mode == "mcp-gate-multi-late" else 1)
                assert len(after) == tool_count and "AW_CLAUDE_FINAL_SECRET_MARKER" not in json.dumps(after)
                result["aw_post_observed_original"] = True
                if "-gate-" in mode:
                    checks = [event for event in events if event["name"] == "security.violation"]
                    expected_checks = 6 if mode.endswith("-history") else (2 if mode.endswith("safe") or mode == "mcp-gate-multi-late" else 1)
                    assert len(checks) == expected_checks, checks
                    assert {check["tool"]["call_id"] for check in checks} == {post["tool"]["call_id"] for post in after}
                    assert ("AW_CLAUDE_FINAL_SECRET_MARKER" in json.dumps(checks)) == mode.endswith(("late", "timeout", "error"))
                    result["final_gate"] = "verified"
            status = json.loads(subprocess.check_output(
                [str(aw), "status", "--config", str(config)], timeout=10))["result"]
            assert status["bindings"] == status["events"] == 0, status
            result["bindings_released"] = True
        finally:
            for path in work.glob("*.json*"):
                shutil.copyfile(path, output / path.name)
            journal = case / "state/journal"
            if journal.exists():
                shutil.copytree(journal, output / "journal")
            processes.close()
            reaped = reap_children()
            if (work / "cli-process.json").exists():
                pid = json.loads((work / "cli-process.json").read_text())["pid"]
                assert not Path(f"/proc/{pid}").exists(), pid
            if (work / "server.json").exists():
                port = json.loads((work / "server.json").read_text())["port"]
                with socket.socket() as connection:
                    connection.settimeout(1)
                    assert connection.connect_ex(("127.0.0.1", port)) != 0, port
            save(output / "cleanup.json", {"processes_stopped": True, "reaped": reaped})
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aw", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--hosted-settings", type=Path,
                        help="Opt in to paid inference through the configured Anthropic gateway")
    parser.add_argument("--cases", nargs="+")
    parser.add_argument("--node", type=Path, help="Run the TypeScript SDK fixture with this Node executable")
    parser.add_argument("--npm", type=Path, help="Prefix containing the pinned TypeScript SDK installation")
    parser.add_argument("--expected-platform", choices=["aarch64", "x86_64"])
    args = parser.parse_args()
    if args.expected_platform and platform.machine() != args.expected_platform:
        parser.error("native platform differs from the scheduled target")
    if args.node and args.hosted_settings:
        parser.error("hosted capture currently uses the Python SDK fixture")
    if args.npm:
        os.environ["NODE_PATH"] = str(args.npm.resolve() / "node_modules")
    args.output.mkdir(parents=True, exist_ok=False)
    if args.hosted_settings:
        os.environ["CLAUDE_HOSTED_SETTINGS"] = str(args.hosted_settings.resolve(strict=True))
    own_children()
    results = [run(args.aw.resolve(), args.output.resolve() / mode, mode, args.node)
               for mode in (args.cases or ["bash-safe", "bash-late", "mcp-safe", "mcp-late", "required-final",
                            "bash-gate-safe", "bash-gate-late", "mcp-gate-safe", "mcp-gate-late",
                            "bash-gate-timeout", "mcp-gate-error", "mcp-gate-multi-late"])]
    save(args.output / "result.json", results)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
