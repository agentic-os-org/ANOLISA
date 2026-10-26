"""Bound admitted strict Post cases against Qoder SDK 1.0.56 / Worker 1.1.78."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from runtime import Processes, await_condition, save

sys.path.insert(0, str(Path(__file__).resolve().parents[5] / "scripts"))
from native_cleanup import own_children, reap_children

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--npm", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--case", choices=["allow", "failure", "timeout", "report", "native-error", "failure-error"])
args = parser.parse_args()
own_children()
fixture = Path(__file__).resolve().parent
evidence = args.output.resolve()
evidence.mkdir(parents=True, exist_ok=False)
sdk = args.npm.resolve() / "node_modules/@qoder-ai/qoder-agent-sdk"
metadata = json.loads((sdk / "package.json").read_text())
assert metadata["version"] == "1.0.56" and metadata["qoderCliVersion"] == "1.1.78"
worker = sdk / "dist/_worker/qoder-worker-runtime.obf.mjs"
assert subprocess.check_output(["node", str(worker), "--version"], text=True, timeout=15).strip() == "1.1.78"
assert os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"), "native Post acceptance requires a PAT"
results = []
for mode in [args.case] if args.case else ["allow", "failure", "timeout", "report", "native-error", "failure-error"]:
    output = evidence / mode
    output.mkdir()
    with tempfile.TemporaryDirectory(prefix="post-", dir=evidence.parent) as temporary:
        case = Path(temporary)
        (case / "work").mkdir()
        (case / "home").mkdir()
        (case / "work/node_modules").symlink_to(args.npm.resolve() / "node_modules", target_is_directory=True)
        shutil.copyfile(fixture / "post-native.mjs", case / "work/app.mjs")
        environment = {**os.environ, "HOME": str(case / "home"), "QODER_CONFIG_DIR": str(case / "home/.qoder"),
                       "QODER_TEST_ROOT": str(case), "QODER_POST_CASE": mode, "XDG_RUNTIME_DIR": str(case)}
        config = case / "aw.json"
        save(config, {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "qoder-post"},
             "spec": {"daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(case / "state")},
                      "execution": {"guarantee": "native_hook", "default_event_budget_ms": 2000},
                      "audit": {"enabled": True, "payload": "metadata_only"},
                      "agents": {"sdk": {"adapter": "qoder-sdk", "argv": [shutil.which("node"), str(case / "work/app.mjs")]}},
                      "providers": {"policy": {"protocol": "aw-provider/v1alpha1", "config": {}, "timeout_ms": 1000,
                          "max_output_bytes": 1024, "transport": {"type": "stdio", "location": "agent",
                          "argv": [sys.executable, str(fixture / "post-policy.py")]}}},
                      "events": {event: {"enabled": True, "steps": [{"id": label, "provider": "policy",
                                 "operation": "check", "effects": ["observe"], "on_error": action}]}
                                 for event, label, action in [("tool.before", "before", "block"),
                                 ("tool.after", "after", "report" if mode == "report" else "withhold_result")]}}})
        processes = Processes(case, output, environment)
        try:
            processes.start("service", [str(args.aw.resolve()), "serve", "--config", str(config), "--state-dir", str(case / "state")])
            await_condition(lambda: (case / "state/aw.sock").exists(), 10, "AW service readiness")
            run = processes.start("worker", [str(args.aw.resolve()), "run", "--config", str(config), "--agent", "sdk"])
            for _, _, record in processes.items:
                record["deadline_seconds"] = 120
            processes.record()
            assert processes.wait(run, 110) == 0, f"{mode} failed; inspect worker.log"
            status = json.loads(subprocess.check_output([str(args.aw.resolve()), "service", "status", "--config", str(config)],
                                cwd=case / "work", env=environment, text=True, timeout=10))
            assert status["result"]["bindings"] == 0 and status["result"]["events"] == 0
            save(output / "status.json", status)
            results.append(json.loads((case / "results.json").read_text()))
        finally:
            for name in ("events.jsonl", "daemon-process.json", "results.json"):
                if (case / name).exists():
                    shutil.copyfile(case / name, output / name)
            processes.close()
            reaped = reap_children()
            pids = json.loads((case / "daemon-process.json").read_text())["pids"] if (case / "daemon-process.json").exists() else []
            assert all(not Path(f"/proc/{pid}").exists() for pid in pids)
            for log in output.iterdir():
                if log.is_file():
                    log.write_text(log.read_text(errors="replace").replace(environment["QODER_PERSONAL_ACCESS_TOKEN"], "[REDACTED]"))
            save(output / "cleanup.json", {"daemon_pids_absent": pids, "reaped": reaped,
                 "processes_stopped": all(item[2].get("pid_absent") for item in processes.items)})
    assert not case.exists()
save(evidence / "results.json", results)
