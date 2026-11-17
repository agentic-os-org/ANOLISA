"""Exercise interrupted events and control reserves with installed language SDKs."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

from direct_contract import exchange


def run(binary: str, command: list[str], root: Path, environment: dict[str, str]) -> None:
    state = root / "event-state"
    state.mkdir(mode=0o700)
    provider = root / "event-policy.py"
    provider.write_text("""import json, os, sys, time
from pathlib import Path
r = json.load(sys.stdin)
out = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    out['operations'] = [{'name':'check','events':['tool.before'],'effects':['observe','block']}]
elif r['method'] == 'invoke':
    with Path(r['config']['calls']).open('a') as f: f.write(r['event']['tool']['call_id']+'\\n')
    if r['event']['tool']['input'].get('slow'):
        Path(r['config']['pid']).write_text(str(os.getpid()))
        time.sleep(15)
    out['input_digest'] = r['input_digest']
    out['effects'] = []
print(json.dumps(out))
""")
    document = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "event-cleanup"},
        "spec": {
            "daemon": {
                "startup": "external",
                "endpoint": "auto",
                "state_dir": str(state),
                "limits": {"connections": 1},
            },
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 20000},
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
                    "timeout_ms": 18000,
                    "max_output_bytes": 4096,
                    "config": {"calls": str(root / "event-calls"), "pid": str(root / "event-pid")},
                }
            },
            "events": {
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": name,
                            "provider": "policy",
                            "operation": "check",
                            "effects": ["observe", "block"],
                            "on_error": "block",
                        }
                        for name in ("first", "second")
                    ],
                }
            },
        },
    }
    config = root / "events.json"
    config.write_text(json.dumps(document))
    endpoint = state / "aw.sock"
    with (root / "events.log").open("wb") as log:
        service = subprocess.Popen(
            [binary, "service", "serve", "--config", str(config), "--state-dir", str(state)],
            stdout=log,
            stderr=log,
        )
        try:
            expires = time.monotonic() + 10
            while not endpoint.exists():
                if service.poll() is not None or time.monotonic() > expires:
                    raise RuntimeError((root / "events.log").read_text())
                time.sleep(0.01)
            result = subprocess.run(
                [*command, str(endpoint), str(root)],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
            )
            assert result.returncode == 0, result.stdout + result.stderr
            assert exchange(endpoint, None, {"method": "status"})["result"]["bindings"] == 0
            assert (root / "event-calls").read_text().splitlines() == [
                "partial",
                "complete",
                "complete",
                "cancelled",
                "aborted",
                "fresh",
                "fresh",
                "scope-close",
                "next-task",
                "next-task",
            ]
        finally:
            service.terminate()
            try:
                service.wait(timeout=5)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait(timeout=5)
