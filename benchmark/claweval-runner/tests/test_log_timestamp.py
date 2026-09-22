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

"""Tests for the timestamped ``log()`` helper in ce_runner._common."""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from ce_runner import _common

# [HH:MM:SS.mmm] prefix, e.g. "[12:34:56.789] "
TS_PREFIX = re.compile(r"^\[\d{2}:\d{2}:\d{2}\.\d{3}\] ")


def test_stdout_has_timestamp_prefix(capsys):
    """log() must prefix stdout lines with a [HH:MM:SS.mmm] timestamp."""
    _common.log("hello world")
    out = capsys.readouterr().out
    assert TS_PREFIX.match(out), f"missing timestamp prefix: {out!r}"
    assert out.rstrip("\n").endswith("hello world")


def test_file_sink_shares_same_timestamp(tmp_path, capsys):
    """File mirror and stdout should carry an identical timestamp per call."""
    log_path = tmp_path / "batch.log"
    _common.attach_log_file(str(log_path))
    try:
        _common.log("aligned message")
        out = capsys.readouterr().out
    finally:
        _common.detach_log_file()

    stdout_line = next(
        line for line in out.splitlines() if "aligned message" in line
    )
    file_line = next(
        line for line in log_path.read_text().splitlines()
        if "aligned message" in line
    )
    assert stdout_line == file_line


def test_detached_sink_does_not_break_logging(capsys):
    """After detaching, log() still works and keeps the timestamp prefix."""
    _common.detach_log_file()
    _common.log("after detach")
    out = capsys.readouterr().out
    assert TS_PREFIX.match(out)
