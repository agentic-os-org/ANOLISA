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

"""Tests for scripts/check_judge_multimodal.py.

Covers:
- the probe image decodes to a single opaque red pixel
- only the single-word ``red`` reply counts as seeing the image
- wrong colors, refusals, empty replies and prose fail
- the CLI exits 0/PASS only for that reply, and sends the fixture in the request
"""
from __future__ import annotations

import base64
import struct
import sys
import zlib
from pathlib import Path
from types import ModuleType
from unittest.mock import MagicMock

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import check_judge_multimodal

# Referenced through the module so a missing helper fails the test that needs it
# instead of aborting collection for the whole file.
probe = check_judge_multimodal


def _mock_client(response_text: str | None) -> tuple[object, MagicMock]:
    """Build a client factory that returns *response_text*."""
    mock_resp = MagicMock()
    mock_resp.choices = [MagicMock()]
    mock_resp.choices[0].message.content = response_text

    mock_client = MagicMock()
    mock_client.chat.completions.create.return_value = mock_resp

    def factory(model: str, base_url: str, api_key: str) -> MagicMock:
        return mock_client

    return factory, mock_client


class TestCheckMultimodal:
    """The verdict follows the single-word reply contract of the prompt."""

    @pytest.mark.parametrize("response_text", ["red", "Red", "RED", "  red  ", "red.", "\nRed\n"])
    def test_single_word_red_is_vision_capable(self, response_text: str) -> None:
        factory, _ = _mock_client(response_text)

        assert probe.check_multimodal("test-model", "http://x", "k", factory) is True

    @pytest.mark.parametrize("response_text", [
        "blue",  # wrong color for an opaque red pixel
        "OK",  # unrelated acknowledgement
        "the color is red",  # prose, not the requested single word
        "I see red",
        "I cannot see images",  # explicit refusal
        "NO_IMAGE",
        "no_image because I'm text-only",
        "",
        None,
        "   \n  ",
    ])
    def test_everything_else_is_not_vision_capable(self, response_text: str | None) -> None:
        factory, _ = _mock_client(response_text)

        assert probe.check_multimodal("test-model", "http://x", "k", factory) is False

    def test_request_carries_the_probe_image_and_prompt(self) -> None:
        factory, mock_client = _mock_client("red")

        probe.check_multimodal("test-model", "http://x", "k", factory)

        kwargs = mock_client.chat.completions.create.call_args.kwargs
        parts = kwargs["messages"][0]["content"]
        assert parts[0]["text"] == probe.PROBE_PROMPT
        assert parts[1]["image_url"]["url"] == (
            f"data:image/png;base64,{probe.TINY_RED_PNG_B64}"
        )


class TestReplyContract:
    """The reply helper is exercised on its own as well."""

    @pytest.mark.parametrize("reply", ["red", " RED ", "red.", "Red!"])
    def test_accepts_the_single_color_word(self, reply: str) -> None:
        assert probe.reply_confirms_image(reply) is True

    @pytest.mark.parametrize("reply", ["reddish", "red blue", "no_image", "", None, "  "])
    def test_rejects_anything_else(self, reply: str | None) -> None:
        assert probe.reply_confirms_image(reply) is False


class TestProbeImage:
    """The embedded fixture is a valid opaque red pixel."""

    @staticmethod
    def _chunks(data: bytes) -> dict[bytes, bytes]:
        chunks: dict[bytes, bytes] = {}
        offset = 8
        while offset < len(data):
            length = struct.unpack(">I", data[offset:offset + 4])[0]
            kind = data[offset + 4:offset + 8]
            payload = data[offset + 8:offset + 8 + length]
            crc = struct.unpack(">I", data[offset + 8 + length:offset + 12 + length])[0]
            assert crc == zlib.crc32(kind + payload) & 0xFFFFFFFF, f"bad CRC for {kind!r}"
            chunks[kind] = payload
            offset += 12 + length
        return chunks

    def test_image_is_a_valid_1x1_rgba_png(self) -> None:
        data = base64.b64decode(probe.TINY_RED_PNG_B64)

        assert data[:8] == b"\x89PNG\r\n\x1a\n"
        chunks = self._chunks(data)
        width, height, depth, color_type = struct.unpack(">IIBB", chunks[b"IHDR"][:10])
        assert (width, height, depth, color_type) == (1, 1, 8, 6)

    def test_pixel_is_opaque_red(self) -> None:
        data = base64.b64decode(probe.TINY_RED_PNG_B64)
        raw = zlib.decompress(self._chunks(data)[b"IDAT"])

        assert raw[0] == 0, "expected filter type 0 on the only scanline"
        assert tuple(raw[1:5]) == (255, 0, 0, 255)


class TestCli:
    """The CLI reports PASS/FAIL for the verdict it computed."""

    @staticmethod
    def _run(monkeypatch: pytest.MonkeyPatch, reply: str) -> int:
        _factory, mock_client = _mock_client(reply)
        fake_openai = ModuleType("openai")
        fake_openai.OpenAI = lambda api_key, base_url: mock_client  # type: ignore[attr-defined]
        monkeypatch.setitem(sys.modules, "openai", fake_openai)
        monkeypatch.setattr(sys, "argv", [
            "check_judge_multimodal.py",
            "--model", "test-model",
            "--base-url", "http://x",
            "--api-key", "k",
        ])
        with pytest.raises(SystemExit) as excinfo:
            probe.main()
        return int(excinfo.value.code or 0)

    def test_cli_passes_for_red(self, monkeypatch: pytest.MonkeyPatch, capsys) -> None:
        assert self._run(monkeypatch, "red") == 0
        assert "PASS" in capsys.readouterr().out

    def test_cli_fails_for_a_wrong_color(self, monkeypatch: pytest.MonkeyPatch, capsys) -> None:
        assert self._run(monkeypatch, "blue") == 1
        assert "FAIL" in capsys.readouterr().out

    def test_cli_fails_for_an_explicit_refusal(
        self, monkeypatch: pytest.MonkeyPatch, capsys
    ) -> None:
        assert self._run(monkeypatch, "I cannot see images") == 1
        assert "FAIL" in capsys.readouterr().out
