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

"""timeout_ms must be enforceable for sync handlers.

Every registered handler is a plain sync function doing SQLite/filesystem
work. asyncio.wait_for cannot preempt a coroutine that never awaits, so a
sync handler that exceeds its advertised timeout used to return success
(long past its budget) while blocking the event loop — stalling every
concurrent connection. Offloading to a worker thread makes the timeout
real.
"""

from __future__ import annotations

import time

from agent_sec_cli.daemon import registry as registry_module
from agent_sec_cli.daemon.protocol import DaemonRequest
from agent_sec_cli.daemon.registry import MethodRegistry, MethodSpec
from agent_sec_cli.daemon.runtime import DaemonRuntime


def _tmp_socket():
    import tempfile
    from pathlib import Path

    return str(Path(tempfile.mkdtemp(prefix="reg-timeout-")) / "daemon.sock")


def _dispatch(monkeypatch, handler, timeout_ms):
    registry = MethodRegistry()
    registry.register(
        MethodSpec(method="m", handler=handler, lifecycle="test", timeout_ms=timeout_ms)
    )
    runtime = DaemonRuntime(socket_path=_tmp_socket())
    import asyncio

    request = DaemonRequest(method="m", request_id="r1")
    return asyncio.run(registry_module.dispatch_request(request, registry, runtime))


class TestSyncHandlerTimeout:
    def test_sync_handler_exceeding_timeout_fails(self, monkeypatch):
        """A sync handler over budget must return the timeout error."""
        monkeypatch.setattr(registry_module, "DEFAULT_TIMEOUT_MS", 100)

        def slow_handler(_request, _runtime):
            time.sleep(0.6)
            return {"too": "late"}

        response = _dispatch(monkeypatch, slow_handler, timeout_ms=100)
        assert response.ok is False
        assert response.error is not None
        assert response.error["code"] == "timeout"

    def test_sync_handler_within_budget_succeeds(self, monkeypatch):
        def fast_handler(_request, _runtime):
            return {"fine": True}

        response = _dispatch(monkeypatch, fast_handler, timeout_ms=5000)
        assert response.ok is True
        assert response.data == {"fine": True}
