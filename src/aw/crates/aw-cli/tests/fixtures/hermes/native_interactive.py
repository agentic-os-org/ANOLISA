"""Drive two real native CLI turns through one idle policy update."""

import json
import os
from pathlib import Path
import pty
import signal
import subprocess
import threading
import time


def verify(args, argv: list[str], env: dict, work: Path, config: dict, server) -> dict:
    candidate = work / "updated.json"
    candidate.write_text(json.dumps(config))
    master, slave = pty.openpty()
    log = (args.output / "interactive.log").open("wb")
    child = subprocess.Popen(argv, cwd=work,
        env={**env, "TERM": "xterm-256color", "PROMPT_TOOLKIT_NO_CPR": "1"},
        stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    def drain() -> None:
        try:
            while True:
                value = os.read(master, 65536)
                if not value:
                    break
                log.write(value)
                log.flush()
        except OSError:
            pass
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    busy_rejections = 0
    def update(label: str) -> None:
        nonlocal busy_rejections
        deadline = time.monotonic() + 20
        last = None
        while time.monotonic() < deadline:
            assert child.poll() is None, "native CLI exited before idle update"
            result = subprocess.run([str(args.aw), "instance", "update", "--config", str(work.parent / "aw.json"), "--policy", str(candidate),
                "--agent", "hermes"], cwd=work, env=env, capture_output=True, timeout=10, check=False)
            last = result
            if result.returncode == 0:
                (args.output / (label + ".json")).write_bytes(result.stdout)
                return
            assert b"busy" in result.stderr, result.stderr.decode()
            busy_rejections += 1
            server.release.set()
            time.sleep(0.1)
        raise AssertionError(last.stderr.decode() if last else "update did not run")
    try:
        deadline = time.monotonic() + 35
        skill = bool(args.skill_provider)
        while not (server.turns if skill else (work / "marker").exists()):
            assert child.poll() is None and time.monotonic() < deadline, "first native tool did not execute"
            time.sleep(0.05)
        assert server.deliveries == [True] if skill else (work / "marker").read_text() == "AW_ALLOW"
        update("idle-enable")
        assert busy_rejections >= 1, "active native turn must reject updates"
        observed = subprocess.run([str(args.aw), "status", "--config", str(work.parent / "aw.json")],
            cwd=work, env=env, capture_output=True, timeout=10, check=True)
        state = json.loads(observed.stdout)["result"]
        (args.output / "active-status.json").write_bytes(observed.stdout)
        assert len(state["observations"]) == 1, state
        observation = state["observations"][0]
        assert observation["coverage"] == "unknown" and observation["query_state"] == "unavailable", observation
        first_pid = child.pid
        (work / "marker").unlink(missing_ok=True)
        server.label = "DENY"
        first_turns = server.turns
        os.write(master, b"DENY")
        time.sleep(0.1)
        os.write(master, b"\r")
        deadline = time.monotonic() + 35
        while server.turns == first_turns:
            assert child.poll() is None and time.monotonic() < deadline, "second native model turn did not complete"
            time.sleep(0.05)
        assert server.deliveries == [True, False] if skill else not (work / "marker").exists(), "updated policy did not block the second native tool"
        observed = subprocess.run([str(args.aw), "status", "--config", str(work.parent / "aw.json")],
            cwd=work, env=env, capture_output=True, timeout=10, check=True)
        (args.output / "denied-status.json").write_bytes(observed.stdout)
        observation = json.loads(observed.stdout)["result"]["observations"][0]
        assert observation["callbacks"] > 0 and observation["coverage"] == "unknown", observation
        config["spec"]["events"]["tool.before"]["enabled"] = False
        candidate.write_text(json.dumps(config))
        update("idle-disable")
        if skill:
            server.label = "RESTORE"
            first_turns = server.turns
            os.write(master, b"Load the Skill again")
            time.sleep(0.1)
            os.write(master, b"\r")
            deadline = time.monotonic() + 35
            while server.turns == first_turns:
                assert child.poll() is None and time.monotonic() < deadline, "third native model turn did not complete"
                time.sleep(0.05)
            assert server.deliveries == [True, False, True], server.deliveries
            update("idle-restored")
        assert child.pid == first_pid and child.poll() is None
        os.write(master, b"/exit")
        time.sleep(0.1)
        os.write(master, b"\r")
        child.wait(timeout=40)
        assert child.returncode == 0
        return {"status": "passed", "same_native_process": True, "idle_policy_updates": 3 if skill else 2, "busy_rejections": busy_rejections,
                "allow_marker_present": not skill, "deny_marker_absent": not skill,
                "skill_deliveries": server.deliveries if skill else [],
                "observation_callbacks": observation["callbacks"], "observation_coverage": observation["coverage"]}
    finally:
        server.release.set()
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
        reader.join(timeout=2)
        os.close(master)
        log.close()
