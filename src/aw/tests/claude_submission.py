"""Real Claude SDK + AW/sec-core submission acceptance with owned local endpoints."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile

FIXTURES = Path(__file__).resolve().parents[1] / "crates/aw-cli/tests/fixtures"
sys.path.insert(0, str(FIXTURES / "sec_core"))
from native import configuration
from runtime import Backend, Processes, await_condition, save


def main():
    def interrupted(signum, frame):
        raise InterruptedError(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binaries = args.bin.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("AW_", "CLAUDE_", "ANTHROPIC_", "AGENT_SEC_"))}
    with tempfile.TemporaryDirectory(prefix="aw-claude-submit-") as temporary:
        root = Path(temporary)
        (root / "work").mkdir()
        processes = Processes(root, output, environment)
        backend = Backend(root, output, processes, binaries, args.image)
        rows = []
        try:
            backend.start()
            for mode in ("foreground", "resident"):
                for action in ("block", "report"):
                    name = mode + "-" + action
                    case = root / name
                    case.mkdir(mode=0o700)
                    work = case / "work"
                    work.mkdir(mode=0o700)
                    evidence = output / name
                    evidence.mkdir()
                    worker = FIXTURES / "claude-sdk/submission_native.py"
                    argv = [sys.executable]
                    if mode == "resident":
                        argv += ["-m", "aw_claude", "--"]
                    argv.append(str(worker))
                    config = configuration("claude-sdk", argv, binaries, case / "s", backend.socket)
                    security = config["spec"]["providers"]["security"]
                    security["transport"]["argv"] = [sys.executable,
                        str(FIXTURES / "claude-sdk/submission_provider.py"),
                        *security["transport"]["argv"]]
                    security["config"].update(prompt={"mode": "block", "scan_mode": "fast"},
                                             pii={"input_mode": "block", "output_mode": "observe"})
                    config["spec"]["events"] = {"input.submit": {
                        "enabled": True, "required": True, "steps": [
                            {"id": step, "provider": "security", "operation": operation,
                             "effects": ["observe", "block"], "on_error": action}
                            for step, operation in [("prompt", "scan_prompt"), ("pii", "scan_user_input")]
                        ]}}
                    save(case / "aw.json", config)
                    env = {**environment, "SUBMISSION_MODE": mode, "SUBMISSION_ACTION": action,
                           "SUBMISSION_CONFIG": str(case / "aw.json")}
                    run = Processes(case, evidence, env)
                    try:
                        run.start("service", [binaries / "aw", "serve", "--config", case / "aw.json",
                                              "--state-dir", case / "s"])
                        await_condition(lambda: (case / "s/aw.sock").exists(), 10, "service startup")
                        child = run.start("worker", [binaries / "aw", "run", "--config",
                                                     case / "aw.json", "--agent", "claude-sdk"])
                        assert run.wait(child, 240) == 0, name
                        run.stop(child)
                        result = json.loads((work / "result.json").read_text())
                        current = json.loads(subprocess.check_output([
                            binaries / "aw", "status", "--config", case / "aw.json",
                        ], env=env, timeout=5))["result"]
                        assert current["bindings"] == current["events"] == 0, current
                        assert current["audit_healthy"], current
                        calls = [json.loads(line) for line in
                                 (work / "submission-calls.jsonl").read_text().splitlines()]
                        turns = {}
                        for call in calls:
                            event = call["event"]
                            assert "tool" not in event
                            key = (event["agent"]["instance_id"], event["turn_id"])
                            previous = turns.setdefault(key, event)
                            assert previous == event, "turn snapshot changed between steps"
                        assert len({event["agent"]["instance_id"] for event in turns.values()}) == (
                            2 if mode == "resident" else 1)
                        save(evidence / "result.json", result)
                        save(evidence / "status.json", current)
                        save(evidence / "calls.json", calls)
                        rows.append({"case": name, "passed": True, "cases": len(result["cases"]),
                                     "bindings": 0, "events": 0})
                    finally:
                        run.close()
                        for path in ("s/journal", "work"):
                            if (case / path).exists():
                                shutil.copytree(case / path, evidence / Path(path).name)
            save(output / "results.json", {"passed": True, "cases": rows})
        finally:
            try:
                backend.recover()
            finally:
                try:
                    processes.close()
                finally:
                    backend.close()
            save(output / "cleanup.json", {"backend_removed": True,
                "processes_stopped": all(item[2].get("stopped") and item[2].get("pid_absent")
                                         for item in processes.items)})


if __name__ == "__main__":
    main()
