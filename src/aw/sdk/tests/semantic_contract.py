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
    state = root / "semantic-state"
    state.mkdir(mode=0o700)
    provider = root / "policy.py"
    provider.write_text("""import json, sys
from pathlib import Path
r = json.load(sys.stdin)
out = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    out['operations'] = [{'name':'check','events':['input.submit','tool.before'],'effects':['observe','block']}, {'name':'record','events':['tool.after'],'effects':['observe']}]
elif r['method'] == 'invoke':
    event = r['event']
    with Path(r['config']['calls']).open('a') as f: f.write(event['name']+'\\n')
    deny = event.get('input', {}).get('text') == 'deny' or event.get('tool', {}).get('input', {}).get('command') == 'deny'
    out['input_digest'] = r['input_digest']
    out['effects'] = [{'type':'block','reason_code':'policy_denied'}] if deny else []
print(json.dumps(out))
""")
    calls = root / "calls"
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
                            "operation": "record" if event == "tool.after" else "check",
                            "effects": (
                                ["observe"] if event == "tool.after" else ["observe", "block"]
                            ),
                            "on_error": "report" if event == "tool.after" else "block",
                        }
                    ],
                }
                for event in ["input.submit", "tool.before", "tool.after"]
            },
        },
    }
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
            assert calls.read_text().splitlines() == [
                "input.submit",
                "tool.before",
                "tool.before",
                "tool.after",
            ]
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
        json.dumps({"status": "passed", "packages": 2, "protocol": "aw-hook-source/v1"})
    )


if __name__ == "__main__":
    main()
