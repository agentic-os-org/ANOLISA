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

"""Failure-path tests for ``redacted_json_file``.

``redacted_json_file`` records the agent config in the run's input manifest.
It handled ``OSError`` and ``json.JSONDecodeError`` only, so a config file that
is valid on disk but not UTF-8 raised ``UnicodeDecodeError`` -- a ``ValueError``,
not a ``JSONDecodeError`` -- straight out of ``collect_input_manifest`` and
aborted manifest generation for the whole run. A config written by an editor or
tool that uses the platform code page (GBK, windows-1252) hits this path.
"""

import json

from swe_runner.run.io.manifest_records import redacted_json_file


def test_non_utf8_config_is_skipped(tmp_path):
    config = tmp_path / "openclaw.json"
    config.write_bytes('{"model": "\u6a21\u578b"}'.encode("gbk"))

    assert redacted_json_file(config) is None


def test_non_utf8_config_still_reports_a_warning(tmp_path, caplog):
    config = tmp_path / "openclaw.json"
    config.write_bytes(b'{"model": "\xff\xfe"}')

    with caplog.at_level("WARNING"):
        assert redacted_json_file(config) is None

    assert "INPUT_MANIFEST_JSON_REDACTION_FAILED" in caplog.text


def test_missing_config_is_skipped(tmp_path):
    assert redacted_json_file(tmp_path / "absent.json") is None
    assert redacted_json_file(None) is None


def test_invalid_json_is_skipped(tmp_path):
    config = tmp_path / "openclaw.json"
    config.write_text("{not json", encoding="utf-8")

    assert redacted_json_file(config) is None


def test_object_payload_keeps_its_keys(tmp_path):
    config = tmp_path / "openclaw.json"
    config.write_text(json.dumps({"model": "gpt", "apiKey": "sk-live"}), encoding="utf-8")

    assert redacted_json_file(config) == {"model": "gpt", "apiKey": "<redacted>"}


def test_non_object_payload_is_wrapped(tmp_path):
    config = tmp_path / "openclaw.json"
    config.write_text('["a"]', encoding="utf-8")

    assert redacted_json_file(config) == {"value": ["a"]}
