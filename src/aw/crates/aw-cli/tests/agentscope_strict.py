"""Real SDK acceptance: strict tool.after faults must withhold the native result.

python tests/agentscope_strict.py --aw target/debug/aw --output target/agentscope-strict
The interpreter running this script also runs the fixture worker. It extends
agentscope_native.py with a binding whose tool.after chain is [observer/report,
strict/withhold_result], and drives three fault classes through real
`aw hook` transport against a running service. Requires a short output path
(Linux Unix socket limit).
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from importlib.metadata import version
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    aw = args.aw.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/agentscope"
    with tempfile.TemporaryDirectory(prefix="s-", dir=args.output) as temporary:
        root = Path(temporary).resolve()
        state = root / "state"
        assert (
            len(os.fsencode(state / "aw.sock")) <= 107
        ), "use a short worktree/target output path for Linux Unix sockets"
        config = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-strict"},
            "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 55000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "agentscope",
                        "argv": [sys.executable, str(fixture / "strict_worker.py")],
                    }
                },
                "providers": {
                    "observer": {
                        "transport": {
                            "type": "stdio",
                            "location": "agent",
                            "argv": [
                                sys.executable,
                                str(fixture / "strict_hook.py"),
                                str(root),
                                "observer",
                            ],
                        },
                        "timeout_ms": 4000,
                        "max_output_bytes": 4096,
                        "config": {},
                    },
                    "strict": {
                        "transport": {
                            "type": "stdio",
                            "location": "agent",
                            "argv": [
                                sys.executable,
                                str(fixture / "strict_hook.py"),
                                str(root),
                                "strict",
                            ],
                        },
                        "timeout_ms": 4000,
                        "max_output_bytes": 4096,
                        "config": {},
                    }
                },
                "events": {
                    "tool.after": {
                        "enabled": True,
                        "required": True,
                        "steps": [
                            {
                                "id": "observer",
                                "provider": "observer",
                                "effects": ["observe"],
                                "on_error": "report",
                            },
                            {
                                "id": "strict",
                                "provider": "strict",
                                "effects": ["observe"],
                                "on_error": "withhold_result",
                            }
                        ],
                    }
                },
            },
        }
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        (args.output / "config.json").write_text(json.dumps(config, indent=2))
        logs = (args.output / "serve.log").open("w")
        cmd = [str(aw), "serve", "--config", str(path), "--state-dir", str(state)]
        record = {
            "command": cmd,
            "cwd": str(root),
            "ports": [],
            "log": str(logs.name),
            "lifetime_seconds": 65,
        }
        (args.output / "process.json").write_text(json.dumps(record))
        proc = subprocess.Popen(cmd, cwd=root, stdout=logs, stderr=logs, start_new_session=True)
        record.update(pid=proc.pid, stop=f"kill -TERM -- -{proc.pid}")
        (args.output / "process.json").write_text(json.dumps(record))
        try:
            deadline = time.monotonic() + 10
            while not (state / "aw.sock").exists():
                assert proc.poll() is None, "service exited"
                assert time.monotonic() < deadline, "service startup timed out"
                time.sleep(0.01)
            env = {**os.environ, "AW_TEST_ROOT": str(root)}
            command = [str(aw), "run", "--config", str(path), "--agent", "sdk"]
            stdout = (args.output / "run.stdout").open("wb")
            stderr = (args.output / "run.stderr").open("wb")
            launch_record = {
                "command": command,
                "cwd": str(root),
                "ports": [],
                "logs": [str(stdout.name), str(stderr.name)],
                "lifetime_seconds": 60,
            }
            record_path = args.output / "launch.json"
            record_path.write_text(json.dumps(launch_record))
            child = subprocess.Popen(
                command, env=env, cwd=root, stdout=stdout, stderr=stderr, start_new_session=True
            )
            launch_record.update(pid=child.pid, stop=f"kill -TERM -- -{child.pid}")
            record_path.write_text(json.dumps(launch_record))
            try:
                child.wait(timeout=60)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=3)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=3)
                stdout.close()
                stderr.close()
                launch_record.update(exited=True, returncode=child.returncode)
                record_path.write_text(json.dumps(launch_record))
            assert child.returncode == 0, (args.output / "run.stderr").read_text()
            context = json.loads((args.output / "run.stdout").read_bytes())[0]
            results = {
                block["id"]: block
                for msg in context["context"]
                for block in msg["content"]
                if block["type"] == "tool_result"
            }
            # Each fault class must withhold the original text from the model
            # context and report an error-state placeholder instead.
            stream = (root / "stream.jsonl").read_text()
            model_inputs = (root / "model-inputs.jsonl").read_text()
            deltas = [json.loads(line) for line in stream.splitlines()
                      if json.loads(line)["type"] == "TOOL_RESULT_TEXT_DELTA"]
            assert len(deltas) == 4, deltas
            assert all(event["delta"] not in ["sensi", "tive-"] for event in deltas)
            withheld = 0
            for mode in ["timeout", "error", "invalid"]:
                block = results[f"call-{mode}"]
                text = json.dumps(block["output"])
                assert f"complete-{mode}" not in text, f"{mode} leaked the original result"
                assert "withheld by policy" in text, f"{mode} missed the placeholder: {block}"
                assert block["state"] == "error", block
                assert f"complete-{mode}" not in stream, mode
                assert f"sensitive-{mode}" not in stream, mode
                assert f"complete-{mode}" not in model_inputs, mode
                withheld += 1
            assert withheld == 3, withheld
            # A healthy strict step still allows the checked result through.
            healthy = results["call-success"]
            assert "complete-success" in json.dumps(healthy["output"]), healthy
            assert healthy["state"] == "success", healthy
            assert "complete-success" in stream and "withheld by policy" in stream
            assert "complete-success" in model_inputs and "withheld by policy" in model_inputs
            (args.output / "stream.jsonl").write_text(stream)
            (args.output / "model-inputs.jsonl").write_text(model_inputs)
            # The outer observer stays correlated with the original response in
            # every mode: strict withholding replaces only the final model-visible
            # result, never the payload other after hooks observe.
            callbacks = [
                json.loads(line)
                for line in (root / "strict-callbacks.jsonl").read_text().splitlines()
            ]
            observer_callbacks = 0
            for mode in ["success", "timeout", "error", "invalid"]:
                for label in ["observer", "strict"]:
                    records = [
                        record
                        for record in callbacks
                        if record["label"] == label and record["mode"] == mode
                    ]
                    assert records, f"{label} callback missing for {mode}"
                assert any(
                    record["label"] == "observer"
                    and record["mode"] == mode
                    and record["original"]
                    for record in callbacks
                ), f"observer lost the original result for {mode}"
                observer_callbacks += 1
            assert observer_callbacks == 4, observer_callbacks
            (args.output / "context.json").write_text(json.dumps(context, indent=2))
            (args.output / "result.json").write_text(
                json.dumps(
                    {
                        "sdk_version": version("agentscope"),
                        "withheld": withheld,
                        "healthy": 1,
                        "observer_callbacks": observer_callbacks,
                    },
                    indent=2,
                )
            )
        finally:
            try:
                subprocess.run(
                    [str(aw), "stop", "--socket", str(state / "aw.sock")],
                    timeout=5,
                    capture_output=True,
                )
                proc.wait(timeout=5)
            except (subprocess.TimeoutExpired, FileNotFoundError):
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
            logs.close()
            record.update(exited=True, returncode=proc.returncode)
            (args.output / "process.json").write_text(json.dumps(record))
    print("AgentScope 2.0.6 strict tool.after withholding acceptance passed")


if __name__ == "__main__":
    main()
