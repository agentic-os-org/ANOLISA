"""Keep pinned SDK transcript mirroring and resume behind the managed transport."""

import importlib.util
import json
import threading
from dataclasses import replace
from http.server import ThreadingHTTPServer

from aw_claude import create_client
from claude_agent_sdk import ClaudeAgentOptions, ResultMessage


class Store:
    def __init__(self):
        self.rows = {}
        self.loads = 0

    async def append(self, key, entries):
        self.rows.setdefault(json.dumps(key, sort_keys=True), []).extend(entries)

    async def load(self, key):
        self.loads += 1
        return self.rows.get(json.dumps(key, sort_keys=True))


async def run(root, source):
    directory = root / "store"
    directory.mkdir()
    spec = importlib.util.spec_from_file_location("store_model", source)
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    fixture.ROOT, fixture.MODE, fixture.BASH = directory, "bash-compress", True
    fixture.ORIGINAL = "store fixture result"
    server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    (directory / "endpoint.json").write_text(json.dumps({"port": server.server_port}))
    store = Store()
    options = ClaudeAgentOptions(
        cwd=str(directory),
        model="claude-sonnet-4-6",
        tools=["Bash"],
        allowed_tools=["Bash"],
        setting_sources=[],
        max_turns=4,
        session_store=store,
        session_store_flush="eager",
        env={
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture",
            "AW_TEST_MODE": "allow",
            "AW_TEST_TENANT": "store",
            "CLAUDE_CONFIG_DIR": str(directory / "profile"),
        },
    )
    session = None
    try:
        for attempt in range(2):
            client = create_client(options if attempt == 0 else replace(options, resume=session))
            async with client:
                evidence = root / f"store-process-{attempt}"
                evidence.mkdir()
                (evidence / "sdk-process.json").write_text(
                    json.dumps(
                        {
                            "pid": client._transport.delegate._process.pid,
                            "relay_port": client._aw_relay.server.sockets[0].getsockname()[1],
                        }
                    )
                )
                materialized = client._materialized.config_dir if client._materialized else None
                assert (materialized is not None) == (attempt == 1)
                if materialized:
                    (evidence / "materialized.json").write_text(
                        json.dumps({"path": str(materialized)})
                    )
                for _ in range(2):
                    await client.query("Run the fixture tool")
                    async for message in client.receive_response():
                        if isinstance(message, ResultMessage):
                            assert not message.is_error
                            assert session is None or session == message.session_id
                            session = message.session_id
            if materialized:
                assert not materialized.exists()
            assert store.rows and sum(map(len, store.rows.values())) > 0
        assert store.loads == 1
        assert len([row for row in fixture.REQUESTS if "messages" in row]) == 8
        (root / "result.json").write_text(
            json.dumps(
                {
                    "tasks": 2,
                    "executions": 4,
                    "passed": True,
                    "cases": ["store-mirror", "store-resume", "materialized-cleanup"],
                }
            )
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()
