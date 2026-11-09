"""Published Pi sessions adopt native startup and update their task policy after close."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--entrypoint", choices=["runWorker", "run"], default="runWorker")
    args = parser.parse_args()
    aw, npm = str(args.aw.resolve()), args.npm.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    source = fixture.parents[4] / "tests/fixtures/resident/pi.mjs"
    for origin, destination in [(fixture / "native-public.mjs", "native-public.mjs"),
                                (fixture / "native-start.mjs", "native-start.mjs"),
                                (fixture / "native-public-run.mjs", "native-public-run.mjs"),
                                (source, "resident-fixture.mjs")]:
        shutil.copyfile(origin, npm / destination)
    with tempfile.TemporaryDirectory(prefix="aw-native-pi-") as temporary:
        root = Path(temporary)
        (root / "home").mkdir(mode=0o700)
        node = shutil.which("node")
        application = "native-public-run.mjs" if args.entrypoint == "run" else "native-public.mjs"
        document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "native-pi"}, "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(root / "s")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"pi": {"adapter": "pi", "argv": [node, str(npm / application), "task with spaces", "--native-option=kept"]}},
                "providers": {}, "events": {}}}
        document["spec"]["agents"]["pi"]["evidence"] = {
            "source_id": "fixture", "endpoint": "http://127.0.0.1:1", "mode": "auto",
            "query_timeout_ms": 100, "query_bytes": 1048576, "max_items": 32, "max_bytes": 262144}
        (root / "aw.yaml").write_text(json.dumps(document))
        document["spec"]["providers"]["deny"] = {"protocol": "native-hook/v1alpha1",
            "transport": {"type": "stdio", "location": "agent", "argv": [node, "-e",
                'process.stdout.write(JSON.stringify({block:true,reason:"fixture denial"}))']},
            "timeout_ms": 1000, "max_output_bytes": 4096, "config": {}}
        document["spec"]["events"]["tool.before"] = {"enabled": True, "required": True,
            "steps": [{"id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}
        (root / "deny.json").write_text(json.dumps(document))
        env = {"PATH": os.environ["PATH"], "HOME": str(root / "home"), "LANG": "C.UTF-8",
               "AW_TEST_EXECUTABLE": aw, "FAKE_ROOT": str(root), "AW_TEST_ROOT": str(root)}
        with (args.output / "service.log").open("wb") as log:
            service = subprocess.Popen([aw, "serve", "--config", root / "aw.yaml", "--state-dir", root / "s"],
                cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                deadline = time.monotonic() + 15
                while not (root / "s/aw.sock").exists():
                    assert service.poll() is None and time.monotonic() < deadline
                    time.sleep(0.05)
                child = subprocess.Popen([node, npm / "native-start.mjs"], cwd=root, env=env,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
                try:
                    stdout, stderr = child.communicate(timeout=90)
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait(timeout=5)
                (args.output / "worker.log").write_bytes(stdout + stderr)
                assert child.returncode == 0, stderr.decode()
                result = json.loads((root / "native-result.json").read_text())
                result["versions"] = {name: json.loads((npm / "node_modules/@earendil-works" /
                    name / "package.json").read_text())["version"] for name in ["pi-coding-agent", "pi-agent-core"]}
                (args.output / "result.json").write_text(json.dumps(result))
                state = json.loads(subprocess.check_output([aw, "status", "--config", root / "aw.yaml"],
                    text=True, timeout=5))["result"]
                assert state["bindings"] == state["events"] == 0, state
            finally:
                if service.poll() is None:
                    os.killpg(service.pid, signal.SIGTERM)
                try:
                    service.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    os.killpg(service.pid, signal.SIGKILL)
                    service.wait(timeout=5)


if __name__ == "__main__":
    main()
