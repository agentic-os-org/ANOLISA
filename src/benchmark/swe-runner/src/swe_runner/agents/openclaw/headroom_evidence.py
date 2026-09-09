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

"""Headroom context-engine evidence collection for OpenClaw runs.

Headroom emits no markers on stdout, so unlike the tokenless collector this one
cannot corroborate activation from ``raw_output``. Agent-side state -- an enabled
plugin entry, an installed extension, the exclusive context-engine slot -- proves
only that the arm was configured; an arm can satisfy all of it and still route
nowhere near the proxy, in which case it is a second baseline wearing the label
of a treatment. Activation therefore requires proxy-side counters to have moved.

The agent's own trajectory would be the natural corroboration, but OpenClaw
2026.8.2 writes no ``.trajectory.jsonl`` at all (its runtime events live in
SQLite, where the ``plugins`` payload arrives pre-truncated). That probe is kept
for older profiles and reported as unobservable rather than false when the file
is absent, because a missing probe is not a negative result.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path

from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts
from swe_runner.agents.openclaw.evidence_probes import (
    dir_probe,
    iter_json_line_objects,
    plugin_config_probe,
    plugin_trajectory_probe,
    safe_int,
    string_bool,
)
from swe_runner.agents.openclaw.identifiers import safe_session_component
from swe_runner.run.io.artifacts import RunArtifacts

logger = logging.getLogger(__name__)

_UNOBSERVABLE = "unobservable"

_HEADROOM_PLUGIN_ID = "headroom"
_CONTEXT_ENGINE_SLOT_VALUE = "headroom"
# Declared in the plugin manifest under contracts.tools; its presence in a session
# is independent proof the plugin reached tool registration.
_HEADROOM_TOOL_NAME = "headroom_retrieve"


def _summarize_session_jsonl(path: Path) -> dict[str, object]:
    summary: dict[str, object] = {
        "path": str(path),
        "exists": path.is_file(),
        "line_count": 0,
        "tool_call_count": 0,
        "headroom_tool_call_count": 0,
    }

    for item in iter_json_line_objects(path):
        summary["line_count"] = safe_int(summary["line_count"]) + 1
        message = item.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if not isinstance(part, dict) or part.get("type") != "toolCall":
                continue
            summary["tool_call_count"] = safe_int(summary["tool_call_count"]) + 1
            if part.get("name") == _HEADROOM_TOOL_NAME:
                summary["headroom_tool_call_count"] = safe_int(summary["headroom_tool_call_count"]) + 1

    return summary


def _tristate(value: bool | None) -> str:
    """Render a probe result, keeping "could not observe" distinct from "false"."""
    return _UNOBSERVABLE if value is None else string_bool(value)


def write_headroom_evidence(
    *,
    output_dir: Path,
    instance_id: str,
    metadata: dict[str, str],
    proxy_delta: dict[str, object] | None = None,
) -> dict[str, str]:
    """Write Headroom activation evidence and return OpenClaw metadata updates.

    ``proxy_delta`` comes from :func:`~swe_runner.agents.openclaw.headroom_proxy.
    proxy_compression_delta` and is the only behavioural signal available; without
    it the arm cannot be called active, only configured.
    """
    run_artifacts = RunArtifacts.from_metadata(metadata)
    openclaw_artifacts = OpenClawArtifacts.from_metadata(metadata)
    profile_dir = Path(openclaw_artifacts.openclaw_profile_dir or "")
    config_path = Path(openclaw_artifacts.openclaw_config_path or "")
    agent_id = run_artifacts.agent_id or ""
    session_id = run_artifacts.session_id or ""
    session_file = profile_dir / "agents" / agent_id / "sessions" / f"{session_id}.jsonl"
    trajectory_file = profile_dir / "agents" / agent_id / "sessions" / f"{session_id}.trajectory.jsonl"
    plugin_extension_dir = profile_dir / "extensions" / _HEADROOM_PLUGIN_ID

    config = plugin_config_probe(config_path, _HEADROOM_PLUGIN_ID)
    plugin_extension = dir_probe(plugin_extension_dir)
    trajectory = plugin_trajectory_probe(trajectory_file, _HEADROOM_PLUGIN_ID)
    session = _summarize_session_jsonl(session_file)

    config_enabled = config.get("entry_enabled") is True
    context_engine_active = config.get("context_engine_slot") == _CONTEXT_ENGINE_SLOT_VALUE
    profile_extension_present = bool(
        plugin_extension["exists"] and (plugin_extension["manifest_exists"] or plugin_extension["package_exists"])
    )
    # Absent trajectory file means the probe could not run, not that the plugin
    # failed to load; conflating the two reported every OpenClaw 2026.8.2 run as
    # a load failure while the plugin was demonstrably live.
    plugin_loaded: bool | None
    if not trajectory.get("exists"):
        plugin_loaded = None
    else:
        plugin_loaded = bool(
            trajectory.get("status") == "loaded" or trajectory.get("imported") is True
        )

    delta = proxy_delta if isinstance(proxy_delta, dict) else {}
    proxy_traffic_observed = delta.get("traffic_observed") is True
    proxy_compression_observed = delta.get("compression_observed") is True
    configured = bool(config_enabled and context_engine_active and profile_extension_present)
    strong = bool(configured and proxy_compression_observed)

    evidence = {
        "schema_version": 2,
        "instance_id": instance_id,
        "plugin_id": _HEADROOM_PLUGIN_ID,
        "strong": strong,
        "reasons": {
            "config_enabled": config_enabled,
            "context_engine_active": context_engine_active,
            "profile_extension_present": profile_extension_present,
            "plugin_loaded": plugin_loaded,
            "proxy_traffic_observed": proxy_traffic_observed,
            "proxy_compression_observed": proxy_compression_observed,
            "headroom_tool_call_count": safe_int(session.get("headroom_tool_call_count")),
        },
        "config": config,
        "profile_extension": plugin_extension,
        "trajectory": trajectory,
        "session": session,
        "proxy": delta or None,
    }

    evidence_dir = output_dir / "openclaw-headroom-evidence"
    evidence_path = evidence_dir / f"{safe_session_component(instance_id)}.json"
    evidence_dir.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(evidence, indent=2), encoding="utf-8")

    logger.info(
        "OPENCLAW_HEADROOM_EVIDENCE instance=%s strong=%s configured=%s proxy_traffic=%s "
        "proxy_compressed=%s plugin_loaded=%s file=%s",
        instance_id,
        strong,
        configured,
        proxy_traffic_observed,
        proxy_compression_observed,
        _tristate(plugin_loaded),
        evidence_path,
    )
    if configured and not proxy_traffic_observed:
        # Loud on purpose: this is the shape of an arm that quietly became a
        # duplicate of the baseline, and it is invisible in the run summary.
        logger.error(
            "OPENCLAW_HEADROOM_NO_PROXY_TRAFFIC instance=%s proxy=%s",
            instance_id,
            delta.get("before", {}).get("proxy_url") if isinstance(delta.get("before"), dict) else None,
        )

    return {
        "openclaw_headroom_evidence_path": str(evidence_path),
        "openclaw_headroom_evidence_strong": string_bool(strong),
        "openclaw_headroom_plugin_loaded": _tristate(plugin_loaded),
        "openclaw_headroom_context_engine_active": string_bool(context_engine_active),
        "openclaw_headroom_proxy_traffic_observed": string_bool(proxy_traffic_observed),
        "openclaw_headroom_proxy_compression_observed": string_bool(proxy_compression_observed),
    }
