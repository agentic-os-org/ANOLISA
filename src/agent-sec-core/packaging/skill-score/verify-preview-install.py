#!/usr/bin/env python3
"""Check the installed raw preview and Docker resource identity without inference."""

import argparse
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path


def run(*arguments: str) -> str:
    return subprocess.run(arguments, check=True, capture_output=True, text=True, timeout=60).stdout


def check_public_score() -> dict:
    """Exercise the installed dispatch without a daemon or provider credentials."""
    command = ["/usr/local/bin/agent-sec-cli", "skillsec", "score"]
    environment = os.environ.copy()
    environment["AGENT_SEC_DAEMON_SOCKET"] = "relative-invalid-socket"
    for name in ("ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"):
        environment.pop(name, None)
    checks = {}
    assert not Path("/usr/local/bin/agent-sec-skill-score").exists()
    checks["no_standalone_score_launcher"] = True
    for stage in ("plan", "run"):
        subprocess.run(
            [*command, stage, "--help"],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
            env=environment,
        )
        checks[f"score_{stage}_help"] = True
    invalid = subprocess.run(
        [*command, "run", "-"],
        input="{}",
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
        env=environment,
    )
    assert invalid.returncode == 2 and "invalid input:" in invalid.stderr, invalid.stderr
    checks["stdin_invalid_task_exit"] = invalid.returncode
    with tempfile.TemporaryDirectory(prefix="score-install-check-") as directory:
        skill = Path(directory) / "harmless skill"
        skill.mkdir()
        source = (
            "---\nname: harmless\ndescription: Harmless installation check\n---\nPrint hello.\n"
        )
        (skill / "SKILL.md").write_text(source)
        result = subprocess.run(
            [*command, "plan", str(skill)],
            check=False,
            capture_output=True,
            text=True,
            timeout=60,
            env=environment,
        )
        assert result.returncode == 1, result.stderr
        for expected in (
            "[skill-score] static scan",
            "[skill-score] semantic audit",
            "Bailian pay-as-you-go credentials require both",
        ):
            assert expected in result.stderr, result.stderr
        checks["public_plan_native_scan_without_daemon"] = True
        checks["plan_stops_before_model_dispatch"] = True
        checks["plan_source_sha256"] = hashlib.sha256(source.encode()).hexdigest()
    # Match SkillEval's process-group termination while the real Python entrypoint
    # waits for stdin; this does not claim cancellation during a model-backed run.
    with subprocess.Popen(
        [*command, "run", "-"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        start_new_session=True,
    ) as process:
        try:
            expected_exe = Path(
                "/usr/local/lib/anolisa/sec-core/skill-score/cli/bin/python3.11"
            ).resolve()
            deadline = time.monotonic() + 10
            waiting_for_stdin = False
            while process.poll() is None and time.monotonic() < deadline:
                if (
                    Path(f"/proc/{process.pid}/exe").resolve() == expected_exe
                    and Path(f"/proc/{process.pid}/wchan").read_text().strip() == "pipe_read"
                ):
                    waiting_for_stdin = True
                    break
                time.sleep(0.02)
            assert waiting_for_stdin, "installed score did not reach its stdin read"
            assert os.getpgid(process.pid) == process.pid
            os.killpg(process.pid, signal.SIGTERM)
            process.communicate(timeout=10)
            assert process.returncode == -signal.SIGTERM, process.returncode
            checks["exec_preserves_pid_and_process_group"] = True
            checks["stdin_wait_process_group_sigterm"] = True
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=10)
    return checks


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((args.release / "release-manifest.json").read_text())
    assert manifest["cli"] == "agent-sec-cli"
    assert manifest["command_prefix"] == ["skillsec", "score"]
    for artifact in manifest["artifacts"]:
        path = args.release / artifact["file"]
        with path.open("rb") as stream:
            assert hashlib.file_digest(stream, "sha256").hexdigest() == artifact["sha256"], path
        assert path.stat().st_size == artifact["bytes"], path
    contract = tomllib.loads(
        Path("/usr/local/share/anolisa/components/sec-core/component.toml").read_text()
    )
    assert contract["component"]["version"] == manifest["version"]
    checks = {"artifact_hashes": True, "installed_version": manifest["version"]}
    for name in ("agent-sec-cli", "agent-sec-daemon", "linux-sandbox"):
        with Path(f"/usr/local/bin/{name}").open("rb") as stream:
            assert stream.read(4) == b"\x7fELF", name
        run(f"/usr/local/bin/{name}", "--help")
        checks[name] = True
    run(
        sys.executable,
        "-I",
        "-c",
        'import importlib.util; assert importlib.util.find_spec("agent_sec_cli") is None',
    )
    checks["no_global_python_cli"] = True
    checks.update(check_public_score())
    prefix = "/usr/local/lib/anolisa/sec-core/skill-score"
    run(
        f"{prefix}/cli/bin/python3.11",
        "-I",
        "-c",
        'from agent_sec_cli import _native; import json; assert json.loads(_native.scan_prompt_json("A harmless test", mode="fast"))["degraded"] is False',
    )
    checks["native_scanner"] = True
    versions = json.loads(
        run(
            f"{prefix}/runtime/bin/python3.12",
            "-I",
            "-c",
            'import importlib.metadata,json,platform; from agent_sec_skill_score_runtime.worker import main; print(json.dumps([platform.python_version(),importlib.metadata.version("harbor")]))',
        )
    )
    assert versions == [manifest["python_worker"], manifest["harbor"]]
    checks["worker_versions"] = versions
    for image in manifest["images"]["image_details"]:
        assert (
            run("docker", "image", "inspect", "--format", "{{.Id}}", image["name"]).strip()
            == image["id"]
        )
    checks["loaded_image_ids"] = True
    checks["model_calls"] = 0
    args.output.write_text(
        json.dumps(
            {"status": "PASS", "source_commit": manifest["source_commit"], "checks": checks},
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
