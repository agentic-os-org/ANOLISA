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

import json
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from swe_runner.trace_extraction import recording


def test_concurrent_recorders_preserve_every_session(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workers = 12
    barrier = threading.Barrier(workers)
    local = threading.local()
    original_next = recording._next_trace_file

    def synchronized_next(issue_dir: Path) -> Path:
        path = original_next(issue_dir)
        if not getattr(local, "ready", False):
            local.ready = True
            barrier.wait(timeout=10)
        return path

    monkeypatch.setattr(recording, "_next_trace_file", synchronized_next)
    payloads = [{"session_id": f"session-{index}", "text": "完整响应"} for index in range(workers)]
    with ThreadPoolExecutor(max_workers=workers) as pool:
        paths = list(pool.map(lambda payload: recording._write_trace_file("case-1", tmp_path, payload), payloads))

    assert len(set(paths)) == workers
    assert len(list((tmp_path / "case-1").glob("trace*.json"))) == workers
    saved = [json.loads(path.read_text(encoding="utf-8")) for path in paths]
    assert sorted(saved, key=lambda value: value["session_id"]) == sorted(
        payloads, key=lambda value: value["session_id"]
    )


def test_existing_and_sequential_traces_keep_numbering(tmp_path: Path) -> None:
    directory = tmp_path / "case-1"
    directory.mkdir()
    original = '{"session_id":"existing"}'
    (directory / "trace1.json").write_text(original, encoding="utf-8")
    (directory / "trace9.json").write_text("{}", encoding="utf-8")
    (directory / "trace-not-numbered.json").write_text("{}", encoding="utf-8")
    first = recording._write_trace_file("case-1", tmp_path, {"session_id": "first"})
    second = recording._write_trace_file("case-1", tmp_path, {"session_id": "second"})
    assert (first.name, second.name) == ("trace10.json", "trace11.json")
    assert (directory / "trace1.json").read_text(encoding="utf-8") == original


def test_file_errors_propagate_without_retrying(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls = 0

    def denied(*args: object, **kwargs: object) -> None:
        nonlocal calls
        calls += 1
        raise PermissionError("denied")

    monkeypatch.setattr("builtins.open", denied)
    with pytest.raises(PermissionError, match="denied"):
        recording._write_trace_file("case-1", tmp_path, {})
    assert calls == 1
