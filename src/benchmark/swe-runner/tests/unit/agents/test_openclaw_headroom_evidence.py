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

"""Tests for OpenClaw Headroom evidence collection."""

from __future__ import annotations

import json
from pathlib import Path

from swe_runner.agents.openclaw.headroom_evidence import write_headroom_evidence
from swe_runner.agents.openclaw.headroom_proxy import (
    proxy_compression_delta,
    resolve_proxy_url,
)


def _counters(
    *, total: int, compressed: int, reachable: bool = True
) -> dict[str, object]:
    return {
        "proxy_url": "http://127.0.0.1:8801",
        "reachable": reachable,
        "error": None,
        "requests_total": total,
        "requests_compressed": compressed,
        "tokens_removed": 0,
        "tokens_before": 0,
    }


def _compressing_delta() -> dict[str, object]:
    """A delta showing traffic that actually got compressed."""
    return proxy_compression_delta(
        _counters(total=0, compressed=0), _counters(total=29, compressed=22)
    )


def _prepare_profile(tmp_path: Path, *, context_engine: str | None) -> dict[str, str]:
    """Build a profile that satisfies every activation precondition except the slot."""
    profile_dir = tmp_path / "profile"
    sessions_dir = profile_dir / "agents" / "case-1" / "sessions"
    sessions_dir.mkdir(parents=True)

    extension_dir = profile_dir / "extensions" / "headroom"
    extension_dir.mkdir(parents=True)
    (extension_dir / "openclaw.plugin.json").write_text('{"id":"headroom"}', encoding="utf-8")

    plugins: dict[str, object] = {"entries": {"headroom": {"enabled": True}}}
    if context_engine is not None:
        plugins["slots"] = {"contextEngine": context_engine}
    config_path = profile_dir / "openclaw.json"
    config_path.write_text(json.dumps({"plugins": plugins}), encoding="utf-8")

    (sessions_dir / "session-1.trajectory.jsonl").write_text(
        json.dumps(
            {
                "type": "trace.metadata",
                "data": {
                    "plugins": {
                        "importedRuntimePluginIds": ["headroom"],
                        "entries": [{"id": "headroom", "status": "loaded", "activated": True}],
                    }
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    (sessions_dir / "session-1.jsonl").write_text(
        json.dumps(
            {
                "message": {
                    "role": "assistant",
                    "content": [{"type": "toolCall", "name": "headroom_retrieve", "arguments": {}}],
                }
            }
        )
        + "\n",
        encoding="utf-8",
    )

    return {
        "agent_id": "case-1",
        "session_id": "session-1",
        "openclaw_profile_dir": str(profile_dir),
        "openclaw_config_path": str(config_path),
    }


def test_write_headroom_evidence_records_weak_evidence_when_files_are_missing(tmp_path: Path) -> None:
    metadata = {
        "agent_id": "case-1",
        "session_id": "session-1",
        "openclaw_profile_dir": str(tmp_path / "missing-profile"),
        "openclaw_config_path": str(tmp_path / "missing-config.json"),
    }

    updates = write_headroom_evidence(
        output_dir=tmp_path / "run",
        instance_id="django/django#13448",
        metadata=metadata,
    )

    assert updates["openclaw_headroom_evidence_strong"] == "false"
    assert updates["openclaw_headroom_context_engine_active"] == "false"
    # No trajectory file means the probe never ran; reporting "false" here would
    # read as a load failure that was never observed.
    assert updates["openclaw_headroom_plugin_loaded"] == "unobservable"

    evidence_path = Path(updates["openclaw_headroom_evidence_path"])
    assert evidence_path.name == "django-django-13448.json"
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    assert evidence["strong"] is False
    assert evidence["reasons"] == {
        "config_enabled": False,
        "context_engine_active": False,
        "profile_extension_present": False,
        "plugin_loaded": None,
        "proxy_traffic_observed": False,
        "proxy_compression_observed": False,
        "headroom_tool_call_count": 0,
    }


def test_write_headroom_evidence_records_strong_evidence_when_the_proxy_compressed(
    tmp_path: Path,
) -> None:
    metadata = _prepare_profile(tmp_path, context_engine="headroom")

    updates = write_headroom_evidence(
        output_dir=tmp_path / "run",
        instance_id="case-1",
        metadata=metadata,
        proxy_delta=_compressing_delta(),
    )

    assert updates["openclaw_headroom_evidence_strong"] == "true"
    assert updates["openclaw_headroom_plugin_loaded"] == "true"
    assert updates["openclaw_headroom_context_engine_active"] == "true"
    assert updates["openclaw_headroom_proxy_compression_observed"] == "true"

    evidence = json.loads(Path(updates["openclaw_headroom_evidence_path"]).read_text(encoding="utf-8"))
    assert evidence["reasons"]["headroom_tool_call_count"] == 1
    assert evidence["proxy"]["requests_compressed"] == 22


def test_write_headroom_evidence_is_weak_when_a_fully_configured_arm_sent_no_proxy_traffic(
    tmp_path: Path,
) -> None:
    """The failure that made three arm-C attempts look like plain baseline runs.

    Every agent-side precondition holds, so any config-only check passes, yet no
    request reached the proxy and the arm measured nothing but the baseline.
    """
    metadata = _prepare_profile(tmp_path, context_engine="headroom")

    updates = write_headroom_evidence(
        output_dir=tmp_path / "run",
        instance_id="case-1",
        metadata=metadata,
        proxy_delta=proxy_compression_delta(
            _counters(total=0, compressed=0), _counters(total=0, compressed=0)
        ),
    )

    assert updates["openclaw_headroom_evidence_strong"] == "false"
    assert updates["openclaw_headroom_proxy_traffic_observed"] == "false"


def test_write_headroom_evidence_is_weak_when_traffic_arrived_uncompressed(tmp_path: Path) -> None:
    """Routing working is not compression working; the two must stay separable."""
    metadata = _prepare_profile(tmp_path, context_engine="headroom")

    updates = write_headroom_evidence(
        output_dir=tmp_path / "run",
        instance_id="case-1",
        metadata=metadata,
        proxy_delta=proxy_compression_delta(
            _counters(total=0, compressed=0), _counters(total=29, compressed=0)
        ),
    )

    assert updates["openclaw_headroom_proxy_traffic_observed"] == "true"
    assert updates["openclaw_headroom_proxy_compression_observed"] == "false"
    assert updates["openclaw_headroom_evidence_strong"] == "false"


def test_write_headroom_evidence_is_weak_when_plugin_is_enabled_without_the_slot(tmp_path: Path) -> None:
    """An enabled plugin that does not hold the exclusive slot is inert, not active."""
    metadata = _prepare_profile(tmp_path, context_engine=None)

    updates = write_headroom_evidence(
        output_dir=tmp_path / "run",
        instance_id="case-1",
        metadata=metadata,
        proxy_delta=_compressing_delta(),
    )

    assert updates["openclaw_headroom_context_engine_active"] == "false"
    assert updates["openclaw_headroom_evidence_strong"] == "false"
    # The plugin did load; only the slot claim is missing, and that alone must void the arm.
    assert updates["openclaw_headroom_plugin_loaded"] == "true"


def test_proxy_counter_delta_rejects_a_counter_reset() -> None:
    """A restarted proxy zeroes its counters; a negative delta proves nothing."""
    delta = proxy_compression_delta(
        _counters(total=30, compressed=22), _counters(total=2, compressed=1)
    )

    assert delta["requests_total"] == -28
    assert delta["traffic_observed"] is False
    assert delta["compression_observed"] is False


def test_proxy_counter_delta_is_empty_when_the_proxy_is_unreachable() -> None:
    delta = proxy_compression_delta(
        _counters(total=0, compressed=0, reachable=False),
        _counters(total=29, compressed=22),
    )

    assert delta["traffic_observed"] is False
    assert delta["requests_total"] is None


def test_resolve_proxy_url_reads_the_plugin_entry(tmp_path: Path) -> None:
    config_path = tmp_path / "openclaw.json"
    config_path.write_text(
        json.dumps(
            {
                "plugins": {
                    "entries": {
                        "headroom": {
                            "enabled": True,
                            "config": {"proxyUrl": "http://127.0.0.1:8803/"},
                        }
                    }
                }
            }
        ),
        encoding="utf-8",
    )

    assert resolve_proxy_url(config_path) == "http://127.0.0.1:8803"
    assert resolve_proxy_url(tmp_path / "absent.json") is None
