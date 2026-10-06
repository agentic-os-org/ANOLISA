from pathlib import Path
import sys
import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))
sys.path.insert(0, str(BENCH / "campaign"))

import campaign_runtime as runtime


@pytest.mark.parametrize("replace_file", [True, False])
def test_regrown_log_retains_new_fatal_prefix(tmp_path, replace_file):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"old prefix\n" * 8)
    cursor = runtime.log_position(source)
    if replace_file:
        source.rename(tmp_path / "old.log")
    payload = b"worker panicked at src/lib.rs\n" + b"healthy tail\n" * 20
    source.write_bytes(payload)
    assert runtime.capture_runtime_log(source, cursor, destination) == (
        False,
        ["panic"],
    )
    assert destination.read_bytes() == payload


@pytest.mark.parametrize(
    "replace_file,payload", [(True, b"new clean\n" * 20), (False, b"new\n")]
)
def test_incomplete_log_interval_does_not_claim_clean(tmp_path, replace_file, payload):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"old prefix\n" * 8)
    cursor = runtime.log_position(source)
    if replace_file:
        source.rename(tmp_path / "old.log")
    source.write_bytes(payload)
    assert runtime.capture_runtime_log(source, cursor, destination) == (None, [])
    assert destination.read_bytes() == payload


def test_append_only_capture_keeps_exact_measured_segment(tmp_path):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"old unrelated panic\n")
    cursor = runtime.log_position(source)
    with source.open("ab") as stream:
        stream.write(b"healthy measured interval\n")
    assert runtime.capture_runtime_log(source, cursor, destination) == (True, [])
    assert destination.read_bytes() == b"healthy measured interval\n"


def test_legacy_zero_offset_and_missing_log_controls(tmp_path):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"worker panicked at src/lib.rs\n")
    assert runtime.capture_runtime_log(source, 0, destination) == (False, ["panic"])
    assert runtime.log_position(tmp_path / "missing") is None
    assert runtime.capture_runtime_log(None, None, destination) == (None, [])


def test_copytruncate_between_boundary_and_payload_read_is_unknown(
    tmp_path, monkeypatch
):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"old boundary\n")
    cursor = runtime.log_position(source)
    with source.open("ab") as stream:
        stream.write(b"healthy interval\n" * 100_000 + b"worker panicked at end\n")
    real_open = Path.open

    class CopyTruncateReader:
        def __init__(self, handle):
            self.handle = handle

        def __getattr__(self, name):
            return getattr(self.handle, name)

        def __enter__(self):
            return self

        def __exit__(self, *args):
            self.handle.close()

        def read(self, size=-1):
            if size == -1:
                source.write_bytes(b"")
            return self.handle.read(size)

    def opened(path, mode="r", *args, **kwargs):
        handle = real_open(path, mode, *args, **kwargs)
        if path == source and mode == "rb":
            return CopyTruncateReader(handle)
        return handle

    monkeypatch.setattr(Path, "open", opened)
    assert runtime.capture_runtime_log(source, cursor, destination) == (None, [])
    assert b"panicked at" not in destination.read_bytes()


def test_legacy_offset_truncation_is_inconclusive(tmp_path):
    source, destination = tmp_path / "source.log", tmp_path / "capture.log"
    source.write_bytes(b"clean shortened log\n")
    assert runtime.capture_runtime_log(source, 100, destination) == (None, [])
    assert destination.read_bytes() == source.read_bytes()
