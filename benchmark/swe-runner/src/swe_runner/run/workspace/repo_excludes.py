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

"""Per-repository exclude rules for runner-generated patch extraction."""

from __future__ import annotations

import logging
from pathlib import Path

from swe_runner.run.workspace.patch_policy import REPO_EXCLUDE_RULES

logger = logging.getLogger(__name__)


def install_repo_exclude_rules(repo_dir: Path, *, instance_id: str) -> None:
    """Append runner patch-exclusion rules into ``.git/info/exclude``.

    SWE-bench evaluation applies the final patch with the plain ``patch``
    command, which cannot handle generated binary diffs, duplicated build
    trees, or agent/tooling side effects. Storing these rules in
    ``.git/info/exclude`` keeps patch extraction clean without modifying the
    upstream repository's tracked ``.gitignore``.
    """
    info_dir = repo_dir / ".git" / "info"
    if not (repo_dir / ".git").exists():
        logger.debug(
            "REPO_EXCLUDE_SKIP_NON_REPO instance=%s work_dir=%s",
            instance_id,
            repo_dir,
        )
        return

    info_dir.mkdir(parents=True, exist_ok=True)
    exclude_path = info_dir / "exclude"
    existing = exclude_path.read_text(encoding="utf-8") if exclude_path.exists() else ""
    existing_lines = {line.strip() for line in existing.splitlines()}
    missing = [line for line in REPO_EXCLUDE_RULES if line not in existing_lines]
    if not missing:
        return

    prefix = "" if not existing or existing.endswith("\n") else "\n"
    exclude_path.write_text(existing + prefix + "\n".join(missing) + "\n", encoding="utf-8")
    logger.info(
        "REPO_EXCLUDE_INSTALLED instance=%s path=%s added=%d",
        instance_id,
        exclude_path,
        len(missing),
    )
