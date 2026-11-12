"""Pinned Codex app-server stdio acceptance with one AW policy and a local model."""

import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import tempfile
import time

from codex_model import Model
from test_codex_native import document, run


class Rpc:
    def __init__(self, child):
        self.child = child
        self.buffer = b""
        self.pending = []

    def send(self, frame):
        self.child.stdin.write(json.dumps(frame).encode() + b"\n")
        self.child.stdin.flush()

    def receive(self, predicate, timeout=45):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for index, frame in enumerate(self.pending):
                if predicate(frame):
                    return self.pending.pop(index)
            readable, _, _ = select.select([self.child.stdout], [], [], 0.2)
            if not readable:
                continue
            data = os.read(self.child.stdout.fileno(), 65536)
            if not data:
                raise RuntimeError(f"app-server closed: {self.child.poll()}")
            self.buffer += data
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                if line:
                    self.pending.append(json.loads(line))
        raise TimeoutError("app-server response timeout")

    def call(self, request_id, method, params):
        self.send({"id": request_id, "method": method, "params": params})
        frame = self.receive(lambda item: item.get("id") == request_id)
        if "error" in frame:
            raise RuntimeError(f"{method}: {frame['error']}")
        return frame["result"]


def start_child(argv, workspace, environment, stderr_path):
    with stderr_path.open("wb") as stderr:
        return subprocess.Popen(argv, cwd=workspace, env=environment, start_new_session=True,
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr)


def stop_child(child, timeout=5):
    try:
        try:
            child.stdin.close()
        except BrokenPipeError:
            pass
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                pass
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=timeout)
        finally:
            child.stdout.close()


def initialize(rpc):
    rpc.call(0, "initialize", {"clientInfo": {"name": "aw_native_test", "version": "0.1"},
                               "capabilities": {"experimentalApi": True}})
    rpc.send({"method": "initialized"})


def trust_hooks(codex, workspace, environment, home, output):
    child = start_child([str(codex), "app-server"], workspace, environment,
                        output / "inventory-stderr.log")
    try:
        rpc = Rpc(child)
        initialize(rpc)
        rows = rpc.call(1, "hooks/list", {"cwds": [str(workspace)]})["data"]
        hooks = rows[0]["hooks"]
        assert len(hooks) == 3 and not rows[0]["errors"], rows
        with (home / "config.toml").open("a") as config:
            for hook in hooks:
                config.write(f'\n[hooks.state.{json.dumps(hook["key"])}]\n')
                config.write(f'trusted_hash = {json.dumps(hook["currentHash"])}\n')
    finally:
        stop_child(child)


def turn(rpc, request_id, prompt):
    thread = rpc.call(request_id, "thread/start", {"ephemeral": True})["thread"]["id"]
    rpc.call(request_id + 1, "turn/start", {"threadId": thread,
                                            "input": [{"type": "text", "text": prompt}]})
    done = rpc.receive(lambda frame: frame.get("method") == "turn/completed"
                       and frame.get("params", {}).get("threadId") == thread, timeout=60)
    assert done["params"]["turn"]["status"] == "completed", done


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.aw = args.aw.resolve()
    args.codex = args.codex.absolute()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True)
    version = subprocess.check_output([args.codex, "--version"], text=True, timeout=5).strip()
    assert version == f"codex-cli {args.expected_version}", version
    with tempfile.TemporaryDirectory(prefix="aw-codex-app-") as temporary, ExitStack() as cleanup:
        root = Path(temporary)
        home, workspace, state = root / "native", root / "work", root / "state"
        home.mkdir(mode=0o700)
        workspace.mkdir()
        model = Model(args.output, "replace")
        cleanup.callback(model.close)
        environment = {"PATH": os.environ["PATH"], "HOME": str(root),
                       "CODEX_HOME": str(home), "NO_PROXY": "127.0.0.1,localhost", "TERM": "dumb"}
        native = ('model="gpt-5.4"\nmodel_provider="fixture"\n'
                  'model_reasoning_effort="low"\napproval_policy="never"\n'
                  'sandbox_mode="danger-full-access"\n'
                  '[model_providers.fixture]\nname="fixture"\nwire_api="responses"\n'
                  f'base_url="http://127.0.0.1:{model.server.server_port}/v1"\n'
                  'requires_openai_auth=false\nsupports_websockets=false\n'
                  '[features]\ncode_mode=false\n')
        (home / "config.toml").write_text(native)
        policy = document(args.aw, args.codex, state, "replace", args.output)
        policy["spec"]["agents"]["codex"]["argv"] = [str(args.codex), "app-server"]
        policy["spec"]["events"]["tool.before"]["required"] = True
        config = root / "aw.json"
        config.write_text(json.dumps(policy))
        run([str(args.aw), "validate", "--config", str(config)], workspace, environment, args.output)
        run([str(args.aw), "install", "--config", str(config), "--agent", "codex"],
            workspace, environment, args.output)
        trust_hooks(args.codex, workspace, environment, home, args.output)
        child = start_child([str(args.aw), "run", "--config", str(config), "--agent", "codex"],
                            workspace, environment, args.output / "aw-stderr.log")
        try:
            rpc = Rpc(child)
            initialize(rpc)
            if args.expected_version == "0.166.0":
                status = rpc.call(20, "remoteControl/status/read", {})
                assert status["status"] == "disabled", status
            rows = rpc.call(1, "hooks/list", {"cwds": [str(workspace)]})["data"]
            assert all(h["trustStatus"] == "trusted" for h in rows[0]["hooks"]), rows
            turn(rpc, 2, "Run the fixture tool exactly once.")
            assert model.outputs == ["AW_REDACTED_RESULT"], model.outputs
            model.mode = "native-rewrite-deny"
            model.calls = 0
            model.outputs = []
            turn(rpc, 4, "Run the fixture tool exactly once.")
            assert "Blocked by AW policy" in json.dumps(model.outputs), model.outputs
            assert not (workspace / "AW_MUST_NOT_EXIST").exists()
            requests_before = len(model.requests)
            turn(rpc, 6, "BLOCK_ME")
            assert len(model.requests) == requests_before, "blocked prompt reached model"
            events = [json.loads(line) for line in (args.output / "events.jsonl").read_text().splitlines()]
            assert any(event["operation"] == "record" for event in events), events
            assert any(event["operation"] == "scan" for event in events), events
            assert any(event["operation"] == "scan_prompt" for event in events), events
        finally:
            try:
                stop_child(child)
            finally:
                run([str(args.aw), "stop", "--config", str(config)], workspace, environment, args.output)
    (args.output / "result.json").write_text(json.dumps({
        "version": version, "entrypoint": "app-server stdio", "output_replaced": True,
        "code_blocked": True, "prompt_blocked": True,
        "cleaned": not root.exists()}, indent=2))
    print(f"{version} app-server stdio: output replaced, code and prompt blocked, cleaned")


if __name__ == "__main__":
    main()
