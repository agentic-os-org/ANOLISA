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

"""Keep provenance hashes identical while bounding file-content memory."""

from __future__ import annotations

import hashlib
import json
import tracemalloc
from pathlib import Path

import pytest

from swe_runner.run.io.manifest_records import directory_tree_record, file_record


@pytest.mark.parametrize("data", [b"", b"fixture", bytes(range(256)) * 513])
def test_file_digest_and_size_match_reference(tmp_path, data):
    path = tmp_path / "input.bin"
    path.write_bytes(data)
    assert file_record(path) == {
        "path": str(path),
        "exists": True,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
    }


def test_directory_fingerprint_keeps_canonical_order_and_records(tmp_path):
    root = tmp_path / "profile"
    (root / "nested").mkdir(parents=True)
    (root / "nested/b.bin").write_bytes(b"binary\x00content")
    (root / "a.txt").write_bytes(b"alpha")
    expected = [
        {"path": "a.txt", "sha256": hashlib.sha256(b"alpha").hexdigest(), "bytes": 5},
        {"path": "nested/b.bin", "sha256": hashlib.sha256(b"binary\x00content").hexdigest(), "bytes": 14},
    ]
    canonical = json.dumps(expected, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    assert directory_tree_record(root) == {
        "path": str(root),
        "exists": True,
        "file_count": 2,
        "sha256": hashlib.sha256(canonical).hexdigest(),
        "files": expected,
    }


@pytest.mark.parametrize("tree", [False, True])
def test_manifest_hashing_does_not_bulk_read_contents(tmp_path, monkeypatch, tree):
    path = tmp_path / "input.bin"
    path.write_bytes(b"fixture")

    def forbidden(*args, **kwargs):
        pytest.fail("Manifest hashing must stream file content")

    monkeypatch.setattr(Path, "read_bytes", forbidden)
    record = directory_tree_record(tmp_path) if tree else file_record(path)
    assert record["exists"] is True


@pytest.mark.parametrize("tree", [False, True])
def test_large_file_has_bounded_hashing_memory(tmp_path, tree):
    path = tmp_path / "input.bin"
    path.write_bytes(b"x" * (4 * 1024 * 1024))
    tracemalloc.start()
    try:
        record = directory_tree_record(tmp_path) if tree else file_record(path)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 1_000_000, f"4MiB source content retained {peak} bytes"
    item = record["files"][0] if tree else record
    assert item["bytes"] == 4 * 1024 * 1024


def test_missing_and_nonfile_inputs_keep_the_existing_shape(tmp_path):
    assert file_record(None) == {"path": None, "exists": False}
    assert directory_tree_record(None) == {"path": None, "exists": False}
    assert file_record(tmp_path) == {"path": str(tmp_path), "exists": False}
    missing = tmp_path / "missing"
    assert file_record(missing) == {"path": str(missing), "exists": False}


def test_read_failure_still_propagates_and_closes_handle(tmp_path, monkeypatch):
    path = tmp_path / "input.bin"
    path.write_bytes(b"fixture")
    original = Path.open
    observed = []

    class Broken:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            observed.append("closed")

        def read(self, *args):
            raise OSError("fixture read failure")

    monkeypatch.setattr(
        Path,
        "open",
        lambda candidate, *args, **kwargs: Broken() if candidate == path else original(candidate, *args, **kwargs),
    )
    with pytest.raises(OSError, match="fixture read failure"):
        file_record(path)
    assert observed == ["closed"]
