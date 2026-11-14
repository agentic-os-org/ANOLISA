"""Check Claude native late Post adoption and fail-closed final-gate admission."""

import argparse
import json
import os
from pathlib import Path
import shutil
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


def run(aw, output, mode):
    output.mkdir()
    with tempfile.TemporaryDirectory(prefix="aw-cfg-") as temporary:
        case = Path(temporary)
        work = case / "work"
        work.mkdir()
        environment = {**os.environ, "CLAUDE_CONFIG_DIR": str(case / "profile"),
                       "CLAUDE_FINAL_CASE": mode, "CLAUDE_TEST_MODE": "allow",
                       "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}
        worker = FIXTURES / "claude-sdk/final_native.py"
        config = case / "aw.json"
        value = document(sys.executable, worker, case / "state", "allow")
        value["spec"]["daemon"]["startup"] = "external"
        if mode == "required-final":
            value["spec"]["events"]["security.violation"] = {
                "enabled": True, "required": True, "steps": [{"id": "final",
                    "provider": "policy", "effects": ["observe", "block"], "on_error": "block"}]}
        save(config, value)
        processes = Processes(case, output, environment)
        result = None
        try:
            processes.start("service", [str(aw), "serve", "--config", str(config),
                                        "--state-dir", str(case / "state")])
            await_condition(lambda: (case / "state/aw.sock").exists(), 10, "AW readiness")
            process = processes.start("worker", [str(aw), "run", "--config", str(config), "--agent", "claude"])
            code = processes.wait(process, 90)
            if mode == "required-final":
                log = (output / "worker.log").read_text()
                assert code != 0 and "policy admission rejected" in log, log
                assert "Adapter evidence lacks an enabled event" in log, log
                assert not (work / "server.json").exists() and not (work / "effect").exists()
                result = {"case": mode, "passed": True, "final_gate": "unsupported",
                          "worker_started": False, "model_requests": 0}
            else:
                assert code == 0, (output / "worker.log").read_text()
                result = json.loads((work / "result.json").read_text())
                assert result["sdk"] in RELEASES, result["sdk"]
                assert result["cli"] == RELEASES[result["sdk"]], result["cli"]
                events = [json.loads(line) for line in (work / "policy-events.jsonl").read_text().splitlines()]
                after = [event for event in events if event["name"] == "tool.after"]
                assert len(after) == 1 and "AW_CLAUDE_FINAL_SECRET_MARKER" not in json.dumps(after)
                result["aw_post_observed_original"] = True
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
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    own_children()
    results = [run(args.aw.resolve(), args.output.resolve() / mode, mode)
               for mode in ["bash-safe", "bash-late", "mcp-safe", "mcp-late", "required-final"]]
    save(args.output / "result.json", results)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
