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

"""Evaluation archives must not claim ownership of neighboring source files."""

import tarfile
from io import BytesIO
from pathlib import Path, PurePosixPath
from unittest.mock import MagicMock

import pytest

from swe_runner.evaluation.service import _copy_to_container_normalized_owner


@pytest.mark.parametrize("transfer_fails", [False, True])
def test_preserves_existing_adjacent_archive(tmp_path: Path, transfer_fails: bool) -> None:
    source = tmp_path / "patch.diff"
    source.write_bytes(b"diff --git a/file b/file\n")
    adjacent = tmp_path / "patch.tar"
    adjacent.write_bytes(b"unrelated saved archive")
    container = MagicMock()
    if transfer_fails:
        container.put_archive.side_effect = OSError("transfer failed")
        with pytest.raises(OSError, match="transfer failed"):
            _copy_to_container_normalized_owner(container, source, PurePosixPath("/tmp/patch.diff"))
    else:
        _copy_to_container_normalized_owner(container, source, PurePosixPath("/tmp/patch.diff"))

    assert source.read_bytes() == b"diff --git a/file b/file\n"
    assert adjacent.read_bytes() == b"unrelated saved archive"
    assert sorted(path.name for path in tmp_path.iterdir()) == ["patch.diff", "patch.tar"]


def test_tar_source_is_copied_without_being_replaced(tmp_path: Path) -> None:
    source = tmp_path / "saved.tar"
    source.write_bytes(b"original archive bytes")
    container = MagicMock()

    _copy_to_container_normalized_owner(container, source, PurePosixPath("/tmp/input.tar"))

    assert source.read_bytes() == b"original archive bytes"
    destination, archive_bytes = container.put_archive.call_args.args
    assert destination == "/tmp"
    with tarfile.open(fileobj=BytesIO(archive_bytes)) as archive:
        extracted = archive.extractfile("input.tar")
        assert extracted is not None
        assert extracted.read() == b"original archive bytes"


def test_directory_members_keep_contents_and_normalized_owners(tmp_path: Path) -> None:
    source = tmp_path / "workspace"
    source.mkdir()
    (source / "nested").mkdir()
    (source / "nested" / "patch.diff").write_bytes(b"patch content")
    container = MagicMock()

    _copy_to_container_normalized_owner(container, source, PurePosixPath("/testbed/copied"))

    container.exec_run.assert_called_once_with("mkdir -p /testbed")
    destination, archive_bytes = container.put_archive.call_args.args
    assert destination == "/testbed"
    with tarfile.open(fileobj=BytesIO(archive_bytes)) as archive:
        assert {member.name for member in archive.getmembers()} == {
            "copied",
            "copied/nested",
            "copied/nested/patch.diff",
        }
        assert all(
            (member.uid, member.gid, member.uname, member.gname) == (0, 0, "root", "root")
            for member in archive.getmembers()
        )
        extracted = archive.extractfile("copied/nested/patch.diff")
        assert extracted is not None
        assert extracted.read() == b"patch content"
    assert not (tmp_path / "workspace.tar").exists()
