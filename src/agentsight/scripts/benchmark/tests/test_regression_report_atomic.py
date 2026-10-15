from pathlib import Path
import sys
import pytest

ROOT = Path(__file__).resolve().parents[5]
BENCH = ROOT / "src/agentsight/scripts/benchmark"
sys.path.insert(0, str(BENCH / "single_run"))
sys.path.insert(0, str(BENCH / "campaign"))

import contextlib
import json
import os
import run_regression as regression

CHECKS = [{"label": "controlled", "exit_code": 0}]


def fail_write(monkeypatch):
    original = Path.open

    @contextlib.contextmanager
    def partial(path, *args, **kwargs):
        mode = args[0] if args else kwargs.get("mode", "r")
        with original(path, *args, **kwargs) as stream:
            if "w" not in mode:
                yield stream
                return

            class Broken:
                def write(self, text):
                    stream.write(text[:7])
                    stream.flush()
                    raise OSError("controlled partial write")

                def __getattr__(self, name):
                    return getattr(stream, name)

            yield Broken()

    monkeypatch.setattr(Path, "open", partial)


@pytest.mark.parametrize("existing", [True, False])
def test_partial_write_keeps_complete_evidence_and_cleans_stage(
    tmp_path, monkeypatch, existing
):
    path = tmp_path / "checks.json"
    if existing:
        path.write_bytes(b'"previous complete evidence"')
    fail_write(monkeypatch)
    with pytest.raises(OSError, match="partial"):
        regression.write_report(path, CHECKS)
    if existing:
        assert path.read_bytes() == b'"previous complete evidence"'
    else:
        assert not path.exists()
    assert sorted(item.name for item in tmp_path.iterdir()) == (
        ["checks.json"] if existing else []
    )


@pytest.mark.parametrize("operation", ["fsync", "replace"])
def test_publication_failure_preserves_completed_report(
    tmp_path, monkeypatch, operation
):
    path = tmp_path / "checks.json"
    path.write_bytes(b'"previous complete evidence"')

    def fail(*args, **kwargs):
        raise OSError("controlled " + operation)

    monkeypatch.setattr(os, operation, fail)
    with pytest.raises(OSError, match=operation):
        regression.write_report(path, CHECKS)
    assert path.read_bytes() == b'"previous complete evidence"'
    assert [item.name for item in tmp_path.iterdir()] == ["checks.json"]


def test_successful_progress_schema_and_existing_mode(tmp_path):
    path = tmp_path / "checks.json"
    path.write_bytes(b'"previous complete evidence"')
    path.chmod(0o640)
    mode = path.stat().st_mode
    regression.write_report(path, CHECKS, full=True, cargo_jobs=2)
    assert json.loads(path.read_text(encoding="utf-8")) == {
        "schema_version": 1,
        "checks": CHECKS,
        "full": True,
        "cargo_jobs": 2,
    }
    assert path.stat().st_mode == mode


@pytest.mark.skipif(os.name == "nt", reason="POSIX symlink fixture")
def test_symlink_destination_retains_write_through_behavior(tmp_path):
    target, link = tmp_path / "target.json", tmp_path / "checks.json"
    target.write_text("{}", encoding="utf-8")
    link.symlink_to(target.name)
    regression.write_report(link, CHECKS)
    assert link.is_symlink()
    assert json.loads(target.read_text(encoding="utf-8"))["checks"] == CHECKS
