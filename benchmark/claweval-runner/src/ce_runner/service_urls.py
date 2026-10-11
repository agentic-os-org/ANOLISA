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

"""URL normalization shared by parallel workers and the standalone MCP wrapper."""

from urllib.parse import urlsplit, urlunsplit


def offset_loopback_url(url: str, offset: int) -> str:
    """Shift an explicit loopback authority port while preserving URL payloads."""
    if not offset:
        return url
    parsed = urlsplit(url)
    if parsed.hostname not in {"localhost", "127.0.0.1", "::1"} or parsed.port is None:
        return url
    authority = f"{parsed.netloc.rsplit(':', 1)[0]}:{parsed.port + offset}"
    return urlunsplit(parsed._replace(netloc=authority))
