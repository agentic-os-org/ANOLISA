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

"""`_send_prompt` reports a timeout as a timeout, not as a generic failure.

`httpx.TimeoutException` subclasses `httpx.HTTPError`, so the handler order
decides which message a stalled gateway produces.
"""

import json
import sys
from pathlib import Path

import httpx
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import prompt_task


@pytest.fixture
def gateway_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point the module at a temporary gateway config."""
    config = tmp_path / "openclaw.json"
    config.write_text(
        json.dumps({"gateway": {"port": 18789, "auth": {"token": "t"}}}),
        encoding="utf-8",
    )
    monkeypatch.setattr(prompt_task, "OPENCLAW_CONFIG", config)
    return config


def _fail_with(monkeypatch: pytest.MonkeyPatch, error: Exception) -> None:
    def post(*args, **kwargs):
        raise error

    monkeypatch.setattr(httpx, "post", post)


def test_timeout_is_reported_as_a_timeout(
    gateway_config: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _fail_with(monkeypatch, httpx.TimeoutException("timed out"))

    result = prompt_task._send_prompt("hello", "agent", timeout=12)

    assert result == ""
    out = capsys.readouterr().out
    assert "timed out after 132s" in out, out
    assert "HTTP API call failed" not in out


def test_other_http_errors_keep_the_generic_message(
    gateway_config: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _fail_with(monkeypatch, httpx.ConnectError("connection refused"))

    result = prompt_task._send_prompt("hello", "agent", timeout=12)

    assert result == ""
    out = capsys.readouterr().out
    assert "HTTP API call failed" in out, out
    assert "connection refused" in out
    assert "timed out" not in out
