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

"""Plugin-agnostic filesystem and trace probes shared by OpenClaw evidence writers.

Evidence answers one question: did the plugin under test actually engage? A run
that silently loaded nothing looks identical to a run that loaded everything
unless each precondition is recorded separately, so these probes always emit a
result rather than raising, and record the path they inspected.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any


def string_bool(value: bool) -> str:
    """Render a bool for the string-only run metadata map."""
    return "true" if value else "false"


def safe_int(value: object, default: int = 0) -> int:
    """Coerce a probe counter back to int after round-tripping through ``object``."""
    return value if isinstance(value, int) else default


def iter_json_line_objects(path: Path) -> Iterator[dict[str, Any]]:
    """Yield JSON objects from a JSONL file, skipping unparsable lines.

    Session and trajectory files are appended to by a live agent, so a truncated
    final line is normal and must not abort evidence collection.
    """
    if not path.is_file():
        return
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(item, dict):
                yield item


def file_probe(path: Path) -> dict[str, object]:
    """Describe a file, distinguishing a missing path from a dangling symlink."""
    return {
        "path": str(path),
        "exists": path.is_file(),
        "is_symlink": path.is_symlink(),
        "realpath": str(path.resolve(strict=False)) if path.exists() or path.is_symlink() else None,
        "size": path.stat().st_size if path.is_file() else None,
    }


def dir_probe(path: Path) -> dict[str, object]:
    """Describe an OpenClaw extension directory and its plugin manifests."""
    return {
        "path": str(path),
        "exists": path.is_dir(),
        "is_symlink": path.is_symlink(),
        "realpath": str(path.resolve(strict=False)) if path.exists() or path.is_symlink() else None,
        "manifest_exists": (path / "openclaw.plugin.json").is_file(),
        "package_exists": (path / "package.json").is_file(),
    }


def read_json_file(path: Path) -> dict[str, object]:
    """Read a JSON object file into a probe result, tolerating absence and corruption."""
    summary: dict[str, object] = {
        "path": str(path),
        "exists": path.is_file(),
        "content": None,
    }
    if not path.is_file():
        return summary
    try:
        content = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return summary
    if isinstance(content, dict):
        summary["content"] = content
    return summary


def plugin_config_probe(config_path: Path, plugin_id: str) -> dict[str, object]:
    """Report how one plugin is declared in a per-instance OpenClaw config.

    ``context_engine_slot`` is included because OpenClaw admits a single context
    engine: a plugin can be enabled yet inert if another plugin holds the slot.
    """
    summary: dict[str, object] = {
        "path": str(config_path),
        "exists": config_path.is_file(),
        "entry_enabled": None,
        "allow_present": None,
        "allow_contains_plugin": None,
        "context_engine_slot": None,
    }
    if not config_path.is_file():
        return summary
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return summary
    if not isinstance(config, dict):
        return summary

    plugins = config.get("plugins")
    if not isinstance(plugins, dict):
        summary["allow_present"] = False
        return summary

    entries = plugins.get("entries")
    entry = entries.get(plugin_id) if isinstance(entries, dict) else None
    if isinstance(entry, dict):
        summary["entry_enabled"] = entry.get("enabled") is True

    allow = plugins.get("allow")
    summary["allow_present"] = "allow" in plugins
    if isinstance(allow, list):
        summary["allow_contains_plugin"] = plugin_id in allow

    slots = plugins.get("slots")
    if isinstance(slots, dict):
        summary["context_engine_slot"] = slots.get("contextEngine")
    return summary


def plugin_trajectory_probe(path: Path, plugin_id: str) -> dict[str, object]:
    """Extract one plugin's load status from an OpenClaw trajectory file.

    This is the agent's own account of what it imported, so it is the strongest
    available signal that a plugin was live rather than merely configured.
    """
    summary: dict[str, object] = {
        "path": str(path),
        "exists": path.is_file(),
        "imported": None,
        "status": None,
        "activated": None,
        "explicitly_enabled": None,
    }

    for item in iter_json_line_objects(path):
        if item.get("type") != "trace.metadata":
            continue
        data = item.get("data")
        plugins = data.get("plugins") if isinstance(data, dict) else None
        imported = plugins.get("importedRuntimePluginIds") if isinstance(plugins, dict) else None
        if isinstance(imported, list):
            summary["imported"] = plugin_id in imported
        entries = plugins.get("entries") if isinstance(plugins, dict) else None
        if not isinstance(entries, list):
            continue
        for entry in entries:
            if not isinstance(entry, dict) or entry.get("id") != plugin_id:
                continue
            summary["status"] = entry.get("status")
            summary["activated"] = entry.get("activated")
            summary["explicitly_enabled"] = entry.get("explicitlyEnabled")
            return summary
    return summary
