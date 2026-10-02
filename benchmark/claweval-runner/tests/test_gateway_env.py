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

"""Unit tests for gateway user-bus environment injection and pre-check."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from ce_runner.infra import _gateway_env, _check_user_bus  # noqa: E402


class TestGatewayEnv:
    def test_sets_defaults_when_missing(self):
        with patch.dict(os.environ, {}, clear=True):
            env = _gateway_env()
        uid = os.getuid()
        assert env["XDG_RUNTIME_DIR"] == f"/run/user/{uid}"
        assert env["DBUS_SESSION_BUS_ADDRESS"] == f"unix:path=/run/user/{uid}/bus"

    def test_preserves_existing_values(self):
        custom = {
            "XDG_RUNTIME_DIR": "/custom/runtime",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/custom/bus",
        }
        with patch.dict(os.environ, custom, clear=True):
            env = _gateway_env()
        assert env["XDG_RUNTIME_DIR"] == "/custom/runtime"
        assert env["DBUS_SESSION_BUS_ADDRESS"] == "unix:path=/custom/bus"

    def test_inherits_other_env_vars(self):
        with patch.dict(os.environ, {"PATH": "/usr/bin", "HOME": "/root"}, clear=True):
            env = _gateway_env()
        assert env["PATH"] == "/usr/bin"
        assert env["HOME"] == "/root"


class TestCheckUserBus:
    def test_raises_when_runtime_dir_missing(self, tmp_path):
        env = {"XDG_RUNTIME_DIR": str(tmp_path / "nonexistent")}
        with pytest.raises(RuntimeError, match="does not exist"):
            _check_user_bus(env)

    def test_raises_when_bus_socket_missing(self, tmp_path):
        runtime_dir = tmp_path / "runtime"
        runtime_dir.mkdir()
        env = {"XDG_RUNTIME_DIR": str(runtime_dir)}
        with pytest.raises(RuntimeError, match="D-Bus socket not found"):
            _check_user_bus(env)

    def test_passes_when_bus_exists(self, tmp_path):
        runtime_dir = tmp_path / "runtime"
        runtime_dir.mkdir()
        (runtime_dir / "bus").touch()
        env = {
            "XDG_RUNTIME_DIR": str(runtime_dir),
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path={runtime_dir}/bus",
        }
        _check_user_bus(env)

    def test_error_message_includes_diagnostics(self, tmp_path):
        env = {
            "XDG_RUNTIME_DIR": str(tmp_path / "gone"),
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/tmp/gone/bus",
        }
        with pytest.raises(RuntimeError) as exc_info:
            _check_user_bus(env)
        msg = str(exc_info.value)
        assert "XDG_RUNTIME_DIR=" in msg
        assert "DBUS_SESSION_BUS_ADDRESS=" in msg
        assert "systemctl --user" in msg
