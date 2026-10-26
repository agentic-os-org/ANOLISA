"""Drive native Qoder and QwenPaw entrypoints, preserving their permission gates."""

from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import socket
import sys
import urllib.request

from runtime import await_condition, save


def configure_qwenpaw(case, agent_bin, environment, processes):
    environment.update(
        HOME=str(case / "home"),
        QWENPAW_WORKING_DIR=str(case / "home"),
        QWENPAW_SECRET_DIR=str(case / "home/secret"),
        QWENPAW_DISABLE_KEYRING="true",
        QWENPAW_AUTH_ENABLED="false",
    )
    (case / "home/secret").mkdir(mode=0o700)
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
    processes.start("model", [sys.executable, str(Path(__file__).with_name("model.py")), str(case)])
    await_condition(lambda: (case / "model-server.json").exists(), 5, "model startup")
    model_port = json.loads((case / "model-server.json").read_text())["port"]
    processes.items[-1][2]["ports"] = [model_port]
    processes.record()
    save(
        case / "home/config.json",
        {
            "agents": {
                "active_agent": "default",
                "profiles": {"default": {"id": "default", "workspace_dir": str(case / "work")}},
            },
            "plugins": {"aw-native": {"enabled": True}},
        },
    )
    save(
        case / "work/agent.json",
        {
            "id": "default",
            "name": "AW real sec-core test",
            "workspace_dir": str(case / "work"),
            "active_model": {"provider_id": "aw-fixture", "model": "aw-fixture"},
            "running": {
                "max_iters": 3,
                "auto_title_config": {"enabled": False},
                "llm_retry_enabled": False,
            },
            "channels": {"console": {"enabled": True}},
        },
    )
    save(
        case / "home/secret/providers.json",
        {
            "custom_providers": {
                "aw-fixture": {
                    "name": "AW local fixture",
                    "base_url": f"http://127.0.0.1:{model_port}/v1",
                    "api_key": "local-fixture-placeholder",
                    "chat_model": "OpenAIChatModel",
                    "models": [
                        {
                            "id": "aw-fixture",
                            "name": "AW fixture",
                            "supports_tool_calling": True,
                        }
                    ],
                }
            },
            "active_llm": {"provider_id": "aw-fixture", "model": "aw-fixture"},
        },
    )
    return [str(agent_bin), "app", "--host", "127.0.0.1", "--port", str(port)], [
        port,
        model_port,
    ]


def qoder_arguments(command):
    prompt = (
        "This is a local security-hook acceptance test. All paths are disposable test fixtures. "
        "Use Bash exactly once to run exactly this command: "
        + command
        + ". If denied, stop. Never retry, delegate, or use other tools. Then report the result."
    )
    return [
        "-m",
        "Performance",
        "--dangerously-skip-permissions",
        "--permission-mode=bypass_permissions",
        "--strict-mcp-config",
        "--mcp-config",
        '{"mcpServers":{}}',
        "--no-session-persistence",
        "--max-model-request-retries",
        "0",
        "--max-output-tokens",
        "256",
        "--tools",
        "Bash",
        "-o",
        "json",
        "-p",
        prompt,
    ]


def request_json(url, body=None):
    request = urllib.request.Request(
        url,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "X-Agent-Id": "default"},
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


def qwenpaw_turn(case, evidence, port, risk, processes, child):
    base = f"http://127.0.0.1:{port}/api"

    def chat():
        payload = {
            "session_id": "aw-sec-test",
            "user_id": "aw-test",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "Use the shell once then stop."}],
                }
            ],
        }
        request = urllib.request.Request(
            base + "/console/chat",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json", "X-Agent-Id": "default"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read(2 * 1024 * 1024)

    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(chat)
        try:
            if risk:
                pending = []

                def approval_ready():
                    if future.done():
                        future.result()
                        raise AssertionError("risk turn ended before native approval")
                    rows = request_json(base + "/approval/list?session_id=aw-sec-test")[
                        "pending_approvals"
                    ]
                    pending[:] = rows
                    return bool(rows)

                await_condition(approval_ready, 30, "native approval for the owned test fixture")
                assert len(pending) == 1
                item = pending[0]
                assert item["session_id"] == "aw-sec-test" and item["tool_name"] == "Bash"
                assert item["exact_target"] == (case / "tool-command.txt").read_text()
                response = request_json(
                    base + "/approval/approve",
                    {
                        "request_id": item["request_id"],
                        "session_id": "aw-sec-test",
                        "scope": "exact",
                    },
                )
                assert response["success"] is True
                save(
                    evidence / "native-approval.json",
                    {"pending": item, "response": response},
                )
            (evidence / "console-events.log").write_bytes(future.result(timeout=65))
        except BaseException:
            processes.stop(child)
            raise
