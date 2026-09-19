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

"""Tokenless evidence collection for OpenClaw runs.

The filesystem and trace probes live in :mod:`evidence_probes`; what stays here is
the part specific to tokenless, namely the ``rtk`` interception signals and the
stdout markers the plugin prints.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path

from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts
from swe_runner.agents.openclaw.evidence_probes import (
    dir_probe,
    file_probe,
    iter_json_line_objects,
    plugin_config_probe,
    plugin_trajectory_probe,
    read_json_file,
    safe_int,
    string_bool,
)
from swe_runner.agents.openclaw.identifiers import safe_session_component
from swe_runner.run.io.artifacts import RunArtifacts

logger = logging.getLogger(__name__)

_TOKENLESS_PLUGIN_ID = "tokenless"
_TOKENLESS_MARKERS = (
    "[tokenless]",
    "[tokenless:",
    "tokenless:",
)


def _summarize_session_jsonl(path: Path) -> dict[str, object]:
    summary: dict[str, object] = {
        "path": str(path),
        "exists": path.is_file(),
        "line_count": 0,
        "tool_call_count": 0,
        "exec_tool_call_count": 0,
        "tool_result_count": 0,
        "rtk_command_count": 0,
        "sample_exec_commands": [],
    }
    sample_exec_commands: list[str] = []

    for item in iter_json_line_objects(path):
        summary["line_count"] = safe_int(summary["line_count"]) + 1
        message = item.get("message")
        if not isinstance(message, dict):
            continue
        role = message.get("role")
        if role == "toolResult":
            summary["tool_result_count"] = safe_int(summary["tool_result_count"]) + 1
            continue

        content = message.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if not isinstance(part, dict) or part.get("type") != "toolCall":
                continue
            summary["tool_call_count"] = safe_int(summary["tool_call_count"]) + 1
            if part.get("name") != "exec":
                continue
            summary["exec_tool_call_count"] = safe_int(summary["exec_tool_call_count"]) + 1
            arguments = part.get("arguments")
            command = arguments.get("command") if isinstance(arguments, dict) else None
            if isinstance(command, str):
                if command.strip().startswith("rtk "):
                    summary["rtk_command_count"] = safe_int(summary["rtk_command_count"]) + 1
                if len(sample_exec_commands) < 3:
                    sample_exec_commands.append(command[:500])

    summary["sample_exec_commands"] = sample_exec_commands
    return summary


def _raw_output_tokenless_summary(raw_output: str) -> dict[str, object]:
    marker_lines = [line for line in raw_output.splitlines() if any(marker in line for marker in _TOKENLESS_MARKERS)]
    rtk_rewrite_lines = [line for line in raw_output.splitlines() if "[tokenless:rtk] rewrite:" in line]
    response_compression_lines = [
        line
        for line in raw_output.splitlines()
        if "tokenless: response compression" in line or "[tokenless:response" in line
    ]
    return {
        "plugin_registered": "[tokenless] OpenClaw plugin registered" in raw_output,
        "rtk_rewrite": bool(rtk_rewrite_lines),
        "response_compression": bool(response_compression_lines),
        "marker_line_count": len(marker_lines),
        "sample_marker_lines": marker_lines[:5],
        "sample_rtk_rewrite_lines": rtk_rewrite_lines[:3],
        "sample_response_compression_lines": response_compression_lines[:3],
    }


def write_tokenless_evidence(
    *,
    output_dir: Path,
    instance_id: str,
    metadata: dict[str, str],
    raw_output: str,
) -> dict[str, str]:
    """Write tokenless activation evidence and return OpenClaw metadata updates."""
    run_artifacts = RunArtifacts.from_metadata(metadata)
    openclaw_artifacts = OpenClawArtifacts.from_metadata(metadata)
    profile_dir = Path(openclaw_artifacts.openclaw_profile_dir or "")
    config_path = Path(openclaw_artifacts.openclaw_config_path or "")
    workspace_root = Path(openclaw_artifacts.openclaw_workspace_root or "")
    agent_id = run_artifacts.agent_id or ""
    session_id = run_artifacts.session_id or ""
    session_file = profile_dir / "agents" / agent_id / "sessions" / f"{session_id}.jsonl"
    trajectory_file = profile_dir / "agents" / agent_id / "sessions" / f"{session_id}.trajectory.jsonl"
    plugin_extension_dir = profile_dir / "extensions" / _TOKENLESS_PLUGIN_ID
    injection_manifest = workspace_root / ".runner" / "tokenless" / "injection.json"
    tokenless_bin_dir = workspace_root / ".runner" / "tokenless" / "bin"

    config = plugin_config_probe(config_path, _TOKENLESS_PLUGIN_ID)
    plugin_extension = dir_probe(plugin_extension_dir)
    injected = {
        "rtk": file_probe(tokenless_bin_dir / "rtk"),
        "tokenless": file_probe(tokenless_bin_dir / "tokenless"),
    }
    raw = _raw_output_tokenless_summary(raw_output)
    session = _summarize_session_jsonl(session_file)
    trajectory = plugin_trajectory_probe(trajectory_file, _TOKENLESS_PLUGIN_ID)

    sandbox_binaries_present = bool(injected["rtk"]["exists"] and injected["tokenless"]["exists"])
    profile_extension_present = bool(
        plugin_extension["exists"] and (plugin_extension["manifest_exists"] or plugin_extension["package_exists"])
    )
    config_enabled = config.get("entry_enabled") is True
    plugin_loaded = bool(
        raw["plugin_registered"] or trajectory.get("status") == "loaded" or trajectory.get("imported") is True
    )
    hook_seen = bool(raw["rtk_rewrite"] or raw["response_compression"] or session.get("rtk_command_count", 0))
    exec_tool_calls = safe_int(session.get("exec_tool_call_count"))
    strong = bool(
        config_enabled and sandbox_binaries_present and profile_extension_present and plugin_loaded and hook_seen
    )

    evidence = {
        # v2 dropped the redundant ``tokenless_`` prefix from the trajectory and config
        # blocks when those probes became plugin-agnostic; ``plugin_id`` already scopes them.
        "schema_version": 2,
        "instance_id": instance_id,
        "plugin_id": _TOKENLESS_PLUGIN_ID,
        "strong": strong,
        "reasons": {
            "config_enabled": config_enabled,
            "sandbox_binaries_present": sandbox_binaries_present,
            "profile_extension_present": profile_extension_present,
            "plugin_loaded": plugin_loaded,
            "hook_seen": hook_seen,
            "exec_tool_calls": exec_tool_calls,
        },
        "config": config,
        "profile_extension": plugin_extension,
        "injected_binaries": injected,
        "injection_manifest": read_json_file(injection_manifest),
        "raw_output": raw,
        "session": session,
        "trajectory": trajectory,
    }

    evidence_dir = output_dir / "openclaw-tokenless-evidence"
    evidence_path = evidence_dir / f"{safe_session_component(instance_id)}.json"
    evidence_dir.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(evidence, indent=2), encoding="utf-8")

    logger.info(
        "OPENCLAW_TOKENLESS_EVIDENCE instance=%s strong=%s plugin_loaded=%s hook_seen=%s exec_calls=%s file=%s",
        instance_id,
        strong,
        plugin_loaded,
        hook_seen,
        exec_tool_calls,
        evidence_path,
    )

    return {
        "openclaw_tokenless_evidence_path": str(evidence_path),
        "openclaw_tokenless_evidence_strong": string_bool(strong),
        "openclaw_tokenless_plugin_loaded": string_bool(plugin_loaded),
        "openclaw_tokenless_hook_seen": string_bool(hook_seen),
        "openclaw_tokenless_exec_tool_calls": str(exec_tool_calls),
    }
