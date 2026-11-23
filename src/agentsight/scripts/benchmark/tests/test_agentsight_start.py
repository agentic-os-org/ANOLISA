"""Exercise the supervisor template with a controlled worker binary.

Lives in this directory because scripts/benchmark/tests is the pytest suite
that covers the agentsight shell entry points.
"""

from __future__ import annotations

import os
import signal
import subprocess
import time
from pathlib import Path

RENDER_SCRIPT = Path(__file__).resolve().parents[3] / "packaging/systemd/render.py"

# Delivers a signal after the supervisor installed its traps but before it can
# enter wait -n: the DEBUG trap fires right before the start_workers call, so
# the pending flag is set while wait -n has not been reached yet.
BASH_ENV_TEMPLATE = """\
signal_once() {{
    if [ ! -e "$SIGNAL_MARKER" ] && [ "$BASH_COMMAND" = "start_workers" ]; then
        : >"$SIGNAL_MARKER"
        kill -{signal} $$
    fi
}}
trap signal_once DEBUG
"""

# Same signal injection, plus a marker created before the start_workers call of
# the second round so shims can tell a restarted worker from the first one.
RELOAD_BASH_ENV = """\
signal_once() {
    if [ ! -e "$SIGNAL_MARKER" ] && [ "$BASH_COMMAND" = "start_workers" ]; then
        : >"$SIGNAL_MARKER"
        kill -HUP $$
    elif [ -e "$SIGNAL_MARKER" ] && [ ! -e "$RELOAD_MARKER" ] && \
         [ "$BASH_COMMAND" = "start_workers" ]; then
        : >"$RELOAD_MARKER"
    fi
}
trap signal_once DEBUG
"""

BLOCKING_SHIM = """\
#!/bin/sh
while kill -0 "$PPID" 2>/dev/null; do sleep 0.05; done
"""

RELOAD_OBSERVING_SHIM = """\
#!/bin/sh
if [ -e "$RELOAD_MARKER" ]; then
    exit 0
fi
while kill -0 "$PPID" 2>/dev/null; do sleep 0.05; done
"""


def write_fake_agentsight(tmp_path: Path, body: str) -> tuple[dict[str, str], Path]:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    shim = fake_bin / "agentsight"
    shim.write_text(body, encoding="utf-8")
    shim.chmod(0o755)
    launcher = tmp_path / "agentsight-start"
    subprocess.run(
        [
            "python3",
            str(RENDER_SCRIPT),
            "--bindir",
            str(fake_bin),
            "--binary",
            str(shim),
            "--launcher",
            str(launcher),
            "--unitdir",
            str(tmp_path / "units"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return {**os.environ, "PATH": f"{fake_bin}:{os.environ['PATH']}"}, launcher


def test_signal_pending_before_wait_is_not_lost(tmp_path: Path) -> None:
    """A SIGTERM trapped during startup must stop the supervisor, not block."""
    env, launcher = write_fake_agentsight(tmp_path, BLOCKING_SHIM)
    env_file = tmp_path / "bash_env.sh"
    env_file.write_text(BASH_ENV_TEMPLATE.format(signal="TERM"), encoding="utf-8")
    env.update(
        BASH_ENV=str(env_file),
        SIGNAL_MARKER=str(tmp_path / "signal-sent"),
    )
    started = time.monotonic()
    result = subprocess.run(
        ["bash", str(launcher)],
        capture_output=True,
        text=True,
        check=False,
        env=env,
        timeout=10,
    )
    elapsed = time.monotonic() - started
    assert (tmp_path / "signal-sent").exists()
    assert result.returncode == 0, result.stderr
    assert elapsed < 5, "supervisor blocked instead of consuming the pending signal"


def test_reload_pending_before_wait_restarts_workers(tmp_path: Path) -> None:
    """A SIGHUP trapped during startup must restart workers, not be dropped."""
    env, launcher = write_fake_agentsight(tmp_path, RELOAD_OBSERVING_SHIM)
    env_file = tmp_path / "bash_env.sh"
    env_file.write_text(RELOAD_BASH_ENV, encoding="utf-8")
    env.update(
        BASH_ENV=str(env_file),
        SIGNAL_MARKER=str(tmp_path / "hup-sent"),
        RELOAD_MARKER=str(tmp_path / "workers-restarted"),
    )
    result = subprocess.run(
        ["bash", str(launcher)],
        capture_output=True,
        text=True,
        check=False,
        env=env,
        timeout=10,
    )
    # The restarted workers exit immediately, so the supervisor exits 0; the
    # first-round workers never see the marker and would keep it blocked.
    assert (tmp_path / "hup-sent").exists()
    assert (tmp_path / "workers-restarted").exists(), result.stderr
    assert result.returncode == 0, result.stderr


def test_raw_launcher_uses_sibling_binary_instead_of_path(tmp_path: Path) -> None:
    """The relocated raw supervisor must execute both workers from its own directory."""
    payload = tmp_path / "relocated payload"
    payload.mkdir()
    markers = tmp_path / "worker-markers"
    markers.mkdir()
    sibling = payload / "agentsight"
    sibling.write_text(
        '#!/bin/sh\n: > "$WORKER_MARKER_DIR/$1"\n'
        'while kill -0 "$PPID" 2>/dev/null; do sleep 0.05; done\n'
    )
    sibling.chmod(0o755)
    bait_directory = tmp_path / "path-bait"
    bait_directory.mkdir()
    bait = bait_directory / "agentsight"
    bait.write_text('#!/bin/sh\n: > "$PATH_BAIT_MARKER"\nexit 91\n')
    bait.chmod(0o755)
    launcher = payload / "agentsight-start"
    subprocess.run(
        [
            "python3",
            str(RENDER_SCRIPT),
            "--bindir",
            "{bindir}",
            "--sibling-binary",
            "--launcher",
            str(launcher),
            "--unitdir",
            str(tmp_path / "units"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert "{bindir}" not in launcher.read_text()
    environment = {
        **os.environ,
        "PATH": f"{bait_directory}:{os.environ['PATH']}",
        "WORKER_MARKER_DIR": str(markers),
        "PATH_BAIT_MARKER": str(tmp_path / "path-was-used"),
    }
    environment.pop("BASH_ENV", None)
    process = subprocess.Popen(
        ["bash", str(launcher)],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if (markers / "trace").exists() and (markers / "serve").exists():
                break
            if process.poll() is not None:
                break
            time.sleep(0.01)
        assert (markers / "trace").exists()
        assert (markers / "serve").exists()
        assert not (tmp_path / "path-was-used").exists()
    finally:
        process.terminate()
        try:
            _, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
    assert process.returncode == 0, stderr
