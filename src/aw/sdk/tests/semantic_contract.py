"""Run installed, framework-free consumers against the shared semantic protocol."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from direct_contract import exchange


def run(binary: str, command: list[str], root: Path, environment: dict[str, str]) -> None:
    for empty in (False, True):
        case = root / ("semantic-empty" if empty else "semantic-configured")
        case.mkdir(mode=0o700)
        with tempfile.TemporaryDirectory(prefix="aws-", dir="/tmp") as state:
            run_case(
                binary,
                command,
                case,
                dict(environment, AW_SEMANTIC_EMPTY=str(int(empty))),
                empty,
                Path(state) / "s",
            )


def run_case(
    binary: str,
    command: list[str],
    root: Path,
    environment: dict[str, str],
    empty: bool,
    state: Path,
) -> None:
    state.mkdir(mode=0o700)
    provider = root / "policy.py"
    provider.write_text("""import json, sys
from pathlib import Path
r = json.load(sys.stdin)
out = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    out['operations'] = [{'name':'check','events':['input.submit','tool.before','turn.stop'],'effects':['observe','block']}, {'name':'record','events':['tool.after','session.start','session.end'],'effects':['observe']}]
elif r['method'] == 'invoke':
    event = r['event']
    with Path(r['config']['calls']).open('a') as f: f.write(event['name']+'\\n')
    deny = event.get('input', {}).get('text') == 'deny' or event.get('tool', {}).get('input', {}).get('command') == 'deny'
    deny = deny or (event['name'] == 'turn.stop' and event['native']['reason'] == 'timeout')
    out['input_digest'] = r['input_digest']
    out['effects'] = [{'type':'block','reason_code':'policy_denied'}] if deny else []
print(json.dumps(out))
""")
    calls = root / "calls"
    observations = {"tool.after", "session.start", "session.end"}
    config = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "semantic-sdk"},
        "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 3000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"app": {"adapter": "sdk", "argv": ["/bin/true"]}},
            "providers": {
                "policy": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(provider)],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 4096,
                    "config": {"calls": str(calls)},
                }
            },
            "events": {
                event: {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "check",
                            "provider": "policy",
                            "operation": "record" if event in observations else "check",
                            "effects": (
                                ["observe"] if event in observations else ["observe", "block"]
                            ),
                            "on_error": "report" if event in observations else "block",
                        }
                    ],
                }
                for event in ["session.start", "input.submit", "tool.before", "tool.after", "turn.stop", "session.end"]
            },
        },
    }
    if empty:
        for event in config["spec"]["events"].values():
            event["steps"] = []
    path = root / "semantic.json"
    path.write_text(json.dumps(config))
    endpoint = state / "aw.sock"
    with (root / "semantic.log").open("wb") as log:
        service = subprocess.Popen(
            [binary, "service", "serve", "--config", str(path), "--state-dir", str(state)],
            stdout=log,
            stderr=log,
        )
        try:
            deadline = time.monotonic() + 10
            while not endpoint.exists():
                if service.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError((root / "semantic.log").read_text())
                time.sleep(0.01)
            result = subprocess.run(
                [*command, str(endpoint)],
                cwd=root,
                env=environment,
                text=True,
                capture_output=True,
                timeout=30,
            )
            assert result.returncode == 0, result.stdout + result.stderr
            status = exchange(endpoint, None, {"method": "status"})
            assert status["result"]["bindings"] == 0, status
            assert (calls.read_text().splitlines() if calls.exists() else []) == (
                []
                if empty
                else [
                    "session.start",
                    "input.submit",
                    "tool.before",
                    "tool.before",
                    "tool.after",
                    "turn.stop",
                    "session.end",
                ]
            )
            if not empty:
                reasons = {}
                for journal in state.rglob("*.jsonl"):
                    first = journal.read_text().splitlines()[0]
                    event = json.loads(first)["record"].get("plan", {}).get("event", {})
                    if event.get("name") in {"turn.stop", "session.end"}:
                        reasons[event["name"]] = event["reason"]
                assert reasons == {"turn.stop": "timeout", "session.end": "cancelled"}, reasons
        finally:
            service.terminate()
            try:
                service.wait(timeout=5)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait(timeout=5)


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    for name in ("typescript", "python"):
        subprocess.run(
            [
                sys.executable,
                str(Path(__file__).with_name(name + "_package.py")),
                str(Path(args.bin) / "aw"),
            ],
            check=True,
            timeout=240,
        )
    (output / "result.json").write_text(
        json.dumps({"status": "passed", "packages": 2, "protocol": "aw-hook-source/v1",
                    "lifecycle": ["session.start", "turn.stop", "session.end"]})
    )


if __name__ == "__main__":
    main()
