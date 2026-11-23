"""Drive the stock Cosh executable with a local model that calls its real shell tool."""

import json
import os
import shlex
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


def run_host(
    case: str, root: Path, work: Path, hook: Path, env: dict, audit: Path, cli: Path
) -> None:
    """Exercise the packaged sandbox hooks through a stock Cosh foreground run."""
    home = root / ("cosh-" + case)
    config = home / ".copilot-shell"
    config.mkdir(parents=True)
    trace = home / "pre.jsonl"
    marker = work / ("stock-" + case)
    command = "rm -rf cache; printf done > " + shlex.quote(str(marker))
    arguments = {"command": command}
    if case == "timeout":
        arguments.update(command=command + "; sleep 30", timeout_ms=4000)

    class Model(BaseHTTPRequestHandler):
        calls = 0

        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            Model.calls += 1
            if Model.calls == 1 or (case == "blocked" and Model.calls == 2):
                delta = {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": f"call-{Model.calls}",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": json.dumps(arguments),
                            },
                        }
                    ]
                }
                end = "tool_calls"
            else:
                delta, end = {"content": "Done."}, "stop"
            chunks = [
                {"choices": [{"index": 0, "delta": d, "finish_reason": f}]}
                for d, f in [(delta, None), ({}, end)]
            ]
            body = (
                "".join("data: " + json.dumps(c) + "\n\n" for c in chunks)
                + "data: [DONE]\n\n"
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    with HTTPServer(("127.0.0.1", 0), Model) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        text = (
            f'[ai]\nactive_provider="test"\n[ai.providers.test]\ntype="openai"\n'
            f'api_key="test"\nmodel="local"\nbase_url="http://127.0.0.1:{server.server_port}/v1"\n'
            "[hooks]\nenabled=true\n"
        )
        base = "python3 " + shlex.quote(str(hook))
        # Install the same event commands as the extension without unrelated scanners.
        events = json.loads((hook.parents[1] / "cosh-extension.json").read_text())[
            "hooks"
        ]
        for event, groups in events.items():
            for group in groups:
                for definition in group["hooks"]:
                    if not definition["command"].endswith("/sandbox-guard.py"):
                        continue
                    tool_command = base
                    if event == "PreToolUse":
                        tool_command += " | tee -a " + shlex.quote(str(trace))
                    text += f"[[hooks.{event}]]\ncommand={json.dumps(tool_command)}\n"
                    text += f'name={json.dumps(definition["name"])}\ntimeout={definition["timeout"]}\n'
                    if "matcher" in group:
                        text += f'matcher={json.dumps(group["matcher"])}\n'
        if case == "blocked":
            blocker = (
                """printf '%s' '{"decision":"block","reason":"another hook blocked"}'"""
            )
            text += f'[[hooks.PreToolUse]]\nname="external-blocker"\nmatcher="^shell$"\ncommand={json.dumps(blocker)}\n'
        (config / "config.toml").write_text(text)
        sentinel = config / "telemetry_disabled"
        sentinel.touch()
        with subprocess.Popen(
            [
                os.environ["ASC_COSH_CORE"],
                "--headless",
                "--workspace",
                str(work),
                "--approval-mode",
                "trust",
                "Call the shell tool once.",
            ],
            cwd=work,
            env=env
            | {
                "HOME": str(home),
                "COSH_STATES_DIR": str(home / "states"),
                "COSH_TELEMETRY_DISABLED_PATH": str(sentinel),
            },
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=home.joinpath("stderr").open("w"),
            text=True,
        ) as host:
            timer = threading.Timer(35, host.kill)
            timer.start()
            approvals = 0
            messages = []
            try:
                for line in host.stdout:
                    msg = json.loads(line)
                    messages.append(msg)
                    if msg.get("type") != "control_request":
                        continue
                    assert case in {"approved", "declined"}, msg
                    approvals += 1
                    if case == "approved":
                        time.sleep(3)  # Simulate approval delay.
                    response = (
                        {"behavior": "allow"}
                        if case == "approved"
                        else {"behavior": "deny", "message": "declined"}
                    )
                    host.stdin.write(
                        json.dumps(
                            {
                                "type": "control_response",
                                "response": {
                                    "subtype": "success",
                                    "request_id": msg["request_id"],
                                    "response": response,
                                },
                            }
                        )
                        + "\n"
                    )
                    host.stdin.flush()
                assert host.wait() == 0, (messages, (home / "stderr").read_text())
            finally:
                timer.cancel()
                server.shutdown()
        assert Model.calls >= 2 and approvals == int(
            case in {"approved", "declined"}
        ), messages
    assert trace.exists(), (case, messages, (home / "stderr").read_text())
    preparations = [json.loads(line) for line in trace.read_text().splitlines()]
    assert len(preparations) == (2 if case == "blocked" else 1), preparations
    decisions = []
    for pre in preparations:
        assert pre["decision"] == (
            "ask" if case in {"approved", "declined"} else "allow"
        ), pre
        command = pre["hookSpecificOutput"]["tool_input"]["command"]
        decisions.append(shlex.split(command)[-1])
    decision = decisions[0]

    def receipts(decision_id: str) -> list:
        return [
            x["details"]
            for line in audit.read_text().splitlines()
            if (x := json.loads(line)).get("details", {}).get("decision_id")
            == decision_id
        ]

    if case == "blocked":
        # The second prepare releases the first slot. No later shell request releases the last one.
        assert decisions[0] != decisions[1] and not receipts(decisions[1])
    rows = receipts(decision)
    assert len(rows) == 1, (case, rows, messages)
    item = rows[0]
    if case == "timeout":
        assert (
            marker.exists()
            and item["result"] is None
            and item["execution_outcome"] == "unknown"
        ), item
    else:
        started = case in {"allowed", "approved"}
        assert marker.exists() == started and item["result"]["started"] == started, (
            case,
            item,
            messages,
        )
    replay = subprocess.run(
        [str(cli), "sandbox", "hook", "execute", decision],
        env=env,
        capture_output=True,
        check=False,
    )
    assert replay.returncode != 0, case
    print(f"PASS stock Cosh {case}", flush=True)
