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

"""Patch extraction helpers."""

from __future__ import annotations

import logging
from pathlib import Path

from swe_runner.run.workspace.git import (
    GitCommandError,
    cached_diff,
    stage_all,
)
from swe_runner.run.workspace.patch_policy import PATCH_DIFF_EXCLUDE_PATHSPECS, clean_model_patch_diff

logger = logging.getLogger(__name__)


def extract_patch(
    work_dir: Path | None = None,
    *,
    instance_id: str = "unknown",
    base_revision: str | None = None,
) -> str | None:
    """Extract a patch from the repository state.

    Guards against known SWE-bench patch-apply pitfalls:
    * Never emits git binary diffs — the evaluator's ``patch`` cannot apply
      them (``git binary diffs are not supported``).
    * Excludes common noise paths (compiled locale files, SQLite databases,
      ``build/`` artefacts, pycache, agent workspace files).
    * Guarantees a trailing newline so ``patch`` does not fail with
      ``patch unexpectedly ends in middle of line``.
    """
    if work_dir is not None:
        try:
            # ``git add -A`` without an explicit pathspec keeps the existing
            # contract of honouring ``.gitignore`` / ``.git/info/exclude`` and
            # avoids noisy ``paths are ignored`` warnings that would flip the
            # exit code to 1 on otherwise-healthy repos.
            stage_all(work_dir)
            # Intentionally NO ``--binary``: produce a text-only diff.
            raw = cached_diff(
                work_dir,
                base_revision=base_revision,
                pathspecs=PATCH_DIFF_EXCLUDE_PATHSPECS,
            )
        except GitCommandError as exc:
            logger.warning(
                "PATCH_EXTRACT_FAILED instance=%s method=git-diff-cached work_dir=%s error=%s",
                instance_id,
                work_dir,
                exc,
            )
        else:
            patch = clean_model_patch_diff(raw)
            logger.info(
                "PATCH_EXTRACT instance=%s method=git-diff-cached found=%s work_dir=%s size=%s base_revision=%s",
                instance_id,
                patch is not None,
                work_dir,
                len(patch) if patch else 0,
                base_revision,
            )
            return patch

    logger.info("PATCH_EXTRACT instance=%s method=none found=false", instance_id)
    return None
