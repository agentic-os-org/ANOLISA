"""Bound real SDK acceptance and retain only redacted evidence outside its disposable profile."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from runtime import Processes, save

sys.path.insert(0, str(Path(__file__).resolve().parents[5] / "scripts"))
from native_cleanup import own_children, reap_children

own_children()

parser = argparse.ArgumentParser()
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--npm", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
modes = parser.add_mutually_exclusive_group()
modes.add_argument("--reloadable", action="store_true")
modes.add_argument("--resident-reload", action="store_true")
args = parser.parse_args()
evidence = args.output.resolve()
evidence.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
sdk = args.npm.resolve() / "node_modules/@qoder-ai/qoder-agent-sdk"
metadata = json.loads((sdk / "package.json").read_text())
assert metadata["version"] == "1.0.56" and metadata["qoderCliVersion"] == "1.1.78"
worker = sdk / "dist/_worker/qoder-worker-runtime.obf.mjs"
assert (
    subprocess.check_output(["node", str(worker), "--version"], text=True, timeout=15).strip()
    == "1.1.78"
)
assert os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"), "Qoder native acceptance requires a PAT"
with tempfile.TemporaryDirectory(prefix="aw-qsdk-") as temporary:
    case = Path(temporary)
    (case / "work").mkdir()
    (case / "home").mkdir()
    (case / "work/node_modules").symlink_to(
        args.npm.resolve() / "node_modules", target_is_directory=True
    )
    application = "native-resident-policy.mjs" if args.resident_reload else "native.mjs"
    shutil.copyfile(fixture / application, case / "work/app.mjs")
    shutil.copyfile(fixture / "native-safety.mjs", case / "work/native-safety.mjs")
    shutil.copyfile(fixture / "native-timeout.mjs", case / "work/native-timeout.mjs")
    environment = {
        **os.environ,
        "HOME": str(case / "home"),
        "QODER_CONFIG_DIR": str(case / "home/.qoder"),
        "QODER_TEST_ROOT": str(case),
        "XDG_RUNTIME_DIR": str(case),
    }
    config = case / "aw.json"
    providers = {
        "policy": {
            "protocol": "aw-provider/v1alpha1",
            "config": {},
            "transport": {
                "type": "stdio",
                "location": "agent",
                "argv": [sys.executable, str(fixture / "policy.py")],
            },
            "timeout_ms": 2000,
            "max_output_bytes": 1024,
        }
    }
    save(
        config,
        {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "qoder-sdk"},
            "spec": {
                "daemon": {
                    "startup": "external",
                    "endpoint": "auto",
                    "state_dir": str(case / "state"),
                },
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "qoder-sdk",
                        "argv": [shutil.which("node"), str(case / "work/app.mjs")],
                    }
                },
                "providers": providers,
                "events": {
                    event: {
                        "enabled": True,
                        "steps": [
                            {
                                "id": label,
                                "provider": "policy",
                                "operation": "check",
                                "effects": effects,
                                "on_error": action,
                            }
                        ],
                    }
                    for event, label, effects, action in [
                        ("tool.before", "before", ["observe", "block"], "block"),
                        ("tool.after", "after", ["observe"], "report"),
                    ]
                },
            },
        },
    )
    if args.reloadable:
        environment["QODER_RELOAD_ACCEPTANCE"] = "1"
        policy = json.loads(config.read_text())
        save(case / "candidate.json", policy)
        policy["spec"]["providers"] = {}
        policy["spec"]["events"] = {}
        save(config, policy)
    if args.resident_reload:
        policy = json.loads(config.read_text())
        policy["spec"]["execution"]["default_event_budget_ms"] = 2400
        save(case / "candidate.json", policy)
        policy["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = "withhold_result"
        save(case / "strict.json", policy)
        policy["spec"]["providers"] = {}
        policy["spec"]["events"] = {}
        save(case / "empty.json", policy)
        save(config, policy)
    processes = Processes(case, evidence, environment)
    try:
        service = processes.start(
            "service",
            [
                str(args.aw.resolve()),
                "serve",
                "--config",
                str(config),
                "--state-dir",
                str(case / "state"),
            ],
        )
        from runtime import await_condition

        await_condition(lambda: (case / "state/aw.sock").exists(), 10, "AW service readiness")
        run = processes.start(
            "worker",
            [str(args.aw.resolve()), "run", "--config", str(config), "--agent", "sdk"]
            + (["--reloadable"] if args.reloadable else []),
        )
        for _, _, record in processes.items:
            record["deadline_seconds"] = 930
        processes.record()
        assert processes.wait(run, 900) == 0, "native worker failed; inspect worker.log"
        status = subprocess.run(
            [str(args.aw.resolve()), "service", "status", "--config", str(config)],
            cwd=case / "work",
            env=environment,
            capture_output=True,
            text=True,
            check=True,
            timeout=10,
        )
        status = json.loads(status.stdout)
        assert status["result"]["bindings"] == 0 and status["result"]["events"] == 0
        assert status["result"]["audit_healthy"]
        save(evidence / "status.json", status)
        shutil.copyfile(case / "results.json", evidence / "results.json")
        shutil.copyfile(case / "daemon-process.json", evidence / "daemon-process.json")
        save(
            evidence / "versions.json",
            {"sdk": metadata["version"], "worker": metadata["qoderCliVersion"]},
        )
    finally:
        for name in (
            "events.jsonl",
            "daemon-process.json",
            "daemon.log",
            "safety.json",
            "timeout.json",
        ):
            if (case / name).exists():
                shutil.copyfile(case / name, evidence / name)
        processes.close()
        reaped = reap_children()
        daemon_pids = []
        if (case / "daemon-process.json").exists():
            daemon_pids = [
                item["pid"]
                for item in json.loads((case / "daemon-process.json").read_text())["identities"]
            ]
        assert all(not Path(f"/proc/{pid}").exists() for pid in daemon_pids)
        token = environment["QODER_PERSONAL_ACCESS_TOKEN"]
        for log in evidence.iterdir():
            if log.is_file() and log.suffix in {".log", ".json", ".jsonl"}:
                log.write_text(log.read_text(errors="replace").replace(token, "[REDACTED]"))
        save(
            evidence / "cleanup.json",
            {
                "daemon_pids_absent": daemon_pids,
                "reaped": reaped,
                "processes_stopped": all(item[2].get("pid_absent") for item in processes.items),
            },
        )
assert not case.exists()
