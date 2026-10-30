"""Native App fixtures retain user plugin ownership and exercise live MCP APIs."""

from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

from runtime import await_condition


def request(port, route, body=None, method=None):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/{route}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "X-Agent-Id": "default"},
        method=method,
    )
    with urllib.request.urlopen(req, timeout=45) as response:
        return response.read(2 * 1024 * 1024)


def ready(port):
    try:
        request(port, "version")
        return True
    except (OSError, urllib.error.URLError):
        return False


def turn(test, root, evidence, port, identity, tool, arguments, *, processes=None):
    (root / "tool-call.json").write_text(json.dumps({"name": tool, "arguments": arguments}))
    (root / "tool-call-id.txt").write_text(f"native-{identity}")
    payload = {
        "session_id": f"native-{identity}", "user_id": "aw-test", "input": [
            {"role": "user", "content": [{"type": "text", "text": f"Use {tool} once then stop."}]},
        ],
    }
    if processes is None:
        events = request(port, "console/chat", payload)
    else:
        # Exercise native approval rather than bypassing its driver permission.
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(request, port, "console/chat", payload)
            try:
                deadline = time.monotonic() + 35
                while not future.done():
                    test.assertLess(time.monotonic(), deadline, "native turn deadline")
                    approvals = json.loads(request(port, "approval/list?session_id=" + payload["session_id"]))
                    for item in approvals["pending_approvals"]:
                        test.assertEqual(item["session_id"], payload["session_id"])
                        test.assertEqual(item["tool_name"], "driver:mcp:native-fixture")
                        approved = json.loads(request(port, "approval/approve", {
                            "request_id": item["request_id"], "session_id": item["session_id"], "scope": "exact",
                        }))
                        test.assertTrue(approved["success"])
                    time.sleep(0.1)
                events = future.result()
            except BaseException:
                processes.close()
                raise
    (evidence / f"turn-{identity}.log").write_bytes(events)
    (evidence / "model-requests.jsonl").write_bytes((root / "model-requests.jsonl").read_bytes())
    frames = [json.loads(line[6:]) for line in events.splitlines() if line.startswith(b"data: {")]
    completed = [frame for frame in frames if frame.get("object") == "response" and frame.get("status") == "completed"]
    test.assertEqual(len(completed), 1, events.decode())
    outputs = [message for message in completed[0]["output"] if message["type"] == "plugin_call_output"]
    test.assertEqual(len(outputs), 1)
    result = outputs[0]["content"][0]["data"]
    test.assertEqual(result["name"], tool)
    return result


def stage_user_configuration(root):
    config = root / "home/config.json"
    document = json.loads(config.read_text())
    document["future_native"] = {"nested": [True, {"reference": "${NATIVE_TEST_VALUE}"}]}
    document["plugins"] = {"aw-native": {"enabled": False}, "native-observer": {"enabled": True, "future": [1, 2]}}
    config.write_text(json.dumps(document, indent=4) + "\n")
    owned = root / "home/plugins/aw-native"
    owned.mkdir(parents=True)
    (owned / "user-data").write_bytes(b"user-owned, never replace\n")
    plugin = root / "home/plugins/native-observer"
    plugin.mkdir()
    (plugin / "plugin.json").write_text(json.dumps({
        "id": "native-observer", "name": "Native observer", "version": "1.0.0", "type": "general",
        "entry": {"backend": "plugin.py"}, "dependencies": [],
    }))
    (plugin / "plugin.py").write_text('''from pathlib import Path
from agentscope.middleware import MiddlewareBase

class Observer(MiddlewareBase):
    async def on_acting(self, agent, input_kwargs, next_handler):
        with Path("native-order.log").open("a") as output:
            output.write("before:" + input_kwargs["tool_call"].name + "\\n")
        async for item in next_handler(**input_kwargs):
            yield item
        with Path("native-order.log").open("a") as output:
            output.write("after:" + input_kwargs["tool_call"].name + "\\n")

class Plugin:
    def register(self, api):
        api.register_middleware(lambda ctx, config: Observer(), priority=50)

plugin = Plugin()
''')
    return {path: path.read_bytes() for path in (owned / "user-data", plugin / "plugin.py", plugin / "plugin.json")}


def mcp_server(root):
    path = root / "work/native_mcp_server.py"
    path.write_text('''import os
from pathlib import Path
from mcp.server.fastmcp import FastMCP
server = FastMCP("native-fixture")
@server.tool(description=os.environ["NATIVE_PREFIX"], annotations={"readOnlyHint": True})
def echo(value: str) -> str:
    with Path("native-mcp-calls.log").open("a") as output:
        output.write(value + "\\n")
    return os.environ["NATIVE_PREFIX"] + value
server.run()
''')
    return path


def dynamic_mcp(test, root, evidence, port, python, identity, processes):
    server = mcp_server(root)
    request(port, "mcp", {"client_key": "native-fixture", "client": {
        "name": "native-fixture", "transport": "stdio", "enabled": True,
        "command": str(python), "args": [str(server)], "cwd": str(root / "work"),
        "env": {"NATIVE_PREFIX": "first:"},
    }})
    try:
        request(port, "mcp/policy/native-fixture", {"default_effect": "allow"}, "PUT")
        def loaded(prefix):
            try:
                tools = json.loads(request(port, "mcp/tools/native-fixture"))
                return len(tools) == 1 and tools[0]["name"] == "echo" and tools[0]["description"] == prefix
            except urllib.error.HTTPError as error:
                if error.code not in (502, 503):
                    raise
                return False

        for index, prefix in enumerate(("first:", "updated:")):
            if index:
                request(port, "mcp/native-fixture", {"env": {"NATIVE_PREFIX": prefix}}, "PUT")
            await_condition(lambda: loaded(prefix), 15, "native MCP activation")
            result = turn(test, root, evidence, port, f"{identity}-mcp-{index}",
                          "native-fixture__echo", {"value": identity}, processes=processes)
            test.assertEqual(result["state"], "success")
            test.assertIn(prefix + identity, result["output"])
        test.assertEqual((root / "work/native-mcp-calls.log").read_text().splitlines()[-2:], [identity, identity])
        request(port, "mcp/policy/native-fixture", {"default_effect": "deny"}, "PUT")
        denied = turn(test, root, evidence, port, f"{identity}-mcp-denied",
                      "native-fixture__echo", {"value": "must-not-run"}, processes=processes)
        test.assertEqual(denied["state"], "error")
        test.assertEqual(json.loads(denied["output"])["type"], "driver_policy_denied")
        test.assertNotIn("must-not-run", (root / "work/native-mcp-calls.log").read_text())
    finally:
        request(port, "mcp/native-fixture", method="DELETE")
