"""Exercise cooperative control with a real in-process Hermes Gateway API."""

import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid


async def serve(root):
    from aw_sdk.hermes import listen
    from gateway.run import GatewayRunner
    from gateway.run_api import start_gateway_api, stop_gateway_api
    from gateway.session_authority import initialize_session_authority
    from hermes_cli import web_server
    from hermes_cli.dashboard_auth.ws_tickets import mint_ticket

    runner = GatewayRunner()
    await initialize_session_authority(runner, profile_id=os.environ["HERMES_HOME"],
                                       instance_id="aw-sdk-fixture")
    control = listen(runner, config_file=str(root / "aw.json"),
                     descriptor_file=str(root / "descriptor.json"), attach_timeout_ms=15000)
    api = await start_gateway_api(runner)
    try:
        web_server.app.state.auth_required = True
        ticket = mint_ticket(user_id="aw-sdk-fixture", provider="fixture")
        print(f"ws://127.0.0.1:{api.socket.getsockname()[1]}/api/ws?ticket={ticket}", flush=True)
        await asyncio.to_thread(sys.stdin.readline)
    finally:
        await stop_gateway_api(api)
        await control.close()


def test_gateway(aw, env, root, work, config, output, requests, model_url):
    import websockets

    def cli(*args):
        return subprocess.check_output([aw, *args], cwd=work, env=env, timeout=45)

    (root / "descriptor.json").write_bytes(cli("config", "descriptor", "--config", config,
        "--agent", "hermes", "--profile", "hermes-installed-native-hooks"))

    async def exercise():
        worker = await asyncio.create_subprocess_exec(
            sys.executable, str(Path(__file__).resolve()), str(root),
            cwd=work, env=env, stdin=-1, stdout=-1, stderr=log)
        try:
            while line := await worker.stdout.readline():
                if line.startswith(b"ws://"):
                    break
            else:
                raise RuntimeError("SDK worker exited before publishing its Gateway")
            async with websockets.connect(line.decode().strip(), proxy=None) as ws:
                async def rpc(method, **params):
                    request = str(uuid.uuid4())
                    await ws.send(json.dumps(dict(jsonrpc="2.0", id=request, method=method,
                                                  params=params)))
                    while True:
                        row = json.loads(await ws.recv())
                        if row.get("id") == request:
                            return row["result"]

                async def submit():
                    session = await rpc("session.create", request_id=str(uuid.uuid4()),
                        source="cli", cwd=str(work), model="aw-gateway-fixture", provider="custom",
                        base_url=model_url, api_key="fixture", toolsets=["terminal"], ignore_rules=True)
                    await rpc("prompt.submit", session_id=session["session_id"],
                              input_id=str(uuid.uuid4()), text="Use the terminal tool")

                async def wait_for(condition):
                    async with asyncio.timeout(60):
                        while not condition():
                            await asyncio.sleep(.05)

                marker = work / "denied-marker"
                await submit()
                await wait_for(lambda: any("AW result withheld" in str(group) for group in requests))
                assert not marker.exists() and not (root / "provider.jsonl").exists()
                assert not (root / "state/aw.sock").exists()
                prior = len(requests)
                await submit()
                await wait_for(lambda: len(requests) > prior)
                assert not marker.exists() and not (root / "provider.jsonl").exists()
                reply = await asyncio.to_thread(cli, "connect", "--config", config,
                    "--sdk-pid", str(worker.pid), "--timeout-ms", "30000")
                assert json.loads(reply)["result"]["state"] == "completed"
                await wait_for(lambda: any("MODEL_VISIBLE_REPLACEMENT" in str(group) for group in requests))
                assert marker.read_text() == "HERMES_TOOL_OK"
                prior = len(requests)
                (root / "fault").touch()
                await submit()
                await wait_for(lambda: any("AW result withheld" in str(group) for group in requests[prior:]))
                assert marker.read_text() == "HERMES_TOOL_OK" * 2
                assert "HERMES_TOOL_OK" not in json.dumps(requests)
        finally:
            (output / "model-requests.json").write_text(json.dumps(requests))
            if (root / "provider.jsonl").exists():
                (output / "sdk-events.jsonl").write_bytes((root / "provider.jsonl").read_bytes())
            worker.stdin.close()
            try:
                await asyncio.wait_for(worker.wait(), 25)
            except TimeoutError:
                worker.kill()
                await worker.wait()
                raise
        assert worker.returncode == 0
    with (output / "sdk-worker.log").open("w") as log:
        asyncio.run(asyncio.wait_for(exercise(), 180))
    print("SDK Gateway passed first-tool admission, result replacement and withheld delivery")


if __name__ == "__main__":
    asyncio.run(serve(Path(sys.argv[1])))
