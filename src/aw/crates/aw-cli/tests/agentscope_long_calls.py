"""Real SDK acceptance: long call histories recycle correlation capacity.

python tests/agentscope_long_calls.py --aw target/debug/aw --output target/agentscope-long
One worker drives more than the 512-legacy-capacity tool calls through real
`aw scope` ledger registration; assertions cover full execution, Provider
Hook delivery per call, bounded correlations and replay rejection. Requires a
short output path (Linux Unix socket limit) and agentscope==2.0.6.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from importlib.metadata import version
from pathlib import Path
from typing import Any

CALLS = 600

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
        # The before-match Provider records one line per successful Hook
        # delivery; a green tool result alone cannot prove the Hook executed.
        hook_log = root / "hook-calls.log"
        record_hook = (
            "cat >/dev/null; printf true | tee -a "
            + shlex.quote(str(hook_log))
            + "; echo >> "
            + shlex.quote(str(hook_log))
        )
        config = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-long"},
            "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 55000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "agentscope",
                        "argv": [sys.executable, str(fixture / "long_worker.py")],
                    }
                },
                "providers": {
                    "before-match": {
                        "transport": {
                            "type": "stdio",
                            "location": "agent",
                            "argv": ["/bin/sh", "-c", record_hook],
                        },
                        "timeout_ms": 2000,
                        "max_output_bytes": 4096,
                    }
                },
                "events": {
                    "tool.before": {
                        "enabled": True,
                        "required": True,
                        "steps": [
                            {
                                "id": "gate",
                                "provider": "before-match",
                                "effects": ["observe"],
                                "on_error": "report",
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
        proc = subprocess.Popen(cmd, cwd=root, stdout=logs, stderr=logs, start_new_session=True)
        record = {"command": cmd, "cwd": str(root), "pid": proc.pid,
                  "stop": f"kill -TERM -- -{proc.pid}", "lifetime_seconds": 300}
        (args.output / "process.json").write_text(json.dumps(record))
        try:
            deadline = time.monotonic() + 30
            while not (state / "aw.sock").exists():
                assert proc.poll() is None, "service exited"
                assert time.monotonic() < deadline, "service startup timed out"
                time.sleep(0.01)
            # aw run injects aw_agentscope next to the worker via AW_NATIVE_CONFIG.
            command = [str(aw), "run", "--config", str(path), "--agent", "sdk"]
            worker = subprocess.run(
                command, cwd=root, capture_output=True, text=True, timeout=280,
            )
            assert worker.returncode == 0, worker.stderr[-4000:]
            # Every correlated Hook must reach its Provider and succeed; a
            # ledger mismatch would silently skip the check in report mode.
            assert "invocation_input_mismatch" not in worker.stderr, worker.stderr[-4000:]
            hook_calls = hook_log.read_text().splitlines()
            assert hook_calls == ["true"] * CALLS, (len(hook_calls), hook_calls[:3])
            report = json.loads(worker.stdout)
            context = report["context"]
            results = [
                block
                for message in context
                for block in message.get("content", [])
                if isinstance(block, dict) and block.get("type") == "tool_result"
            ]
            assert len(results) == CALLS, len(results)
            states = {block["state"] for block in results}
            assert states == {"success"}, states
            texts = sorted(block["output"][0]["text"] for block in results)
            assert texts == sorted(f"complete-{index}" for index in range(CALLS)), texts[:5]
            # The worker replays its first registration sequence against the
            # live ledger; only an explicit replay rejection proves the guard.
            replay = report["replay"]
            assert replay["rejected"], replay
            assert "replay" in replay["diagnostic"], replay
            # The process scope closes at worker exit: no binding, event or
            # correlation may remain on the service.
            status = subprocess.run(
                [str(aw), "status", "--config", str(path)],
                capture_output=True, text=True, timeout=10,
            )
            assert status.returncode == 0, status.stderr[-2000:]
            usage = json.loads(status.stdout)["result"]
            assert usage["bindings"] == 0, usage
            assert usage["events"] == 0, usage
            (args.output / "result.json").write_text(
                json.dumps({"sdk_version": version("agentscope"), "calls": CALLS,
                            "states": sorted(states), "hook_calls": len(hook_calls),
                            "replay_rejected": True,
                            "bindings": usage["bindings"], "events": usage["events"]},
                           indent=2)
            )
        finally:
            try:
                subprocess.run([str(aw), "stop", "--socket", str(state / "aw.sock")],
                               timeout=5, capture_output=True)
                proc.wait(timeout=5)
            except (subprocess.TimeoutExpired, FileNotFoundError):
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
            logs.close()
            record.update(exited=True, returncode=proc.returncode)
            (args.output / "process.json").write_text(json.dumps(record))
    print("AgentScope 2.0.6 long-call recycling acceptance passed")


if __name__ == "__main__":
    main()
