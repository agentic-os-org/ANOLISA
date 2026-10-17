"""Accept real SDK 2.0.6 + Tokenless CLI lifecycle through the AW launcher.

The sec-core scanner is deterministic; the sec-core Provider and native SDK
permission engine are real. Toolkit tools record execution without a shell.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from importlib.metadata import version
from pathlib import Path
from typing import Any, Iterator


@contextmanager
def process(command: list[str], cwd: Path, output: Path, name: str) -> Iterator[Any]:
    record = {"command": command, "cwd": str(cwd), "ports": [], "lifetime_seconds": 120}
    path = output / f"{name}-process.json"
    record["logs"] = [str(output / f"{name}.{stream}") for stream in ["stdout", "stderr"]]
    path.write_text(json.dumps(record))
    with Path(record["logs"][0]).open("wb") as stdout, Path(record["logs"][1]).open("wb") as stderr:
        child = subprocess.Popen(
            command, cwd=cwd, stdout=stdout, stderr=stderr, start_new_session=True
        )
        record.update(pid=child.pid, stop=f"kill -TERM -- -{child.pid}")
        path.write_text(json.dumps(record))
        try:
            yield child
        finally:
            # Reap the whole owned group even if a launcher exited before a child.
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=3)
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            record.update(exited=True, returncode=child.returncode)
            path.write_text(json.dumps(record))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--tokenless", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    aw = args.aw.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/agentscope/lifecycle.py"
    with tempfile.TemporaryDirectory(prefix="s-", dir=output) as temporary:
        root = Path(temporary)
        state = root / "state"
        assert len(os.fsencode(state / "aw.sock")) <= 107, "use a short output path"
        # A private scanner shim exercises the real sec-core bridge's public CLI.
        scanner = root / "scanner"
        scanner.write_text(
            f'#!{sys.executable}\nimport runpy, sys\nsys.argv.insert(1, "scan")\nrunpy.run_path({str(fixture)!r}, run_name="__main__")\n'
        )
        scanner.chmod(0o700)
        config: dict[str, Any] = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-lifecycle"},
            "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 15000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "agentscope",
                        "argv": [sys.executable, str(fixture), "worker"],
                    }
                },
                "providers": {},
                "events": {},
            },
        }
        providers = config["spec"]["providers"]
        for name, argv, protocol, settings in [
            (
                "security",
                [
                    str(aw.parent / "aw-provider-sec-core"),
                    "--cli",
                    str(scanner),
                    "--socket",
                    str(root / "scanner.sock"),
                ],
                "aw-provider/v1alpha1",
                {
                    "version": 1,
                    "mode": "block",
                    "tools": {"exec": {"language": "bash", "input_pointer": "/command"}},
                },
            ),
            (
                "tokenless",
                [
                    str(aw.parent / "aw-provider-tokenless"),
                    "--cli",
                    str(args.tokenless.resolve()),
                    "--data-dir",
                    str(root / "tokenless"),
                ],
                "aw-provider/v1alpha1",
                {"version": 1},
            ),
            ("observe", [sys.executable, str(fixture), "observe"], None, {}),
            ("custom", [sys.executable, str(fixture), "custom"], None, {}),
            ("late", [sys.executable, str(fixture), "late"], None, {}),
            ("native", [sys.executable, str(fixture), "native"], "native-hook/v1alpha1", {}),
        ]:
            providers[name] = {
                "transport": {"type": "stdio", "location": "agent", "argv": argv},
                "timeout_ms": 4000,
                "max_output_bytes": 65536,
                "config": settings,
            }
            if protocol:
                providers[name]["protocol"] = protocol
        config["spec"]["events"] = {
            "tool.before": {
                "enabled": True,
                "required": True,
                "steps": [
                    {
                        "id": "scan",
                        "provider": "security",
                        "operation": "scan_code",
                        "effects": ["observe", "block"],
                        "on_error": "block",
                    },
                    {
                        "id": "retry",
                        "provider": "tokenless",
                        "operation": "pre_tool",
                        "effects": ["block"],
                        "on_error": "report",
                    },
                ],
            },
            "tool.after": {
                "enabled": True,
                "required": True,
                "steps": [
                    {
                        "id": name,
                        "provider": name,
                        "on_error": "report",
                        **({"native": {}} if name == "native" else {"effects": ["observe"]}),
                    }
                    for name in ["observe", "custom", "native", "late"]
                ]
                + [
                    {
                        "id": "compress",
                        "provider": "tokenless",
                        "operation": "post_tool",
                        "effects": ["replace_result"],
                        "on_error": "report",
                    }
                ],
            },
        }
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        (output / "config.json").write_text(json.dumps(config, indent=2))
        os.environ.update(
            AW_TEST_ROOT=str(root), TOKENLESS_STATS_ENABLED="0", TOKENLESS_SLS_ENABLED="0"
        )
        with process(
            [str(aw), "serve", "--config", str(path), "--state-dir", str(state)],
            root,
            output,
            "serve",
        ) as service:
            deadline = time.monotonic() + 10
            while not (state / "aw.sock").exists():
                assert service.poll() is None, "service exited"
                assert time.monotonic() < deadline, "startup timed out"
                time.sleep(0.02)
            with process(
                [str(aw), "run", "--config", str(path), "--agent", "sdk"], root, output, "run"
            ) as child:
                child.wait(timeout=90)
                assert child.returncode == 0, (output / "run.stderr").read_text()

            def records(name: str) -> list[Any]:
                return [
                    json.loads(line) for line in (root / f"{name}.jsonl").read_text().splitlines()
                ]

            executed, permissions, scans = [
                records(name) for name in ["executed", "permissions", "scanned"]
            ]
            assert len(executed) == 5, executed
            assert all(
                item["tool"] != "exec" or item["command"] != "git status" for item in executed
            )
            retry = executed[0]
            assert "TOKENLESS_AGENT_ID=agentscope" in retry["command"]
            assert retry in permissions and retry["command"] in scans
            assert len(permissions) == 8 and len(scans) == 3, (permissions, scans)
            observed, native, late = records("observe"), records("native"), records("late")
            assert len(observed) == len(native) == len(late) == 7
            for before, user, after in zip(observed, native, late):
                assert before["native"] == user == after["native"]
            stderr = (output / "run.stderr").read_text()
            assert "native_correlation_mismatch" not in stderr, stderr
            for name in ["observe", "native", "late", "permissions", "scanned", "executed"]:
                (output / f"{name}.json").write_text(json.dumps(records(name), indent=2))
            results = json.loads((output / "run.stdout").read_text())
            # Every observer receives the original pretty text, even when the
            # final model context contains the compact replacement.
            for item in observed:
                call = item["native"]["tool_call"]["id"]
                if call == "api-success":
                    raw = item["tool"]["result"]["content"][0]["text"]
                    final = next(r for r in results if r["id"] == call)["output"][0]["text"]
                    assert len(final) < len(raw) and json.loads(final) == json.loads(raw)
            status = json.loads(
                subprocess.check_output(
                    [str(aw), "status", "--socket", str(state / "aw.sock")], timeout=5
                )
            )["result"]
            assert status["events"] == status["bindings"] == 0, status
            assert not list(state.glob("launch-*"))
            journal = [
                json.loads(line)
                for p in state.rglob("*.jsonl")
                for line in p.read_text().splitlines()
            ]
            assert all(r["record"].get("status") != "expired" for r in journal)
            (output / "journal.json").write_text(json.dumps(journal, indent=2))
            (output / "result.json").write_text(
                json.dumps(
                    {
                        "sdk": version("agentscope"),
                        "executed": len(executed),
                        "permissions": len(permissions),
                        "scans": len(scans),
                        "after": len(observed),
                        "status": status,
                    },
                    indent=2,
                )
            )
    print("Real AgentScope + Tokenless PreTool/PostTool acceptance passed")


if __name__ == "__main__":
    main()
