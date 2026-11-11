"""Verify same-call rewrites through the pinned SDK, native CLI and real Tokenless."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile

from test_claude_sdk_native import document


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", required=True, type=Path)
    parser.add_argument("--tokenless", required=True, type=Path)
    parser.add_argument("--tokenless-provider", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    worker = Path(__file__).resolve().parents[1] / "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py"
    reports = []
    for denied in [False, True]:
        case = "denied" if denied else "allowed"
        output = args.output / case
        output.mkdir()
        mode = "bash-same-call-deny" if denied else "bash-same-call"
        with tempfile.TemporaryDirectory(prefix="aw-cli-rewrite-") as temporary:
            state = Path(temporary) / "state"
            config = output / "aw.json"
            value = document(sys.executable, worker, state, mode,
                             args.tokenless.resolve(), args.tokenless_provider.resolve())
            value["spec"]["events"]["tool.before"]["steps"].insert(0, {
                "id":"rewrite", "provider":"tokenless", "operation":"pre_tool",
                "effects":["replace_input"], "on_error":"block"})
            config.write_text(json.dumps(value))
            subprocess.run(["git", "init", "-q", str(output)], check=True, timeout=10)
            environment = {**os.environ, "CLAUDE_CONFIG_DIR":str(output / "profile"),
                           "CLAUDE_TEST_MODE":mode, "TOKENLESS_STATS_ENABLED":"0",
                           "TOKENLESS_SLS_ENABLED":"0", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1"}
            command = [str(args.aw.resolve()), "run", "--config", str(config), "--agent", "claude"]
            process = subprocess.Popen(command, cwd=output, env=environment, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, text=True, start_new_session=True)
            (output / "process.json").write_text(json.dumps({"pid":process.pid,
                "timeout_seconds":90, "stop":f"kill -TERM -- -{process.pid}"}))
            try:
                stdout, stderr = process.communicate(timeout=90)
                (output / "stdout.log").write_text(stdout)
                (output / "stderr.log").write_text(stderr)
                assert process.returncode == 0, stderr
                native = json.loads((output / "report.json").read_text())
                policy = [json.loads(line) for line in (output / "policy-events.jsonl").read_text().splitlines()]
                before = [event for event in policy if event["name"] == "tool.before"]
                after = [event for event in policy if event["name"] == "tool.after"]
                assert len(before) == 1 and "TOKENLESS_AGENT_ID=" in before[0]["tool"]["input"]["command"], policy
                assert len(native["tool_results"]) == 1, native
                assert bool(native["tool_results"][0].get("is_error")) == denied, native
                assert len(after) == (0 if denied else 1), policy
                if after:
                    assert after[0]["tool"]["input"] == before[0]["tool"]["input"]
                reports.append({"case":case, "passed":True, "before":len(before), "after":len(after)})
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)
                subprocess.run([str(args.aw.resolve()), "stop", "--config", str(config)],
                               env=environment, capture_output=True, timeout=15, check=True)
                shutil.copytree(state / "journal", output / "journal")
    (args.output / "result.json").write_text(json.dumps(reports, indent=2))
    print(json.dumps(reports))


if __name__ == "__main__":
    main()
