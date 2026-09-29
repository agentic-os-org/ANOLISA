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
- check_multimodal returns True when model responds with a color name
- check_multimodal returns False on NO_IMAGE / empty / None responses
- Embedded probe PNG is valid and 1x1
"""
from __future__ import annotations

import sys
from pathlib import Path
from unittest.mock import MagicMock

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

from check_judge_multimodal import TINY_RED_PNG_B64, check_multimodal  # noqa: E402


class TestCheckMultimodal:
    """Unit tests for check_multimodal using mocked OpenAI client."""

    @staticmethod
    def _mock_client(response_text: str):
        """Build a mock client factory that returns *response_text*."""
        mock_resp = MagicMock()
        mock_resp.choices = [MagicMock()]
        mock_resp.choices[0].message.content = response_text

        mock_client = MagicMock()
        mock_client.chat.completions.create.return_value = mock_resp

        def factory(model, base_url, api_key):
            return mock_client

        return factory, mock_client

    # ── Success cases ──────────────────────────────────────────────

    @pytest.mark.parametrize("response_text", [
        "red",
        "Red",
        "RED",
        "  red  ",
        "the color is red",
        "I see red",
    ])
    def test_model_sees_image(self, response_text):
        """Model returns a color name -> vision capable."""
        factory, mock_client = self._mock_client(response_text)
        assert check_multimodal("test-model", "http://x", "k", factory) is True

    # ── Failure cases ──────────────────────────────────────────────

    def test_model_reports_no_image(self):
        """Model explicitly reports NO_IMAGE -> not vision capable."""
        factory, _ = self._mock_client("NO_IMAGE")
        assert check_multimodal("test-model", "http://x", "k", factory) is False

    def test_model_reports_no_image_lowercase(self):
        """Case-insensitive NO_IMAGE detection."""
        factory, _ = self._mock_client("no_image because I'm text-only")
        assert check_multimodal("test-model", "http://x", "k", factory) is False

    def test_empty_response(self):
        """Empty response -> not vision capable."""
        factory, _ = self._mock_client("")
        assert check_multimodal("test-model", "http://x", "k", factory) is False

    def test_none_response(self):
        """None response (content is None) -> not vision capable."""
        factory, _ = self._mock_client(None)
        assert check_multimodal("test-model", "http://x", "k", factory) is False

    # ── Edge cases ─────────────────────────────────────────────────

    def test_whitespace_only_response(self):
        """Whitespace-only response -> not vision capable."""
        factory, _ = self._mock_client("   \n  ")
        assert check_multimodal("test-model", "http://x", "k", factory) is False


class TestProbeImage:
    """Verify the embedded test image is valid."""

    def test_image_is_valid_base64(self):
        import base64
        data = base64.b64decode(TINY_RED_PNG_B64)
        # PNG magic bytes
        assert data[:8] == b'\x89PNG\r\n\x1a\n', "Embedded probe image is not a valid PNG"

    def test_image_is_1x1(self):
        import base64
        import struct
        data = base64.b64decode(TINY_RED_PNG_B64)
        # IHDR starts at byte 16 (8-byte signature + 4-byte length + 4-byte 'IHDR')
        width, height = struct.unpack('>II', data[16:24])
        assert width == 1
        assert height == 1
