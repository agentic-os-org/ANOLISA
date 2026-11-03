"""Regression coverage for publishing the benchmark OpenClaw config."""

from __future__ import annotations

import importlib.util
import json
import os
import stat
from pathlib import Path
from types import ModuleType

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "configure_openclaw.py"


def load_configure_openclaw() -> ModuleType:
    spec = importlib.util.spec_from_file_location("configure_openclaw", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_main_keeps_config_updates_and_output(tmp_path: Path, monkeypatch, capsys) -> None:
    module = load_configure_openclaw()
    config_path = tmp_path / "openclaw.json"
    config_path.write_text(
        json.dumps(
            {
                "models": {
                    "providers": {
                        "fixture": {
                            "models": [{"id": "model-a", "contextWindow": 128000}]
                        }
                    }
                },
                "agents": {"defaults": {"params": {"temperature": 0.7}}},
                "gateway": {"port": 19000},
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(module, "OPENCLAW_CONFIG", config_path)

    module.main()

    saved = json.loads(config_path.read_text(encoding="utf-8"))
    model = saved["models"]["providers"]["fixture"]["models"][0]
    assert model["contextWindow"] == 256000
    assert model["input"] == ["text", "image"]
    assert saved["agents"]["defaults"]["params"]["temperature"] == 0
    assert saved["gateway"]["port"] == 19000
    assert f"Updated {config_path}:" in capsys.readouterr().out
    assert list(tmp_path.glob(".openclaw.json.*.tmp")) == []


def test_partial_serialization_failure_preserves_original_and_cleans_temp(
    tmp_path: Path, monkeypatch
) -> None:
    module = load_configure_openclaw()
    config_path = tmp_path / "openclaw.json"
    original = b'{"keep": "the original bytes"}\n'
    config_path.write_bytes(original)

    def fail_after_partial_write(config, stream, **kwargs):
        stream.write('{"partial":')
        raise OSError("simulated write failure")

    monkeypatch.setattr(module.json, "dump", fail_after_partial_write)
    with pytest.raises(OSError, match="simulated write failure"):
        module.write_config_atomically({"new": True}, config_path)

    assert config_path.read_bytes() == original
    assert list(tmp_path.glob(".openclaw.json.*.tmp")) == []


def test_replace_failure_preserves_original_and_cleans_temp(
    tmp_path: Path, monkeypatch
) -> None:
    module = load_configure_openclaw()
    config_path = tmp_path / "openclaw.json"
    original = b'{"keep": "the original bytes"}'
    config_path.write_bytes(original)

    def fail_replace(source, target):
        raise OSError("simulated replace failure")

    monkeypatch.setattr(module.os, "replace", fail_replace)
    with pytest.raises(OSError, match="simulated replace failure"):
        module.write_config_atomically({"new": True}, config_path)

    assert config_path.read_bytes() == original
    assert list(tmp_path.glob(".openclaw.json.*.tmp")) == []


def test_atomic_replace_preserves_existing_mode_and_symlink(tmp_path: Path) -> None:
    if not hasattr(os, "symlink"):
        pytest.skip("symlinks are unavailable")

    module = load_configure_openclaw()
    target = tmp_path / "actual-config.json"
    target.write_text('{"old": true}', encoding="utf-8")
    target.chmod(0o640)
    link = tmp_path / "openclaw.json"
    try:
        link.symlink_to(target)
    except OSError as exc:
        pytest.skip(f"symlinks are unavailable: {exc}")

    module.write_config_atomically({"new": True}, link)

    assert link.is_symlink()
    assert json.loads(target.read_text(encoding="utf-8")) == {"new": True}
    assert stat.S_IMODE(target.stat().st_mode) == 0o640
    assert list(tmp_path.glob(".actual-config.json.*.tmp")) == []
