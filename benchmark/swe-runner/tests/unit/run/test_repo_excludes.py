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

from __future__ import annotations

from pathlib import Path

from swe_runner.run.workspace.repo_excludes import REPO_EXCLUDE_RULES, install_repo_exclude_rules


def test_installs_expected_entries_into_info_exclude(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    (repo / ".git" / "info").mkdir(parents=True)

    install_repo_exclude_rules(repo, instance_id="django__django-1")

    exclude = (repo / ".git" / "info" / "exclude").read_text(encoding="utf-8")
    for pattern in REPO_EXCLUDE_RULES:
        assert pattern in exclude, f"expected {pattern!r} in exclude file"


def test_preserves_existing_entries_and_is_idempotent(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    info_dir = repo / ".git" / "info"
    info_dir.mkdir(parents=True)
    exclude_path = info_dir / "exclude"
    exclude_path.write_text("# existing user rule\nmy_private_notes.md\n", encoding="utf-8")

    install_repo_exclude_rules(repo, instance_id="inst")
    first = exclude_path.read_text(encoding="utf-8")
    install_repo_exclude_rules(repo, instance_id="inst")
    second = exclude_path.read_text(encoding="utf-8")

    assert first == second, "second call must be a no-op"
    assert "my_private_notes.md" in first
    assert "build/" in first


def test_skips_silently_when_not_a_git_repo(tmp_path: Path) -> None:
    repo = tmp_path / "not-a-repo"
    repo.mkdir()

    install_repo_exclude_rules(repo, instance_id="inst")

    assert not (repo / ".git").exists()
