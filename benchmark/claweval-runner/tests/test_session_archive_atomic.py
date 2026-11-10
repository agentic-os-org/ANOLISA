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

"""A failed session archive must not publish a truncated file.

`archive_session_alongside_trace` copied straight to the final archive name.
A copy that wrote part of the session and then raised left the truncation
under that name and discarded a complete archive from an earlier run, while
still reporting failure.

These tests drive the real helper with a copy that behaves the same way: it
writes some bytes into the path it is given and then raises `OSError`.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SRC = Path(__file__).resolve().parents[1] / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from ce_runner import pipeline  # noqa: E402  (path set up above)

SESSION = '{"type": "session", "id": "abc"}\n'


def _failing_copy(written: bytes):
    """Return a copy replacement that writes part of the file, then raises."""

    def copy(_source, destination, *args, **kwargs):
        Path(destination).write_bytes(written)
        raise OSError("simulated write failure")

    return copy


class SessionArchiveAtomicTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-session-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        traces = self.root / "traces"
        traces.mkdir()
        self.session = self.root / "session.jsonl"
        self.session.write_text(SESSION, encoding="utf-8")
        self.trace = traces / "T001_trial1.jsonl"
        self.trace.write_text("{}\n", encoding="utf-8")
        self.archive = traces / "sessions" / "T001_trial1.session.jsonl"

    def partials(self) -> list[Path]:
        sessions = self.root / "traces" / "sessions"
        return sorted(sessions.glob("*.partial")) if sessions.is_dir() else []

    def test_partial_copy_publishes_nothing(self) -> None:
        with mock.patch.object(pipeline.shutil, "copy2", _failing_copy(b'{"type"')):
            self.assertEqual(
                pipeline.archive_session_alongside_trace(str(self.session), str(self.trace)), ""
            )
        self.assertFalse(self.archive.exists(), "a truncated archive was published")
        self.assertEqual(self.partials(), [], "the staging file was left behind")

    def test_partial_copy_keeps_the_previous_archive(self) -> None:
        self.archive.parent.mkdir(parents=True)
        complete = b'{"type": "session", "complete": true}\n'
        self.archive.write_bytes(complete)

        with mock.patch.object(pipeline.shutil, "copy2", _failing_copy(b'{"type"')):
            self.assertEqual(
                pipeline.archive_session_alongside_trace(str(self.session), str(self.trace)), ""
            )

        self.assertEqual(self.archive.read_bytes(), complete, "the previous archive was damaged")
        self.assertEqual(self.partials(), [], "the staging file was left behind")

    def test_success_publishes_the_complete_copy(self) -> None:
        published = pipeline.archive_session_alongside_trace(str(self.session), str(self.trace))
        self.assertEqual(published, str(self.archive))
        self.assertEqual(self.archive.read_text(encoding="utf-8"), SESSION)
        self.assertEqual(self.partials(), [], "the staging file was left behind")

    def test_missing_source_still_returns_empty(self) -> None:
        self.assertEqual(
            pipeline.archive_session_alongside_trace(str(self.root / "gone.jsonl"), str(self.trace)),
            "",
        )
        self.assertFalse(self.archive.exists())


if __name__ == "__main__":
    unittest.main()
