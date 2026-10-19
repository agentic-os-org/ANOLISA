"""Pinned real dsh headless CLI, Providers and Tokenless with deterministic model/scanner/RTK."""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CASES = [
    "allow",
    "security-deny",
    "security-error",
    "native-deny",
    "native-bypass",
    "native-override",
    "compress",
    "tool-error",
    "parallel",
    "nested-deny",
    "retry-denied",
    "retry-allowed",
    "post-content",
    "post-value",
    "post-block",
    "finalizer",
    "cancel",
]


def rows(path: Path) -> list:
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def call(argv: list[str], cwd: Path, env: dict, *, cancel: bool = False) -> tuple[int, str, str]:
    with subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    ) as child:
        try:
            with (cwd / "processes.jsonl").open("a") as stream:
                stream.write(
                    json.dumps(
                        {
                            "command": argv,
                            "cwd": str(cwd),
                            "pid": child.pid,
                            "pgid": child.pid,
                            "ports": [],
                            "timeout_seconds": 60,
                            "stop": f"kill -TERM -- -{child.pid}",
                        }
                    )
                    + "\n"
                )
            if cancel:
                deadline = time.monotonic() + 40
                while not (cwd / "tool-started").exists() and child.poll() is None:
                    if time.monotonic() > deadline:
                        raise TimeoutError("headless tool did not start")
                    time.sleep(0.05)
                if (cwd / "tool-started").exists():
                    os.kill(child.pid, signal.SIGTERM)
            stdout, stderr = child.communicate(timeout=60)
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
    return child.returncode, stdout, stderr


def configuration(state: Path, args: argparse.Namespace, case: str, model: Path) -> dict:
    python = sys.executable
    bin_dir = state / "bin"
    bin_dir.mkdir()
    rtk = bin_dir / "rtk"
    rtk.write_text(
        "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'rtk 0.49.0'; elif [ \"$1\" = rewrite ]; then printf 'rtk %s\\n' \"$2\"; else exit 99; fi\n"
    )
    rtk.chmod(0o700)
    scanner = state / "scanner"
    scanner.write_text(
        f"""#!{python}
import json,sys,pathlib,os
command=sys.argv[sys.argv.index('--code')+1]
with (pathlib.Path(__file__).parent/'scanned.jsonl').open('a') as output: output.write(json.dumps(command)+'\\n')
mode=os.environ['AW_DSH_TEST_CASE']
if mode=='security-error': sys.exit(7)
denied=command=='forbidden' or mode=='native-override' or (mode=='retry-denied' and 'TOKENLESS_AGENT_ID=' in command)
print(json.dumps({{'ok':True,'verdict':'deny' if denied else 'pass'}}))
"""
    )
    scanner.chmod(0o700)
    wrapper = state / "tokenless-cli"
    wrapper.write_text(
        f"""#!{python}
import json,pathlib,subprocess,sys
raw=sys.stdin.buffer.read()
result=subprocess.run([{str(args.bin / 'tokenless')!r},*sys.argv[1:]],input=raw,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=20)
with (pathlib.Path(__file__).parent/'tokenless.jsonl').open('a') as output:
 output.write(json.dumps({{'request':json.loads(raw),'response':json.loads(result.stdout),'code':result.returncode}})+'\\n')
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
raise SystemExit(result.returncode)
"""
    )
    wrapper.chmod(0o700)
    patch = state / "fixture.patch.json"
    patch.write_text(
        json.dumps(
            [
                {"id": "llm-deepseek", "disabled": True},
                {"id": "session-title-llm", "disabled": True},
                {"insert": [{"id": "aw-native-fixture", "name": model.as_uri()}]},
            ]
        )
    )

    def provider(argv: list[str], config: dict) -> dict:
        return {
            "protocol": "aw-provider/v1alpha1",
            "config": config,
            "transport": {"type": "stdio", "location": "agent", "argv": argv},
            "timeout_ms": 4000,
            "max_output_bytes": 1048576,
        }

    tools = {
        name: {"language": "bash", "input_pointer": "/command"}
        for name in ("execute_shell_command", "aw_inner")
    }
    before = [
        {
            "id": "security",
            "provider": "security",
            "operation": "scan_code",
            "effects": ["observe", "block"],
            "on_error": "block",
        }
    ]
    if case.startswith("retry-"):
        before.append(
            {
                "id": "retry",
                "provider": "tokenless",
                "operation": "pre_tool",
                "effects": ["block"],
                "on_error": "report",
            }
        )
    return {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "deepseek-native"},
        "spec": {
            "daemon": {
                "startup": "on_demand",
                "endpoint": "auto",
                "state_dir": str(state / "state"),
            },
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 10000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {
                "dsh": {
                    "adapter": "deepseek-harness",
                    "argv": [
                        str(args.npm / "node_modules/.bin/dsh"),
                        "--profile",
                        "headless",
                        "--patch",
                        str(patch),
                        "--json",
                        "AW native acceptance",
                    ],
                }
            },
            "providers": {
                "security": provider(
                    [
                        str(args.bin / "aw-provider-sec-core"),
                        "--cli",
                        str(scanner),
                        "--socket",
                        str(state / "scanner.sock"),
                    ],
                    {"version": 1, "mode": "block", "tools": tools},
                ),
                "tokenless": provider(
                    [
                        str(args.bin / "aw-provider-tokenless"),
                        "--cli",
                        str(wrapper),
                        "--data-dir",
                        str(state / "tokenless"),
                    ],
                    {"version": 1},
                ),
            },
            "events": {
                "tool.before": {"enabled": True, "steps": before},
                "tool.after": {
                    "enabled": True,
                    "steps": [
                        {
                            "id": "compress",
                            "provider": "tokenless",
                            "operation": "post_tool",
                            "effects": ["replace_result"],
                            "on_error": "report",
                        }
                    ],
                },
            },
        },
    }


def verify(state: Path, case: str, stdout: str) -> None:
    results = rows(state / "results.jsonl")
    executed = rows(state / "executed.jsonl")
    scanned = rows(state / "scanned.jsonl")
    requests = [entry["request"] for entry in rows(state / "tokenless.jsonl")]
    if case == "parallel":
        assert (
            {row["id"] for row in results}
            == {row["id"] for row in executed}
            == {"call-0", "call-1"}
        )
        assert len({row["attribution"]["tool_use_id"] for row in requests}) == 2
    if case.startswith("retry-"):
        initial = [row for row in requests if row["operation"] == "pre_tool"]
        assert initial, "retry did not reach real Tokenless"
        if case == "retry-allowed":
            post = [
                row
                for row in requests
                if row["operation"] == "post_tool" and row["input"].get("status") == "success"
            ]
            assert (
                post
                and post[-1]["attribution"]["tool_use_id"]
                != initial[0]["attribution"]["tool_use_id"]
            )
    if case == "cancel":
        pid = int((state / "tool-started").read_text())
        assert not Path(f"/proc/{pid}").exists(), f"native process {pid} survived cancellation"
        assert "AW_NATIVE_DONE" not in stdout, "cancelled task completed normally"
        assert (state / "disposed").exists(), "cancelled plugin did not dispose"
        return
    assert "AW_NATIVE_DONE" in stdout and results, (case, stdout)
    denied = case in {
        "security-deny",
        "security-error",
        "native-deny",
        "native-bypass",
        "native-override",
    }
    assert len(executed) == (
        0 if denied or case == "retry-denied" else 2 if case == "parallel" else 1
    ), (case, executed)
    assert not (state / "inner-executed.jsonl").exists()
    if denied:
        assert all(row["result"]["isError"] for row in results), results
    elif case.startswith("retry-"):
        assert len(scanned) == 2 and scanned[0] == "git status", scanned
        assert "TOKENLESS_AGENT_ID=deepseek-harness" in scanned[1], scanned
        assert len(results) == 2 and results[0]["result"]["isError"]
        assert results[1]["result"]["isError"] == (case == "retry-denied")
        if executed:
            assert executed[0]["command"] == scanned[1]
    elif case == "post-value":
        assert results[0]["result"]["value"] == "native canonical value"
        assert results[0]["result"]["content"] == [
            {"type": "text", "text": "native canonical value"}
        ]
        assert not (
            state / "tokenless.jsonl"
        ).exists(), "unmaterialized native value reached compression"
    elif case == "post-block":
        assert results[0]["result"]["isError"]
        assert results[0]["result"]["content"] == [{"type": "text", "text": "native post denial"}]
    elif case == "tool-error":
        assert results[0]["result"]["isError"]
        assert "native tool failed" in results[0]["result"]["content"][0]["text"]
    else:
        successful = [row["result"] for row in results if not row["result"]["isError"]]
        assert successful, results
        for result in successful:
            assert result["meta"] == {"preserved": True}
            if case == "post-content":
                assert "native_policy" in result["content"][0]["text"]
                assert "native_policy" not in result["value"]
            if case == "finalizer":
                assert result["content"][1:] == [{"type": "text", "text": "native finalizer"}]
            assert len(result["content"][0]["text"]) < len(result["value"]), result
        models = rows(state / "model.jsonl")
        assert all(
            result["content"][0]["text"] in json.dumps(models, ensure_ascii=False)
            or json.dumps(result["content"][0]["text"])[1:-1] in json.dumps(models)
            for result in successful
        )
        if case == "nested-deny":
            assert any(row["nested"] and row["result"]["isError"] for row in results)
    assert (state / "disposed").exists(), "native plugin teardown did not run"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.bin, args.npm, args.output = [path.resolve() for path in (args.bin, args.npm, args.output)]
    args.output.mkdir(parents=True, exist_ok=True)
    model = args.npm / "aw-model.mjs"
    shutil.copyfile(Path(__file__).with_name("model.mjs"), model)
    reports = []
    try:
        for case in CASES:
            with tempfile.TemporaryDirectory(prefix="awdsh-") as temporary:
                state = Path(temporary)
                config = state / "aw.json"
                config.write_text(json.dumps(configuration(state, args, case, model)))
                env = {
                    **os.environ,
                    "AW_DSH_TEST_ROOT": str(state),
                    "AW_DSH_TEST_CASE": case,
                    "DSH_HOME": str(state / "home"),
                    "DSH_AGENTS_HOME": str(state / "agents"),
                    "DSH_TELEMETRY_DISABLED": "1",
                    "TOKENLESS_STATS_ENABLED": "0",
                    "TOKENLESS_SLS_ENABLED": "0",
                    "PATH": str(state / "bin") + os.pathsep + os.environ["PATH"],
                }
                try:
                    code, out, err = call(
                        [str(args.bin / "aw"), "run", "--config", str(config), "--agent", "dsh"],
                        state,
                        env,
                        cancel=case == "cancel",
                    )
                    (state / "stdout.log").write_text(out)
                    (state / "stderr.log").write_text(err)
                    # The native runner may settle its turn while the signal shutdown
                    # drains; AW forwards its zero or signal exit code unchanged.
                    assert code in ({0, 143} if case == "cancel" else {0}), (case, code, err, out)
                    verify(state, case, out)
                    code, out, err = call(
                        [str(args.bin / "aw"), "status", "--config", str(config)], state, env
                    )
                    assert code == 0, err
                    status = json.loads(out)["result"]
                    assert status["bindings"] == status["events"] == 0, status
                    assert not list((state / "state").glob("launch-*"))
                    reports.append({"case": case, "passed": True})
                finally:
                    if (state / "state/aw.sock").exists():
                        code, out, err = call(
                            [str(args.bin / "aw"), "stop", "--config", str(config)], state, env
                        )
                        assert code == 0, err
                        deadline = time.monotonic() + 5
                        while (state / "state/aw.sock").exists() and time.monotonic() < deadline:
                            time.sleep(0.02)
                        assert not (state / "state/aw.sock").exists()
                    evidence = args.output / case
                    evidence.mkdir()
                    for file in state.iterdir():
                        if file.is_file():
                            shutil.copyfile(file, evidence / file.name)
            print(f"{case}: passed", flush=True)
    finally:
        model.unlink(missing_ok=True)
        (args.output / "results.json").write_text(json.dumps(reports, indent=2) + "\n")


if __name__ == "__main__":
    main()
