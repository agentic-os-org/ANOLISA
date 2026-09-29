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

"""Prompt resource paths and loaders."""

from __future__ import annotations

import logging
from pathlib import Path

logger = logging.getLogger(__name__)

SKILL_NAME = "swe-bench-patch-generation"
PACKAGE_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SKILLS_DIR = PACKAGE_ROOT / "skills"
BUILTIN_SKILL_PATH = DEFAULT_SKILLS_DIR / SKILL_NAME / "SKILL.md"

# Directory containing per-instance custom prompt overrides.
CUSTOM_PROMPTS_DIR = PACKAGE_ROOT / "prompts"


def resolve_builtin_skill_path(skills_dir: Path | None = None) -> Path:
    """Resolve the skill file path using the original skill resource layout."""
    root = skills_dir or DEFAULT_SKILLS_DIR
    return root / SKILL_NAME / "SKILL.md"


def resolve_custom_prompt_path(instance_id: str, prompts_dir: Path | None = None) -> Path:
    """Resolve a per-instance prompt path using the original resource naming."""
    root = prompts_dir or CUSTOM_PROMPTS_DIR
    return root / instance_id


def load_custom_prompt(instance_id: str, *, prompts_dir: Path | None = None) -> str | None:
    """Load a custom prompt file for the given instance_id, if it exists."""
    prompt_file = resolve_custom_prompt_path(instance_id, prompts_dir)
    if not prompt_file.is_file():
        logger.warning("CUSTOM_PROMPT_UNAVAILABLE instance=%s file=%s", instance_id, prompt_file)
        return None

    try:
        content = prompt_file.read_text(encoding="utf-8").strip()
        if content:
            logger.info("CUSTOM_PROMPT_LOADED instance=%s file=%s size=%d", instance_id, prompt_file, len(content))
            return content
        logger.info("CUSTOM_PROMPT_EMPTY instance=%s file=%s", instance_id, prompt_file)
        return None
    except OSError as exc:
        logger.warning("CUSTOM_PROMPT_LOAD_FAILED instance=%s file=%s error=%s", instance_id, prompt_file, exc)
        return None


def load_required_custom_prompt(instance_id: str, *, prompts_dir: Path | None = None) -> str:
    """Load a required per-instance custom prompt."""
    prompt_file = resolve_custom_prompt_path(instance_id, prompts_dir)
    if not prompt_file.is_file():
        raise FileNotFoundError(f"Per-case prompt file not found for {instance_id}: {prompt_file}")

    try:
        content = prompt_file.read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise RuntimeError(f"Failed to load per-case prompt for {instance_id}: {prompt_file}") from exc

    if not content:
        raise RuntimeError(f"Per-case prompt file is empty for {instance_id}: {prompt_file}")

    logger.info("CUSTOM_PROMPT_LOADED instance=%s file=%s size=%d", instance_id, prompt_file, len(content))
    return content


def builtin_skill_available(*, skills_dir: Path | None = None) -> bool:
    """Return whether the optional package-local SWE-bench skill is bundled."""
    return resolve_builtin_skill_path(skills_dir).is_file()


def load_builtin_skill_text(*, skills_dir: Path | None = None) -> str:
    """Load the optional package-local SWE-bench skill text."""
    skill_path = resolve_builtin_skill_path(skills_dir)
    if not skill_path.is_file():
        raise FileNotFoundError(f"SWE-bench skill file is not available: {skill_path}")

    try:
        content = skill_path.read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise RuntimeError(f"Failed to load SWE-bench skill from {skill_path}") from exc

    if not content:
        raise RuntimeError(f"SWE-bench skill file is empty: {skill_path}")

    return content


def load_optional_builtin_skill_text(*, skills_dir: Path | None = None) -> str | None:
    """Load optional package-local SWE-bench skill text, if bundled."""
    try:
        return load_builtin_skill_text(skills_dir=skills_dir)
    except (FileNotFoundError, RuntimeError) as exc:
        logger.warning("BUILTIN_SKILL_UNAVAILABLE file=%s error=%s", resolve_builtin_skill_path(skills_dir), exc)
        return None
