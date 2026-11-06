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

"""Docker-context probe tests for ``_ensure_docker_host_for_rootless_context``.

Evaluation setup probes ``~/.docker/config.json`` to detect a rootless daemon.
The probe read the file with the locale encoding and assumed the JSON held an
object: a config holding ``null`` raised ``AttributeError`` on ``.get`` and a
config that is not UTF-8 raised ``UnicodeDecodeError``, neither of which was
caught. Both aborted evaluation before any instance ran.
"""

import json
import os

import pytest

from swe_runner.evaluation.service import _ensure_docker_host_for_rootless_context


@pytest.fixture(autouse=True)
def _clear_docker_host(monkeypatch):
    monkeypatch.delenv("DOCKER_HOST", raising=False)


def _config(tmp_path, payload):
    path = tmp_path / "config.json"
    path.write_text(payload, encoding="utf-8")
    return path


class TestDockerContextProbe:
    """Red on main: an unusable config raises instead of meaning "no context"."""

    @pytest.mark.parametrize("payload", ["null", "[]", '"rootless"', "42"])
    def test_non_object_config_is_ignored(self, tmp_path, payload):
        _ensure_docker_host_for_rootless_context(
            docker_config=_config(tmp_path, payload),
            rootless_socket=tmp_path / "absent.sock",
        )

        assert "DOCKER_HOST" not in os.environ

    def test_non_utf8_config_is_ignored(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_bytes(b'{"currentContext": "\xb8\xf9"}')

        _ensure_docker_host_for_rootless_context(
            docker_config=path,
            rootless_socket=tmp_path / "absent.sock",
        )

        assert "DOCKER_HOST" not in os.environ

    def test_missing_config_is_ignored(self, tmp_path):
        _ensure_docker_host_for_rootless_context(
            docker_config=tmp_path / "absent.json",
            rootless_socket=tmp_path / "absent.sock",
        )

        assert "DOCKER_HOST" not in os.environ

    def test_existing_docker_host_is_never_replaced(self, tmp_path, monkeypatch):
        monkeypatch.setenv("DOCKER_HOST", "unix:///custom.sock")
        socket = tmp_path / "rootless.sock"
        socket.touch()

        _ensure_docker_host_for_rootless_context(
            docker_config=_config(tmp_path, json.dumps({"currentContext": "rootless"})),
            rootless_socket=socket,
        )

        assert os.environ["DOCKER_HOST"] == "unix:///custom.sock"

    def test_rootless_context_selects_the_socket(self, tmp_path):
        socket = tmp_path / "rootless.sock"
        socket.touch()

        _ensure_docker_host_for_rootless_context(
            docker_config=_config(tmp_path, json.dumps({"currentContext": "rootless"})),
            rootless_socket=socket,
        )

        assert os.environ["DOCKER_HOST"] == f"unix://{socket}"

    def test_non_rootless_context_is_ignored(self, tmp_path):
        socket = tmp_path / "rootless.sock"
        socket.touch()

        _ensure_docker_host_for_rootless_context(
            docker_config=_config(tmp_path, json.dumps({"currentContext": "default"})),
            rootless_socket=socket,
        )

        assert "DOCKER_HOST" not in os.environ
