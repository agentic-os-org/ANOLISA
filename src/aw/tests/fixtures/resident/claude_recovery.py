"""A recording Messages gateway drives native and managed tool errors followed by valid calls."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from threading import Thread

from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import ClaudeAgentOptions, ResultMessage, tool


async def run(root, mode):
    directory = root / mode
    directory.mkdir()
    requests = []
    calls = []
    original = json.dumps({"rows": [{"id": n, "description": "search receipt"} for n in range(80)]}, indent=2)

    @tool("search_patch", "Return the search label", {"query": str})
    async def search(arguments):
        calls.append(dict(arguments))
        if arguments["query"] == "invalid-result":
            return {"content": [{"type": "text", "text": 123}]}
        return {"content": [{"type": "text", "text": original}]}

    class Gateway(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path.split("?")[0].endswith("count_tokens"):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{"input_tokens":1}')
                return
            requests.append(body)
            step = len(requests)
            name = "mcp__local__search_patch" if step in (2, 4, 5) else "Bash"
            done = step >= 6
            block = ({"type": "text", "text": ""} if done else
                     {"type": "tool_use", "id": f"native_{step}", "name": name, "input": {}})
            args = {"command": "echo executed >> effects; echo valid-shell-receipt"}
            if step in (2, 4, 5):
                args = {"query": "invalid-result" if step == 4 else "valid"}
            deltas = ([{"type": "text_delta", "text": "diagnosis complete"}] if done else
                      [{"type": "input_json_delta", "partial_json": json.dumps(args)}])
            if step in (1, 2):
                deltas.append({"type": "input_json_delta", "partial_json": " TRAILING"})
            events = [
                {"type": "message_start", "message": {"id": f"msg_{step}", "type": "message",
                 "role": "assistant", "model": body["model"], "content": [], "stop_reason": None,
                 "stop_sequence": None, "usage": {"input_tokens": 5, "output_tokens": 0}}},
                {"type": "content_block_start", "index": 0, "content_block": block},
                *({"type": "content_block_delta", "index": 0, "delta": delta} for delta in deltas),
                {"type": "content_block_stop", "index": 0},
                {"type": "message_delta", "delta": {"stop_reason": "end_turn" if done else "tool_use",
                 "stop_sequence": None}, "usage": {"output_tokens": 4}}, {"type": "message_stop"},
            ]
            data = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    with ThreadingHTTPServer(("127.0.0.1", 0), Gateway) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        endpoint = f"http://127.0.0.1:{server.server_port}"
        pid = os.getpid()
        try:
            client = create_client(ClaudeAgentOptions(
                cwd=str(directory), model="claude-sonnet-4-6", tools=["Bash"], max_turns=7,
                allowed_tools=["Bash", "mcp__local__search_patch"], setting_sources=[],
                mcp_servers={"local": create_sdk_mcp_server("local", tools=[search])},
                env={"ANTHROPIC_API_KEY": "fixture", "ANTHROPIC_BASE_URL": endpoint,
                     "AW_TEST_MODE": "allow", "AW_TEST_TENANT": "recovery",
                     "CLAUDE_CONFIG_DIR": str(directory / "profile")},
            ))
            async with client:
                process = client._transport.delegate._process
                cli_pid = process.pid
                assert (client._aw_relay is None) == (mode == "direct")
                await client.query("Run the diagnostic")
                replies = [m async for m in client.receive_response() if isinstance(m, ResultMessage)]
                assert len(replies) == 1 and replies[0].result == "diagnosis complete"
                await client.query("Continue the same diagnosis")
                followup = [m async for m in client.receive_response() if isinstance(m, ResultMessage)]
                assert len(followup) == 1 and followup[0].session_id == replies[0].session_id
                assert followup[0].result == "diagnosis complete"
                cli_after = client._transport.delegate._process
                worker_after = os.getpid()
                assert cli_after.returncode is None and cli_after.pid == cli_pid and worker_after == pid
                assert not client._aw_bridge.invocations.active and not client._aw_bridge.failure
            assert len(requests) == 7
            assert "valid-shell-receipt" in json.dumps(requests[-1]["messages"])
            results = []
            for request in requests[1:6]:
                content = [b for m in request["messages"] if isinstance(m["content"], list)
                           for b in m["content"] if b.get("type") == "tool_result"]
                result = content[-1]
                issued = [b for m in request["messages"] if isinstance(m["content"], list)
                          for b in m["content"] if b.get("type") == "tool_use"][-1]
                assert result["tool_use_id"] == issued["id"]
                results.append(result)
            assert results[0].get("is_error") is True
            assert results[1].get("is_error") is True and "query" in str(results[1]["content"])
            assert not results[2].get("is_error") and "valid-shell-receipt" in str(results[2]["content"])
            assert results[3].get("is_error") is True and "Invalid MCP tool result" in str(results[3]["content"])
            events = [json.loads(line)["event"] for line in (directory / "events.jsonl").read_text().splitlines()]
            after, = (event for event in events if event["name"] == "tool.after"
                      and event["native"].get("tool_input") == {"query": "invalid-result"})
            assert after["native"]["is_error"] is True
            assert after["tool"]["result"]["is_error"] is True
            assert not results[4].get("is_error") and "search receipt" in str(results[4]["content"])
            assert (directory / "effects").read_text().splitlines() == ["executed"]
            assert calls == [{"query": "invalid-result"}, {"query": "valid"}]
            compressed = results[4]["content"][0]["text"]
            assert "rows[80]" in compressed and len(compressed) < len(original)
            (root / "result.json").write_text(json.dumps({"tasks": 1, "passed": True,
                "transport": mode, "worker_pid": pid, "worker_pid_after": worker_after,
                "cli_pid": cli_pid, "cli_pid_after": cli_after.pid, "session_id": replies[0].session_id,
                "followup_session_id": followup[0].session_id, "model_requests": len(requests),
                "shell_executions": len((directory / "effects").read_text().splitlines()),
                "mcp_executions": len(calls), "original_bytes": len(original.encode()),
                "invalid_result_hook_error": after["native"]["is_error"],
                "gateway_bytes": len(compressed.encode()), "gateway_results": results}))
        finally:
            server.shutdown()
            thread.join(5)
