"""Real AgentScope resident updates, occupied replies and immutable hook routing."""
import argparse
import copy
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
from importlib.metadata import version

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from runtime import Processes, await_condition, checked, save


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--native-start", action="store_true")
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="aw-as-idle-") as temporary:
        root = Path(temporary)
        (root / "work").mkdir()
        env = {**os.environ, "AW_TEST_ROOT": str(root)}
        processes = Processes(root, output, env)
        aw = args.aw.resolve()
        config, candidate = root / "current.json", root / "next.json"
        document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-idle"}, "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"sdk": {"adapter": "agentscope", "argv": [sys.executable, str(fixture / ("native_start.py" if args.native_start else "idle_worker.py"))]}},
                "providers": {}, "events": {}}}
        blocked = copy.deepcopy(document)
        blocked["spec"]["providers"]["deny"] = {"protocol": "native-hook/v1alpha1", "transport": {
            "type": "stdio", "location": "agent", "argv": ["/bin/sh", "-c", "exit 2"]},
            "timeout_ms": 500, "max_output_bytes": 4096, "config": {}}
        blocked["spec"]["events"]["tool.before"] = {"enabled": True, "steps": [{
            "id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}
        submitted = copy.deepcopy(document)
        submitted["spec"]["providers"]["scan"] = {"protocol": "aw-provider/v1alpha1", "transport": {
            "type": "stdio", "location": "agent", "argv": [sys.executable, str(fixture / "submission_provider.py"), str(root)]},
            "timeout_ms": 500, "max_output_bytes": 4096, "config": {}}
        submitted["spec"]["events"]["input.submit"] = {"enabled": True, "steps": [{
            "id": "scan", "provider": "scan", "operation": "scan_prompt", "effects": ["observe", "block"], "on_error": "block"}]}
        save(config, document)
        results = []

        def status():
            return json.loads(checked([aw, "status", "--config", config], env=env).stdout)["result"]

        def update(value, busy=False):
            save(candidate, value)
            command = ([aw, "instance", "update", "--instance", status()["instances"][0]["instance_id"]]
                       if args.native_start else [aw, "update", "--agent", "sdk"])
            completed = subprocess.run([*command, "--config", config, "--policy", candidate],
                                       env=env, capture_output=True, text=True, timeout=15)
            assert (completed.returncode != 0) == busy, completed
            if busy:
                assert "busy" in completed.stderr.lower() or "busy" in completed.stdout.lower(), completed
            else:
                results.append(json.loads(completed.stdout))

        def start(index, mode="allow", text="hello"):
            save(root / f"turn-{index}.json", {"mode": mode, "text": text})

        def finish(index):
            result = root / f"result-{index}.json"
            await_condition(result.exists, 30, "reply result")
            value = json.loads(result.read_text())
            await_condition(lambda: status()["instances"][0]["requests"] == 0, 10, "reply drain")
            return value

        def executed():
            path = root / "executed.jsonl"
            return len(path.read_text().splitlines()) if path.exists() else 0

        try:
            daemon = processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", root / "state"])
            await_condition(lambda: (root / "state/aw.sock").exists(), 10, "service startup")
            if args.native_start:
                env.update(AW_TEST_EXECUTABLE=str(aw), AW_TEST_CONFIG=str(config))
                worker = processes.start("worker", [sys.executable, fixture / "native_start.py"])
            else:
                worker = processes.start("worker", [aw, "run", "--config", config, "--agent", "sdk"])
            await_condition(lambda: (root / "worker-ready").exists() or processes.exited(worker), 30, "factory ready")
            assert not processes.exited(worker)
            identity = status()["instances"][0]["process"]
            start(0)
            assert finish(0)["outcome"] == "ok" and executed() == 1
            update(blocked)
            start(1)
            assert finish(1)["outcome"] == "ok" and executed() == 1
            update(document)
            start(2, "hold-model")
            await_condition(lambda: (root / "model-2").exists(), 15, "active reasoning")
            update(blocked, busy=True)
            (root / "release-model").touch()
            assert finish(2)["outcome"] == "ok" and executed() == 2
            start(3, "hold-tool")
            await_condition(lambda: (root / "tool-entered").exists(), 15, "active tool")
            update(blocked, busy=True)
            (root / "release-tool").touch()
            assert finish(3)["outcome"] == "ok" and executed() == 3
            update(submitted)
            start(4, text="risk")
            assert finish(4) == {"outcome": "SubmissionDenied", "rounds": 0} and executed() == 3
            start(5)
            assert finish(5)["outcome"] == "ok" and executed() == 4
            update(document)
            start(6, text="risk")
            assert finish(6)["outcome"] == "ok" and executed() == 5
            final = status()
            assert final["instances"][0]["process"] == identity
            assert final["events"] == final["instances"][0]["requests"] == final["instances"][0]["tools"] == 0
            (root / "stop-worker").touch()
            await_condition(lambda: processes.exited(worker), 15, "worker exit")
            assert status()["bindings"] == 0
            if args.native_start:
                assert not list((root / "state").glob("native-*"))
            save(output / "result.json", {"sdk_version": version("agentscope"), "turns": 7,
                "updates": len(results), "busy_rejections": 2, "executed": executed(), "same_worker": True, "native_start": args.native_start})
            save(output / "updates.json", results)
        finally:
            processes.close()


if __name__ == "__main__":
    main()
