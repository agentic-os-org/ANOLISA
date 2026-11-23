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

"""Proxy-side observation of Headroom compression for a single OpenClaw run.

Headroom does its work in a separate proxy process, so the only place its effect
is observable is that proxy's own counters. Everything on the agent side --
config entries, an installed extension, a held context-engine slot -- proves at
most that the arm was *configured*; a misrouted base URL leaves all of those
true while the arm silently degrades to a second baseline. That failure happened
three times before the counters were consulted, which is why activation is
gated on this module rather than on agent-side state alone.

Attribution is per-instance only when instances run sequentially (``--workers
1``): the counters are process-global, so a concurrent run interleaves other
instances into the same delta. Callers running concurrently must treat the delta
as arm-level.
"""

from __future__ import annotations

import json
import logging
import urllib.error
import urllib.request
from pathlib import Path

logger = logging.getLogger(__name__)

_HEADROOM_PLUGIN_ID = "headroom"
_STATS_TIMEOUT_SECONDS = 5.0

# /stats paths pinned against a live proxy (headroom 0.42.x). The CLI help for
# --log-file suggests tokens_before/tokens_after names that the payload does not
# use, so every field read here is one that was observed, not one that was
# documented.
_TOTAL_REQUESTS_PATH = ("requests", "total")
_COMPRESSED_REQUESTS_PATH = ("summary", "compression", "requests_compressed")
_TOKENS_REMOVED_PATH = ("summary", "compression", "total_tokens_removed")
_TOKENS_BEFORE_PATH = ("summary", "compression", "total_tokens_before_with_cli_filtering")


def resolve_proxy_url(config_path: Path) -> str | None:
    """Return the Headroom proxy URL declared in a per-instance OpenClaw config.

    Read from the plugin entry rather than from a runner flag so the probe can
    never disagree with the proxy the arm was actually pointed at.
    """
    if not config_path.is_file():
        return None
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    if not isinstance(config, dict):
        return None

    plugins = config.get("plugins")
    entries = plugins.get("entries") if isinstance(plugins, dict) else None
    entry = entries.get(_HEADROOM_PLUGIN_ID) if isinstance(entries, dict) else None
    entry_config = entry.get("config") if isinstance(entry, dict) else None
    proxy_url = entry_config.get("proxyUrl") if isinstance(entry_config, dict) else None
    if isinstance(proxy_url, str) and proxy_url.strip():
        return proxy_url.strip().rstrip("/")
    return None


def _dig(payload: object, path: tuple[str, ...]) -> int | None:
    current = payload
    for key in path:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current if isinstance(current, int) and not isinstance(current, bool) else None


def fetch_proxy_counters(proxy_url: str) -> dict[str, object]:
    """Snapshot the counters needed to prove compression happened.

    Returns a snapshot that always records ``reachable``; an unreachable proxy is
    reported rather than raised so a probe failure cannot fail the run itself,
    while still being visible as missing activation evidence.
    """
    snapshot: dict[str, object] = {
        "proxy_url": proxy_url,
        "reachable": False,
        "error": None,
        "requests_total": None,
        "requests_compressed": None,
        "tokens_removed": None,
        "tokens_before": None,
    }
    url = f"{proxy_url}/stats"
    try:
        with urllib.request.urlopen(url, timeout=_STATS_TIMEOUT_SECONDS) as response:  # noqa: S310
            payload = json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, OSError, ValueError, json.JSONDecodeError) as exc:
        snapshot["error"] = f"{type(exc).__name__}: {exc}"
        return snapshot

    snapshot["reachable"] = True
    snapshot["requests_total"] = _dig(payload, _TOTAL_REQUESTS_PATH)
    snapshot["requests_compressed"] = _dig(payload, _COMPRESSED_REQUESTS_PATH)
    snapshot["tokens_removed"] = _dig(payload, _TOKENS_REMOVED_PATH)
    snapshot["tokens_before"] = _dig(payload, _TOKENS_BEFORE_PATH)
    return snapshot


def _delta(before: dict[str, object], after: dict[str, object], key: str) -> int | None:
    lhs = after.get(key)
    rhs = before.get(key)
    if not isinstance(lhs, int) or not isinstance(rhs, int):
        return None
    return lhs - rhs


def proxy_compression_delta(
    before: dict[str, object] | None,
    after: dict[str, object] | None,
) -> dict[str, object]:
    """Diff two counter snapshots taken around one agent invocation.

    ``traffic_observed`` and ``compression_observed`` are separated because they
    fail for opposite reasons: no traffic means the arm never reached the proxy,
    whereas traffic without compression means routing worked and compression did
    not. Collapsing them into one boolean is what let a misrouted arm read as a
    plain "no compression" result.
    """
    summary: dict[str, object] = {
        "before": before,
        "after": after,
        "requests_total": None,
        "requests_compressed": None,
        "tokens_removed": None,
        "tokens_before": None,
        "traffic_observed": False,
        "compression_observed": False,
    }
    if not isinstance(before, dict) or not isinstance(after, dict):
        return summary
    if not (before.get("reachable") and after.get("reachable")):
        return summary

    for key in ("requests_total", "requests_compressed", "tokens_removed", "tokens_before"):
        summary[key] = _delta(before, after, key)

    total = summary["requests_total"]
    compressed = summary["requests_compressed"]
    # A negative delta means the proxy restarted mid-instance and its counters
    # reset; that is not evidence of anything, so it must not read as success.
    summary["traffic_observed"] = isinstance(total, int) and total > 0
    summary["compression_observed"] = (
        summary["traffic_observed"] and isinstance(compressed, int) and compressed > 0
    )
    return summary
