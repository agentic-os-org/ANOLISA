from pathlib import Path
import sys
import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))
sys.path.insert(0, str(BENCH / "campaign"))

import subprocess
import campaign_manifest as manifest


@pytest.mark.parametrize("probe", [manifest.command_version, manifest.command_output])
@pytest.mark.parametrize("stream", ["stdout", "stderr"])
def test_nonzero_probe_is_unavailable(probe, stream):
    script = (
        "import sys; print('failed optional probe', file=sys."
        + stream
        + "); sys.exit(7)"
    )
    assert probe([sys.executable, "-c", script]) is None


@pytest.mark.parametrize("probe", [manifest.command_version, manifest.command_output])
@pytest.mark.filterwarnings("error::pytest.PytestUnhandledThreadExceptionWarning")
def test_invalid_encoding_is_unavailable(probe):
    assert probe([sys.executable, "-c", "import os; os.write(1, bytes([255]))"]) is None


def test_successful_stdout_and_stderr_contracts():
    command = [sys.executable, "-c", "print('tool 1.0'); print('extra details')"]
    assert manifest.command_version(command) == "tool 1.0"
    assert manifest.command_output(command) == "tool 1.0\nextra details"
    command = [sys.executable, "-c", "import sys; print('tool 1.0', file=sys.stderr)"]
    assert manifest.command_version(command) == "tool 1.0"
    assert manifest.command_output(command) == "tool 1.0"


@pytest.mark.parametrize(
    "error", [FileNotFoundError("missing"), subprocess.TimeoutExpired("probe", 5)]
)
def test_missing_and_timed_out_probes_remain_unavailable(monkeypatch, error):
    def fail(*args, **kwargs):
        raise error

    monkeypatch.setattr(manifest.subprocess, "run", fail)
    assert manifest.command_version(["unused"]) is None
    assert manifest.command_output(["unused"]) is None
