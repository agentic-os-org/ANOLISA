"""End-to-end contract for framework-free language-level Hook Sources."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from direct_contract import exchange


def run(binary: str, command: list[str], root: Path, environment: dict[str, str]) -> None:
    case = root / "language-source"
    case.mkdir(mode=0o700)
    with tempfile.TemporaryDirectory(prefix="aws-", dir="/tmp") as state_dir:
        state = Path(state_dir) / "s"
        state.mkdir(mode=0o700)
        provider = case / "source_policy.py"
        provider.write_text(
            """import json, sys
from pathlib import Path
r = json.load(sys.stdin)
out = {'api_version': r['api_version'], 'request_id': r['request_id'], 'status': 'ok'}
if r['method'] == 'describe':
    out['operations'] = [
        {'name': 'check', 'events': ['input.submit', 'tool.before'], 'effects': ['observe', 'block']},
        {'name': 'record', 'events': ['tool.after'], 'effects': ['observe']},
    ]
elif r['method'] == 'invoke':
    event = r['event']
    entry = {'event': event['name']}
    if event['name'] == 'input.submit':
        entry['text'] = event['input']['text']
    else:
        entry['tool'] = event['tool']['name']
        entry['input'] = event['tool']['input']
        if event['name'] == 'tool.after':
            entry['result'] = event['tool']['result']
    with Path(r['config']['log']).open('a') as f:
        f.write(json.dumps(entry, sort_keys=True) + '\\n')
    deny = (
        event.get('input', {}).get('text') == 'deny'
        or event.get('tool', {}).get('input', {}).get('command') == 'deny'
    )
    out['input_digest'] = r['input_digest']
    out['effects'] = [{'type': 'block', 'reason_code': 'policy_denied'}] if deny else []
print(json.dumps(out))
"""
        )
        log_path = case / "events.jsonl"
        config = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "language-source-sdk"},
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
                        "config": {"log": str(log_path)},
                    }
                },
                "events": {
                    event: {
                        "enabled": True,
                        "required": True,
                        "steps": [
                            {
                                "id": "policy",
                                "provider": "policy",
                                "operation": "record" if event == "tool.after" else "check",
                                "effects": (
                                    ["observe"] if event == "tool.after" else ["observe", "block"]
                                ),
                                "on_error": "report" if event == "tool.after" else "block",
                            }
                        ],
                    }
                    for event in ("input.submit", "tool.before", "tool.after")
                },
            },
        }
        config_path = case / "source.json"
        config_path.write_text(json.dumps(config))
        endpoint = state / "aw.sock"
        with (case / "service.log").open("wb") as log:
            service = subprocess.Popen(
                [binary, "service", "serve", "--config", str(config_path), "--state-dir", str(state)],
                stdout=log,
                stderr=log,
            )
            try:
                deadline = time.monotonic() + 10
                while not endpoint.exists():
                    if service.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError((case / "service.log").read_text())
                    time.sleep(0.01)
                result = subprocess.run(
                    [*command, str(endpoint), str(case / "missing.sock"), str(Path(binary).resolve())],
                    cwd=case,
                    env=environment,
                    text=True,
                    capture_output=True,
                    timeout=30,
                )
                assert result.returncode == 0, result.stdout + result.stderr
                status = exchange(endpoint, None, {"method": "status"})
                assert status["result"]["bindings"] == 0, status
                observed = [json.loads(line) for line in log_path.read_text().splitlines()]
                expected = [
                    {"event": "tool.before", "input": {}, "tool": "pendingTool"},
                    *([{"event": "tool.before", "input": {}, "tool": "chunks"}] * 2),
                    {"event": "input.submit", "text": "draft"},
                    *[
                        entry
                        for _ in range(2)
                        for entry in (
                            {"event": "tool.before", "input": {"command": "draft"}, "tool": "readTool"},
                            {
                                "event": "tool.after",
                                "input": {"command": "draft"},
                                "result": ["draft"],
                                "tool": "readTool",
                            },
                        )
                    ],
                    {"event": "input.submit", "text": "deny"},
                    {"event": "input.submit", "text": "allow-then-deny-tool"},
                    {"event": "tool.before", "input": {"command": "deny"}, "tool": "shell"},
                    {"event": "input.submit", "text": "allow-all"},
                    {"event": "tool.before", "input": {"command": "echo"}, "tool": "shell"},
                    {
                        "event": "tool.after",
                        "input": {"command": "echo"},
                        "result": "ran:echo",
                        "tool": "shell",
                    },
                    {"event": "tool.before", "input": {"command": "boom"}, "tool": "shell"},
                    {"event": "tool.before", "input": {"mode": "early"}, "tool": "stream"},
                    {"event": "tool.before", "input": {"mode": "full"}, "tool": "stream"},
                    {
                        "event": "tool.after",
                        "input": {"mode": "full"},
                        "result": ["part-1", "part-2"],
                        "tool": "stream",
                    },
                    *[
                        entry
                        for _ in range(2)
                        for entry in (
                            {"event": "tool.before", "input": {"command": "held"}, "tool": "pausedTool"},
                            {
                                "event": "tool.after",
                                "input": {"command": "held"},
                                "result": "held",
                                "tool": "pausedTool",
                            },
                        )
                    ],
                    {"event": "tool.before", "input": {"command": "deny"}, "tool": "shell"},
                    {"event": "input.submit", "text": "drain-input"},
                ]
                if Path(command[0]).name == "node":
                    expected.append({"event": "tool.before", "input": {}, "tool": "pendingFactory"})
                assert observed == expected, observed
            finally:
                service.terminate()
                try:
                    service.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait(timeout=5)
