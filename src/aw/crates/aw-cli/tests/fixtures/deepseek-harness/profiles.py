"""Published DSH profile startup and AW Hook readiness acceptance."""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path

from native import configuration


def run_profile(profile: str, bin_dir: Path, npm: Path, model: Path, output: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="awdsh-profile-") as temporary:
        state = Path(temporary)
        args = argparse.Namespace(bin=bin_dir, npm=npm)
        config = configuration(state, args, "submit-allow", model)
        argv = [str(npm / "node_modules/.bin/dsh"), "--profile", profile,
                "--patch", str(state / "fixture.patch.json")]
        if profile == "web":
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]
            argv += ["--host", "127.0.0.1", "--port", str(port), "--no-open"]
        config["spec"]["agents"]["dsh"]["argv"] = argv
        config_path = state / "aw.json"
        config_path.write_text(json.dumps(config))
        env = {
            **os.environ,
            "AW_DSH_TEST_ROOT": str(state),
            "AW_DSH_TEST_CASE": "submit-allow",
            "DSH_HOME": str(state / "home"),
            "DSH_AGENTS_HOME": str(state / "agents"),
            "DSH_TELEMETRY_DISABLED": "1",
            "TOKENLESS_STATS_ENABLED": "0",
            "TOKENLESS_SLS_ENABLED": "0",
            "PATH": str(state / "bin") + os.pathsep + os.environ["PATH"],
        }
        child = subprocess.Popen(
            [str(bin_dir / "aw"), "run", "--config", str(config_path), "--agent", "dsh"],
            cwd=state, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, start_new_session=True,
        )
        with (output / "processes.jsonl").open("a") as stream:
            stream.write(json.dumps({"profile": profile, "pid": child.pid, "pgid": child.pid,
                                     "timeout_seconds": 45, "stop": f"kill -TERM -- -{child.pid}"}) + "\n")
        try:
            deadline = time.monotonic() + 45
            ready = False
            while time.monotonic() < deadline and child.poll() is None:
                receipts = list((state / "state").glob("launch-*/deepseek-ready.json"))
                ready = len(receipts) == 1 and json.loads(receipts[0].read_text()).get("hooks") == 3
                if ready:
                    break
                time.sleep(0.1)
            assert ready, f"{profile} did not register AW Hooks"
            if profile == "web":
                listening = False
                while time.monotonic() < deadline and child.poll() is None:
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=1):
                            listening = True
                        break
                    except OSError:
                        time.sleep(0.1)
                assert listening, "Web did not listen after AW Hook readiness"
            assert child.poll() is None, f"{profile} exited after Hook readiness"
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
            try:
                _, stderr = child.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                _, stderr = child.communicate(timeout=5)
            if (state / "state/aw.sock").exists():
                stopped = subprocess.run(
                    [str(bin_dir / "aw"), "stop", "--config", str(config_path)],
                    cwd=state, env=env, capture_output=True, text=True, timeout=10,
                )
                assert stopped.returncode == 0, stopped.stderr
                deadline = time.monotonic() + 5
                while (state / "state/aw.sock").exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                assert not (state / "state/aw.sock").exists()
            if not ready:
                print(stderr[-3000:])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.bin, args.npm, args.output = [path.resolve() for path in (args.bin, args.npm, args.output)]
    args.output.mkdir(parents=True, exist_ok=True)
    model = args.npm / "aw-profile-model.mjs"
    shutil.copyfile(Path(__file__).with_name("model.mjs"), model)
    reports = []
    try:
        for profile in ("web", "sdk", "acp"):
            run_profile(profile, args.bin, args.npm, model, args.output)
            reports.append({"case": profile, "passed": True})
            print(f"{profile}: Hook ready", flush=True)
    finally:
        model.unlink(missing_ok=True)
        (args.output / "results.json").write_text(json.dumps(reports, indent=2) + "\n")


if __name__ == "__main__":
    main()
