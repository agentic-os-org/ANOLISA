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

"""Credential-key redaction tests for the input manifest records.

``_is_sensitive_key`` split camelCase with ``(?<!^)(?=[A-Z])``, which inserts an
underscore before *every* capital. A key written in caps or separated by
hyphens therefore reached the vocabulary as one letter per underscore --
``OPENAI_API_KEY`` became ``o_p_e_n_a_i__a_p_i__k_e_y`` -- matched nothing, and
``redacted_json_file`` recorded the secret verbatim in the run's input manifest.
Only ``apiKey``/``authToken``-style spellings were covered before.
"""

import pytest

from swe_runner.run.io.manifest_records import _is_sensitive_key, _redact_sensitive

REDACTED = "<redacted>"

# The same credential under the spellings a config file actually uses.
SENSITIVE_KEYS = (
    "apiKey",
    "APIKey",
    "api_key",
    "API_KEY",
    "X-API-Key",
    "x-api-key",
    "OPENAI_API_KEY",
    "openaiApiKey",
    "authToken",
    "Authorization",
    "TOKEN",
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "credentials",
)

# Neighbouring keys that are configuration, not credentials.
SAFE_KEYS = (
    "model",
    "base_url",
    "endpoint",
    "max_tokens",
    "total_tokens",
    "input_tokens",
    "prompt",
    "temperature",
    "sandbox_mode",
    "auth_mode",
    "api_version",
)


class TestSensitiveKeyShapes:
    """Red on main: caps and hyphenated spellings fall through."""

    @pytest.mark.parametrize("key", SENSITIVE_KEYS)
    def test_credential_spelling_is_recognised(self, key):
        assert _is_sensitive_key(key)

    @pytest.mark.parametrize("key", SAFE_KEYS)
    def test_other_keys_are_left_alone(self, key):
        assert not _is_sensitive_key(key)


class TestRedaction:
    def test_caps_key_value_is_replaced(self):
        assert _redact_sensitive({"OPENAI_API_KEY": "sk-live"}) == {
            "OPENAI_API_KEY": REDACTED
        }

    def test_nested_caps_key_value_is_replaced(self):
        payload = {"providers": {"X-API-Key": "abc", "model": "gpt"}}
        assert _redact_sensitive(payload) == {
            "providers": {"X-API-Key": REDACTED, "model": "gpt"}
        }

    def test_mapping_under_a_plural_key_is_replaced_whole(self):
        assert _redact_sensitive({"tokens": {"github": "ghp_x"}}) == {
            "tokens": REDACTED
        }

    def test_list_under_a_plural_key_is_replaced_whole(self):
        assert _redact_sensitive({"secrets": ["a", "b"]}) == {"secrets": REDACTED}

    def test_numeric_token_limit_keeps_its_value(self):
        assert _redact_sensitive({"max_tokens": 4096}) == {"max_tokens": 4096}
