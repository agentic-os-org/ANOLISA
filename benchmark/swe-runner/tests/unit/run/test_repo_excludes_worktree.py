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

"""Worktree handling tests for ``install_repo_exclude_rules``.

The installer decided "is this a checkout?" with ``(repo_dir / ".git").exists()``
and then created ``.git/info``. In a linked worktree or a submodule ``.git`` is a
file, so the existence check passed and ``mkdir`` raised ``FileExistsError`` out
of the prepare phase, failing that instance before the agent ever ran.
"""

from pathlib import Path

from swe_runner.run.workspace.patch_policy import REPO_EXCLUDE_RULES
from swe_runner.run.workspace.repo_excludes import install_repo_exclude_rules


def test_linked_worktree_git_file_is_left_alone(tmp_path):
    worktree = tmp_path / "worktree"
    worktree.mkdir()
    git_file = worktree / ".git"
    git_file.write_text("gitdir: /repo/.git/worktrees/wt\n", encoding="utf-8")

    install_repo_exclude_rules(worktree, instance_id="django__django-1")

    assert git_file.read_text(encoding="utf-8") == "gitdir: /repo/.git/worktrees/wt\n"
    assert not (worktree / ".git" / "info").exists()


def test_submodule_git_file_is_left_alone(tmp_path):
    submodule = tmp_path / "submodule"
    submodule.mkdir()
    (submodule / ".git").write_text("gitdir: ../.git/modules/sub\n", encoding="utf-8")

    install_repo_exclude_rules(submodule, instance_id="inst")

    assert (submodule / ".git").is_file()


def test_plain_checkout_still_receives_the_rules(tmp_path):
    repo = tmp_path / "repo"
    (repo / ".git" / "info").mkdir(parents=True)

    install_repo_exclude_rules(repo, instance_id="inst")

    exclude = (repo / ".git" / "info" / "exclude").read_text(encoding="utf-8")
    for pattern in REPO_EXCLUDE_RULES:
        assert pattern in exclude


def test_directory_without_git_is_skipped(tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()

    install_repo_exclude_rules(plain, instance_id="inst")

    assert not (plain / ".git").exists()
    assert list(plain.iterdir()) == []
