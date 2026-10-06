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

"""Regression tests for sensitive-key redaction in input manifest records."""

from __future__ import annotations

import pytest

from swe_runner.run.io.manifest_records import _redact_sensitive


@pytest.mark.parametrize(
    ("key", "expected_redacted"),
    [
        ("apiKey", True),
        ("x_api_key", True),
        ("password", True),
        # Acronym-heavy casing must redact just like the plain forms.
        ("APIKey", True),
        ("APIKEY", True),
        ("SECRET", True),
        ("AUTH_TOKEN", True),
        ("ApiToken", True),
        # Benign keys that merely contain sensitive substrings stay visible.
        ("tokenizer", False),
        ("max_tokens", False),
        ("token_count", False),
        ("secret_name_is_public", False),
    ],
)
def test_sensitive_key_casings(key: str, expected_redacted: bool) -> None:
    value = "leak-me"
    result = _redact_sensitive({key: value})
    if expected_redacted:
        assert result[key] == "<redacted>"
    else:
        assert result[key] == value


def test_nested_and_list_values_are_redacted() -> None:
    payload = {
        "provider": {"APIKey": "sk-1", "name": "bailian"},
        "secrets": [{"SECRET": "s", "label": "prod"}],
    }
    result = _redact_sensitive(payload)
    assert result["provider"]["APIKey"] == "<redacted>"
    assert result["provider"]["name"] == "bailian"
    assert result["secrets"][0]["SECRET"] == "<redacted>"
    assert result["secrets"][0]["label"] == "prod"
