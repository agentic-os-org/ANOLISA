"""Two native OS workers share a daemon while keeping independent lifecycles."""
import argparse
import copy
import json
import os
from pathlib import Path
import sys
import tempfile
from importlib.metadata import version

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from runtime import Processes, await_condition, checked, save


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="aw-as-workers-") as temporary:
        root = Path(temporary)
        for directory in ["work", "a", "b"]:
            (root / directory).mkdir()
        processes = Processes(root, output, dict(os.environ))
        aw = args.aw.resolve()
        config, candidate = root / "current.json", root / "next.json"
        document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "agentscope-workers"}, "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"sdk": {"adapter": "agentscope", "argv": [sys.executable, str(fixture / "idle_worker.py")]}},
            "providers": {}, "events": {}}}
        blocked = copy.deepcopy(document)
        blocked["spec"]["providers"]["deny"] = {"protocol": "native-hook/v1alpha1", "transport": {
            "type": "stdio", "location": "agent", "argv": ["/bin/sh", "-c", "exit 2"]},
            "timeout_ms": 500, "max_output_bytes": 4096, "config": {}}
        blocked["spec"]["events"]["tool.before"] = {"enabled": True, "steps": [{
            "id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}
        save(config, document)

        def status():
            return json.loads(checked([aw, "status", "--config", config]).stdout)["result"]

        def roots():
            return [row for row in status()["instances"] if row["parent_instance_id"] is None]

        def begin(name, index, mode="allow"):
            save(root / name / f"turn-{index}.json", {"mode": mode, "text": "hello"})

        def finish(name, index):
            path = root / name / f"result-{index}.json"
            await_condition(path.exists, 30, "native reply")
            assert json.loads(path.read_text())["outcome"] == "ok"

        def count(name):
            path = root / name / "executed.jsonl"
            return len(path.read_text().splitlines()) if path.exists() else 0

        def update(instance, policy):
            save(candidate, policy)
            return json.loads(checked([aw, "update", "--config", config, "--instance", instance,
                                       "--policy", candidate]).stdout)

        try:
            processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", root / "state"])
            await_condition(lambda: (root / "state/aw.sock").exists(), 10, "service startup")
            workers = {}
            for name in ["a", "b"]:
                # Connection installation holds a shared configuration lock.
                worker = processes.start(name, [aw, "run", "--config", config, "--agent", "sdk"],
                                         extra_env={"AW_TEST_ROOT": str(root / name)})
                workers[name] = worker
                await_condition(lambda: (root / name / "worker-ready").exists() or processes.exited(worker),
                                30, "native readiness")
                assert not processes.exited(worker)
            initial = roots()
            assert len(initial) == 2 and all(row["ready"] for row in initial)
            identities = {name: next(row for row in initial if row["process"]["pid"] ==
                          int((root / name / "worker-ready").read_text())) for name in workers}
            assert identities["a"]["instance_id"] != identities["b"]["instance_id"]
            assert len(list((root / "state").glob("launch-*"))) == 2
            begin("a", 0, "hold-tool")
            await_condition(lambda: (root / "a/tool-entered").exists(), 15, "first worker tool")
            begin("b", 0)
            finish("b", 0)
            assert count("a") == count("b") == 1
            await_condition(lambda: next(row for row in roots() if row["instance_id"] == identities["b"]["instance_id"])["requests"] == 0,
                            10, "second worker idle")
            update(identities["b"]["instance_id"], blocked)
            snapshot = {row["instance_id"]: row for row in roots()}
            assert snapshot[identities["a"]["instance_id"]]["policy_revision"] == identities["a"]["policy_revision"]
            assert snapshot[identities["a"]["instance_id"]]["requests"] == 1
            begin("b", 1)
            finish("b", 1)
            assert count("b") == 1
            # Terminate only the first launcher and its native worker.
            processes.stop(workers["a"])
            await_condition(lambda: len(roots()) == 1, 15, "terminated worker reclaimed")
            assert roots()[0]["instance_id"] == identities["b"]["instance_id"]
            assert len(list((root / "state").glob("launch-*"))) == 1
            update(identities["b"]["instance_id"], document)
            for index in range(2, 7):
                begin("b", index)
                finish("b", index)
            assert count("b") == 6 and not processes.exited(workers["b"])
            final = status()
            assert final["events"] == final["instances"][0]["requests"] == final["instances"][0]["tools"] == 0
            (root / "b/stop-worker").touch()
            await_condition(lambda: processes.exited(workers["b"]), 15, "second worker exit")
            await_condition(lambda: status()["bindings"] == 0, 15, "all bindings reclaimed")
            assert not list((root / "state").glob("launch-*"))
            save(output / "result.json", {"sdk_version": version("agentscope"), "workers": 2,
                "independent_ready": True, "selected_updates": 2, "surviving_worker_turns": 7,
                "surviving_worker_executions": count("b"), "released_bindings": True})
            save(output / "initial-instances.json", initial)
        finally:
            processes.close()


if __name__ == "__main__":
    main()
