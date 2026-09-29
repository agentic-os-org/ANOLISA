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

"""Per-instance OpenClaw profile preparation for local runs."""

from __future__ import annotations

import hashlib
import json
import logging
import shutil
from dataclasses import dataclass
from pathlib import Path

from swe_runner.agents import AgentEnvironmentError
from swe_runner.agents.openclaw.config import resolve_openclaw_config_path

logger = logging.getLogger(__name__)

_PROFILE_ROOT_DIRNAME = "openclaw-profiles"


@dataclass(frozen=True)
class OpenClawCaseProfile:
    """OpenClaw profile state for one SWE-bench instance."""

    name: str
    directory: Path
    config_path: Path
    link_path: Path


def _safe_profile_component(value: str) -> str:
    chars = [char if char.isalnum() or char in "._-" else "-" for char in value]
    safe = "".join(chars).strip("-._")
    return safe or "instance"


def _profile_name(instance_id: str, profile_dir: Path) -> str:
    safe_instance = _safe_profile_component(instance_id)
    digest = hashlib.sha256(str(profile_dir.resolve()).encode("utf-8")).hexdigest()[:8]
    return f"swebench-{safe_instance[:48]}-{digest}"


class OpenClawCaseProfileManager:
    """Create a run-local OpenClaw profile and expose it via OpenClaw's profile path."""

    def __init__(
        self,
        *,
        output_dir: Path,
        base_config_path: Path | None = None,
        profile_link_root: Path | None = None,
    ) -> None:
        self._output_dir = output_dir.expanduser().resolve()
        self._base_config_path = base_config_path
        self._profile_link_root = profile_link_root or Path.home()

    def prepare(self, instance_id: str) -> OpenClawCaseProfile:
        profile_dir = self._output_dir / _PROFILE_ROOT_DIRNAME / _safe_profile_component(instance_id)
        profile_name = _profile_name(instance_id, profile_dir)
        link_path = self._profile_link_root / f".openclaw-{profile_name}"
        config_path = profile_dir / "openclaw.json"

        self._reset_profile_dir(profile_dir)
        self._copy_base_config(config_path)
        self._ensure_profile_link(link_path, profile_dir)

        logger.info(
            "OPENCLAW_PROFILE_READY instance=%s profile=%s dir=%s link=%s",
            instance_id,
            profile_name,
            profile_dir,
            link_path,
        )
        return OpenClawCaseProfile(
            name=profile_name,
            directory=profile_dir,
            config_path=config_path,
            link_path=link_path,
        )

    def cleanup_link(self, profile: OpenClawCaseProfile) -> None:
        link_path = profile.link_path
        if not link_path.is_symlink():
            return
        if link_path.resolve(strict=False) != profile.directory.resolve(strict=False):
            return
        link_path.unlink()
        logger.info("OPENCLAW_PROFILE_LINK_REMOVED profile=%s link=%s", profile.name, link_path)

    def _reset_profile_dir(self, profile_dir: Path) -> None:
        if profile_dir.exists():
            shutil.rmtree(profile_dir, ignore_errors=True)
        profile_dir.mkdir(parents=True, exist_ok=True)

    def _copy_base_config(self, target_config_path: Path) -> None:
        source_config_path = resolve_openclaw_config_path(self._base_config_path)
        if source_config_path.exists():
            shutil.copy2(source_config_path, target_config_path)
            return

        target_config_path.write_text(json.dumps({}, indent=2), encoding="utf-8")
        logger.warning("OPENCLAW_BASE_CONFIG_MISSING path=%s using_empty_config=true", source_config_path)

    def _ensure_profile_link(self, link_path: Path, profile_dir: Path) -> None:
        if link_path.is_symlink():
            if link_path.resolve(strict=False) == profile_dir.resolve(strict=False):
                return
            raise AgentEnvironmentError(f"OpenClaw profile link already exists for another target: {link_path}")
        if link_path.exists():
            raise AgentEnvironmentError(f"OpenClaw profile path already exists and is not a symlink: {link_path}")
        link_path.symlink_to(profile_dir, target_is_directory=True)
