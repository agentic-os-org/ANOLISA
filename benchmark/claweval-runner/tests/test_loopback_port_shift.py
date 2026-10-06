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

"""Port offsets must apply to every loopback URL spelling.

Batch runs shift every service URL by a per-task port offset. All four
shift sites matched only ``localhost:<port>`` — a task.yaml URL written
with the numeric loopback host (``127.0.0.1:9100``) silently skipped
the shift, so the runner probed the un-shifted port (or another task's
service) while the process listened on the shifted one.
"""

import sys
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestShiftUrl:
    def test_localhost_and_numeric_loopback_both_shift(self):
        from ce_runner.mcp_mock_services import _shift_url

        assert _shift_url("http://localhost:9100/rss/feeds", 10) == "http://localhost:9110/rss/feeds"
        assert _shift_url("http://127.0.0.1:9100/rss/feeds", 10) == "http://127.0.0.1:9110/rss/feeds"

    def test_zero_offset_is_identity(self):
        from ce_runner.mcp_mock_services import _shift_url

        assert _shift_url("http://127.0.0.1:9100/x", 0) == "http://127.0.0.1:9100/x"
        assert _shift_url("http://localhost:9100/x", 0) == "http://localhost:9100/x"

    def test_remote_hosts_untouched(self):
        from ce_runner.mcp_mock_services import _shift_url

        assert _shift_url("http://example.com:9100/x", 10) == "http://example.com:9100/x"


class TestFetchAuditDataShift:
    def test_audit_url_shifts_for_numeric_loopback(self, tmp_path):
        from ce_runner.session_trace_converter import fetch_audit_data

        task = {
            "services": [
                {
                    "name": "svc",
                    "reset_endpoint": "http://127.0.0.1:9100/reset",
                }
            ]
        }
        seen = {}

        class FakeResp:
            def raise_for_status(self):
                pass

            def json(self):
                return {"calls": []}

        def fake_get(url, timeout=None):
            seen["url"] = url
            return FakeResp()

        with mock.patch("ce_runner.session_trace_converter.httpx.get", fake_get):
            audit = fetch_audit_data(task, port_offset=7)

        assert audit == {"svc": {"calls": []}}
        assert seen["url"] == "http://127.0.0.1:9107/audit"
