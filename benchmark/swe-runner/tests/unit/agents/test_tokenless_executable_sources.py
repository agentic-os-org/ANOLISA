# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Executable ownership contract for copied sandbox helper binaries."""

import json
import os
import subprocess
from pathlib import Path

import pytest

from swe_runner.agents.openclaw import sandbox


def _binary(path: Path, *, executable: bool) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nprintf 'private-helper-ok\\n'\n", encoding="utf-8")
    path.chmod(0o755 if executable else 0o644)
    return path


def _private_candidates(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> tuple[Path, Path]:
    first, second = tmp_path / "installed", tmp_path / "fallback"
    monkeypatch.setattr(sandbox.shutil, "which", lambda _: None)
    monkeypatch.setattr(sandbox, "_HOST_TOKENLESS_BIN_DIR", first)
    monkeypatch.setattr(sandbox, "_HOST_TOKENLESS_EXTRA_BIN_DIRS", (second,))
    monkeypatch.setattr(sandbox, "_HOST_OPENCLAW_EXTENSIONS_DIR", tmp_path / "extensions")
    return first, second


@pytest.mark.parametrize("name", ["rtk", "tokenless"])
def test_nonexecutable_install_does_not_shadow_executable_fallback(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    name: str,
) -> None:
    first, second = _private_candidates(monkeypatch, tmp_path)
    rejected = _binary(first / name, executable=False)
    accepted = _binary(second / name, executable=True)
    assert not os.access(rejected, os.X_OK)
    assert sandbox._resolve_host_tokenless_binary(name) == accepted


def test_injection_copies_runnable_fallbacks_and_records_selected_sources(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    first, second = _private_candidates(monkeypatch, tmp_path)
    for name in sandbox._TOKENLESS_BINARY_NAMES:
        _binary(first / name, executable=False)
        _binary(second / name, executable=True)
    workspace = tmp_path / "workspace"
    sandbox._inject_tokenless_binaries(workspace)
    records = json.loads((workspace / ".runner/tokenless/injection.json").read_text(encoding="utf-8"))["binaries"]
    for record in records:
        copied = Path(record["copied"])
        assert record["source"] == str(second / record["name"])
        result = subprocess.run([str(copied)], capture_output=True, text=True, timeout=5, check=True)
        assert result.stdout == "private-helper-ok\n"
    assert all((first / name).stat().st_mode & 0o111 == 0 for name in sandbox._TOKENLESS_BINARY_NAMES)


@pytest.mark.parametrize("shape", ["file", "directory", "broken_link"])
def test_no_runnable_source_is_reported_before_copy(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    shape: str,
) -> None:
    first, _ = _private_candidates(monkeypatch, tmp_path)
    candidate = first / "rtk"
    candidate.parent.mkdir()
    if shape == "file":
        _binary(candidate, executable=False)
    elif shape == "directory":
        candidate.mkdir()
    else:
        candidate.symlink_to(first / "missing")
    with pytest.raises(RuntimeError) as raised:
        sandbox._resolve_host_tokenless_binary("rtk")
    assert str(candidate) in str(raised.value)


def test_executable_priority_and_symlink_are_preserved(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    first, second = _private_candidates(monkeypatch, tmp_path)
    selected = _binary(first / "rtk", executable=True)
    _binary(second / "rtk", executable=True)
    assert sandbox._resolve_host_tokenless_binary("rtk") == selected
    selected.unlink()
    selected.symlink_to(second / "rtk")
    assert sandbox._resolve_host_tokenless_binary("rtk") == selected


def test_all_sources_are_validated_before_injection_writes(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    first, _ = _private_candidates(monkeypatch, tmp_path)
    _binary(first / "rtk", executable=True)
    _binary(first / "tokenless", executable=False)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    with pytest.raises(RuntimeError):
        sandbox._inject_tokenless_binaries(workspace)
    assert list(workspace.iterdir()) == []
