"""Exercise application-owned direct/prewarm startup on the pinned, unmodified SDK."""

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
args = parser.parse_args()
fixture = Path(__file__).resolve().parent
sdk = args.npm.resolve() / "node_modules/@qoder-ai/qoder-agent-sdk"
metadata = json.loads((sdk / "package.json").read_text())
assert (metadata["version"], metadata["qoderCliVersion"]) == ("1.0.56", "1.1.78")
assert (
    subprocess.check_output(
        ["node", str(sdk / "dist/_worker/qoder-worker-runtime.obf.mjs"), "--version"],
        text=True,
        timeout=15,
        env={
            key: value
            for key, value in os.environ.items()
            if key != "QODER_PERSONAL_ACCESS_TOKEN"
        },
    ).strip()
    == "1.1.78"
)
token = os.environ.get("QODER_PERSONAL_ACCESS_TOKEN")
assert token, "native initialization requires a Qoder test PAT"
evidence = args.output.resolve()
evidence.mkdir(mode=0o700, parents=True, exist_ok=True)
evidence.chmod(0o700)
own_children()
with tempfile.TemporaryDirectory(prefix="aw-sdk-direct-") as temporary:
    case = Path(temporary)
    for name in ["work", "home", "runtime", "tmp"]:
        (case / name).mkdir(mode=0o700)
    (case / "work/node_modules").symlink_to(
        args.npm.resolve() / "node_modules", target_is_directory=True
    )
    shutil.copyfile(fixture / "native-start.mjs", case / "work/app.mjs")
    shutil.copyfile(fixture / "native-direct-worker.mjs", case / "work/worker.mjs")
    config = case / "aw.json"
    save(
        config,
        {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "sdk-native-start"},
            "spec": {
                "daemon": {
                    "startup": "external",
                    "endpoint": "auto",
                    "state_dir": str(case / "state"),
                },
                "execution": {
                    "guarantee": "native_hook",
                    "default_event_budget_ms": 5000,
                },
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "qoder-sdk",
                        "argv": [shutil.which("node"), str(case / "work/app.mjs")],
                    }
                },
                "providers": {},
                "events": {},
            },
        },
    )
    environment = {
        "PATH": os.environ["PATH"],
        "HOME": str(case / "home"),
        "LANG": "C.UTF-8",
        "QODER_CONFIG_DIR": str(case / "home/.qoder"),
        "XDG_CONFIG_HOME": str(case / "home/.config"),
        "XDG_RUNTIME_DIR": str(case / "runtime"),
        "TMPDIR": str(case / "tmp"),
        "QODER_TEST_ROOT": str(case),
        "AW_TEST_EXECUTABLE": str(args.aw.resolve()),
        "AW_TEST_CONFIG": str(config),
        "AW_TEST_WORKER": str(case / "work/worker.mjs"),
    }
    for key in (
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
    ):
        if os.environ.get(key):
            environment[key] = os.environ[key]
    processes = Processes(case, evidence, environment)
    try:
        processes.start(
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
        await_condition(
            lambda: (case / "state/aw.sock").exists(), 10, "AW service startup"
        )
        app = processes.start(
            "native-app",
            [shutil.which("node"), str(case / "work/app.mjs")],
            extra_env={"QODER_PERSONAL_ACCESS_TOKEN": token},
        )
        assert (
            processes.wait(app, 150) == 0
        ), "direct/prewarm native application failed; inspect native-app.log"
        status = json.loads(
            subprocess.check_output(
                [str(args.aw.resolve()), "status", "--config", str(config)],
                cwd=case / "work",
                env=environment,
                text=True,
                timeout=10,
            )
        )["result"]
        assert (
            status["bindings"] == status["events"] == 0 and status["observations"] == []
        )
        assert not list((case / "state").glob("native-*"))
        result = json.loads((case / "results.json").read_text())
        result["cleanup"] = {
            "bindings": 0,
            "events": 0,
            "observations": 0,
            "native_assets": 0,
        }
        save(evidence / "result.json", result)
    finally:
        processes.close()
        reaped = reap_children()
        for log in evidence.iterdir():
            if log.is_file() and log.suffix in {".log", ".json", ".jsonl"}:
                log.write_text(
                    log.read_text(errors="replace").replace(token, "[REDACTED]")
                )
        save(
            evidence / "cleanup.json",
            {
                "processes_stopped": all(
                    item[2].get("pid_absent") for item in processes.items
                ),
                "reaped": reaped,
            },
        )
assert not case.exists()
