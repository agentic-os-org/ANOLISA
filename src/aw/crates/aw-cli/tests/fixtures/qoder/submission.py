"""Verify native Qoder submission identity and failure policy before model dispatch."""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

AW = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(AW / "scripts"))
from native_cleanup import own_children, reap_children


PROVIDER = '''import json, sys
from pathlib import Path
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [{"name": name, "events": ["input.submit"], "effects": ["observe", "block"]} for name in ("deny", "fault")]
elif request["method"] == "invoke":
    with Path(__file__).with_suffix(".jsonl").open("a") as stream:
        stream.write(json.dumps(request) + "\\n")
    if request["operation"] == "fault":
        sys.exit(1)
    reply.update(input_digest=request["input_digest"], effects=[{"type": "block", "reason_code": "native_input_fixture"}])
print(json.dumps(reply))
'''


def exercise(binaries, native, output):
    if not os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"):
        raise RuntimeError("Qoder acceptance requires QODER_PERSONAL_ACCESS_TOKEN")
    cases = []
    with tempfile.TemporaryDirectory(prefix="aw-qoder-submit-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir(mode=0o700)
        environment = {**os.environ, "HOME": str(home)}
        controls = {key: value for key, value in environment.items()
                    if key != "QODER_PERSONAL_ACCESS_TOKEN"}
        aw = str(binaries / "aw")
        version = subprocess.check_output([str(native), "--version"], env=controls,
                                          text=True, timeout=10).strip()
        for scenario, operation, action in [("block", "deny", "block"),
                                            ("fault-block", "fault", "block"),
                                            ("fault-report", "fault", "report")]:
            case = root / scenario
            case.mkdir(mode=0o700)
            provider = case / "provider.py"
            provider.write_text(PROVIDER)
            steps = [{"id": "scan", "provider": "scan", "operation": operation,
                      "effects": ["observe", "block"], "on_error": action}]
            if action == "report":
                steps.append({**steps[0], "id": "guard", "operation": "deny", "on_error": "block"})
            document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
                        "metadata": {"name": "qoder-submit-native"}, "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(case / "state")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"qoder": {"adapter": "qoder", "argv": [str(native)], "qoder": {"sequential": True}}},
                "providers": {"scan": {"protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": [sys.executable, str(provider)]},
                    "timeout_ms": 4000, "max_output_bytes": 4096, "config": {}}},
                "events": {"input.submit": {"enabled": True, "steps": steps}}}}
            config = case / "aw.json"
            config.write_text(json.dumps(document))
            try:
                process = subprocess.run([aw, "run", "--config", str(config), "--agent", "qoder",
                    "--", "-p", "AW harmless input fixture", "--output-format", "json"],
                    cwd=case, env=environment, capture_output=True, text=True, timeout=60)
                assert process.returncode != 0, scenario
                (output / f"{scenario}.stdout").write_text(process.stdout)
                (output / f"{scenario}.stderr").write_text(process.stderr)
                result = json.loads(process.stdout)
                assert result["num_turns"] == result["duration_api_ms"] == 0, result
                assert result["errors"] == ["Agent execution blocked by hook"], result
                requests = [json.loads(line) for line in provider.with_suffix(".jsonl").read_text().splitlines()]
                assert len(requests) == len(steps), requests
                identities = set()
                for request in requests:
                    event = request["event"]
                    assert event["input"]["text"] == "AW harmless input fixture"
                    assert event["turn_id"] == event["native"]["request_set_id"]
                    identities.add((event["session_id"], event["turn_id"]))
                assert len(identities) == 1
                snapshot = json.loads(subprocess.check_output([aw, "status", "--config", str(config)],
                    env=controls, text=True, timeout=10))["result"]
                assert snapshot["bindings"] == snapshot["events"] == 0, snapshot
                cases.append({"case": scenario, "passed": True, "provider_calls": len(requests), "model_turns": 0})
            finally:
                for journal in (case / "state/journal").glob("*.jsonl"):
                    (output / f"{scenario}-{journal.name}").write_bytes(journal.read_bytes())
                if (case / "state/aw.sock").exists():
                    subprocess.run([aw, "stop", "--config", str(config)], env=controls,
                                   capture_output=True, check=True, timeout=15)
    (output / "results.json").write_text(json.dumps({"passed": True,
        "native_version": version, "runtime_removed": not root.exists(), "cases": cases}, indent=2) + "\n")


def interrupted(signum, frame):
    raise KeyboardInterrupt(f"interrupted by signal {signum}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--agent-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupted)
    own_children()
    try:
        exercise(args.bin.resolve(), args.agent_bin.resolve(), output)
    finally:
        reaped = reap_children()
        (output / "cleanup.json").write_text(json.dumps({"passed": True, "reaped": reaped}) + "\n")
