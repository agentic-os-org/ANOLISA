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

import subprocess
from pathlib import Path

from swe_runner.run.workspace.git import get_git_revision
from swe_runner.run.workspace.patches import extract_patch

_SAMPLE_DIFF_FRAGMENT = "diff --git a/foo.py b/foo.py"


def _run_git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=True,
    )


def _init_repo(repo: Path) -> str:
    repo.mkdir(parents=True, exist_ok=True)
    _run_git(repo, "init")
    _run_git(repo, "config", "user.email", "test@example.com")
    _run_git(repo, "config", "user.name", "Test User")
    (repo / "foo.py").write_text("print('old')\n", encoding="utf-8")
    _run_git(repo, "add", "foo.py")
    _run_git(repo, "commit", "-m", "initial")
    revision = get_git_revision(repo)
    assert revision is not None
    return revision


class TestExtractPatch:
    def test_extract_patch_respects_git_exclude_for_openclaw_workspace_files(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        info_dir = tmp_path / ".git" / "info"
        info_dir.mkdir(parents=True, exist_ok=True)
        (info_dir / "exclude").write_text(".openclaw/\nAGENTS.md\n", encoding="utf-8")

        (tmp_path / ".openclaw").mkdir()
        (tmp_path / ".openclaw" / "workspace-state.json").write_text('{"version": 1}\n', encoding="utf-8")
        (tmp_path / "AGENTS.md").write_text("# workspace\n", encoding="utf-8")
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert _SAMPLE_DIFF_FRAGMENT in result
        assert ".openclaw/workspace-state.json" not in result
        assert "AGENTS.md" not in result

    def test_extract_patch_from_git_diff(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert _SAMPLE_DIFF_FRAGMENT in result
        assert "+print('new')" in result

    def test_extract_patch_uses_current_worktree_without_commits(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert _SAMPLE_DIFF_FRAGMENT in result
        assert "+print('new')" in result

    def test_extract_patch_includes_untracked_files(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        (tmp_path / "new_module.py").write_text("VALUE = 1\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert "diff --git a/new_module.py b/new_module.py" in result
        assert "+VALUE = 1" in result

    def test_extract_patch_returns_none_when_clean(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)

        assert extract_patch(work_dir=tmp_path, base_revision=base_revision) is None

    def test_extract_patch_returns_none_when_not_a_repo(self, tmp_path: Path) -> None:
        assert extract_patch(work_dir=tmp_path) is None

    def test_extract_patch_returns_none_when_no_work_dir(self) -> None:
        assert extract_patch() is None

    def test_get_git_revision_returns_none_when_not_a_repo(self, tmp_path: Path) -> None:
        assert get_git_revision(tmp_path) is None

    def test_extract_patch_ends_with_newline(self, tmp_path: Path) -> None:
        """Patch must always end with '\n' so ``patch`` does not bail with
        'unexpectedly ends in middle of line'."""
        base_revision = _init_repo(tmp_path)
        # Write a file whose last line has no trailing newline.
        (tmp_path / "foo.py").write_text("print('new')", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert result.endswith("\n")

    def test_extract_patch_excludes_build_and_binary_paths(self, tmp_path: Path) -> None:
        """``build/**``, ``*.mo`` and ``*.db`` must not end up in the diff."""
        base_revision = _init_repo(tmp_path)
        # Textual change that SHOULD appear.
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")
        # Noise that MUST be filtered out by the pathspec excludes.
        (tmp_path / "build").mkdir()
        (tmp_path / "build" / "lib" / "pkg").mkdir(parents=True)
        (tmp_path / "build" / "lib" / "pkg" / "m.py").write_text("COPY = 1\n", encoding="utf-8")
        (tmp_path / "locale.mo").write_bytes(b"\x00\x00\xde\xad\xbe\xef")
        (tmp_path / "history.db").write_bytes(b"SQLite\x00binary")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert "+print('new')" in result
        assert "build/lib/pkg/m.py" not in result
        assert "locale.mo" not in result
        assert "history.db" not in result

    def test_extract_patch_excludes_tests_docs_config_and_helper_scripts(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")

        (tmp_path / "tests").mkdir()
        (tmp_path / "tests" / "test_foo.py").write_text("def test_new(): pass\n", encoding="utf-8")
        (tmp_path / "test_requests.py").write_text("def test_top_level(): pass\n", encoding="utf-8")
        (tmp_path / "docs").mkdir()
        (tmp_path / "docs" / "usage.rst").write_text("new docs\n", encoding="utf-8")
        (tmp_path / "pyproject.toml").write_text("[project]\nname = 'noise'\n", encoding="utf-8")
        (tmp_path / "package.json").write_text('{"scripts": {"test": "echo noise"}}\n', encoding="utf-8")
        (tmp_path / "test_fix.py").write_text("print('temporary verification')\n", encoding="utf-8")
        (tmp_path / "verify_fix.py").write_text("print('temporary verification')\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert "+print('new')" in result
        assert "tests/test_foo.py" not in result
        assert "test_requests.py" not in result
        assert "docs/usage.rst" not in result
        assert "pyproject.toml" not in result
        assert "package.json" not in result
        assert "test_fix.py" not in result
        assert "verify_fix.py" not in result

    def test_extract_patch_returns_none_when_only_excluded_files_change(self, tmp_path: Path) -> None:
        base_revision = _init_repo(tmp_path)
        (tmp_path / "tests").mkdir()
        (tmp_path / "tests" / "test_foo.py").write_text("def test_new(): pass\n", encoding="utf-8")
        (tmp_path / "pyproject.toml").write_text("[project]\nname = 'noise'\n", encoding="utf-8")
        (tmp_path / "test_fix.py").write_text("print('temporary verification')\n", encoding="utf-8")

        assert extract_patch(work_dir=tmp_path, base_revision=base_revision) is None

    def test_extract_patch_keeps_existing_root_helper_files(self, tmp_path: Path) -> None:
        _init_repo(tmp_path)
        (tmp_path / "run_test.py").write_text("print('old helper')\n", encoding="utf-8")
        _run_git(tmp_path, "add", "run_test.py")
        _run_git(tmp_path, "commit", "-m", "add helper")
        base_revision = get_git_revision(tmp_path)
        assert base_revision is not None

        (tmp_path / "run_test.py").write_text("print('real project helper change')\n", encoding="utf-8")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert "diff --git a/run_test.py b/run_test.py" in result
        assert "+print('real project helper change')" in result

    def test_extract_patch_strips_binary_only_sections(self, tmp_path: Path) -> None:
        """Sections that only contain 'Binary files ... differ' must be dropped
        so the SWE-bench ``patch`` tool does not choke on them."""
        base_revision = _init_repo(tmp_path)
        (tmp_path / "foo.py").write_text("print('new')\n", encoding="utf-8")
        # A non-excluded binary path (no .mo/.db suffix) whose diff will show
        # up as 'Binary files ... differ' and should be stripped.
        (tmp_path / "asset.bin").write_bytes(b"\x00\x01\x02\x03BINARY\xff")

        result = extract_patch(work_dir=tmp_path, base_revision=base_revision)

        assert result is not None
        assert "Binary files" not in result
        assert "asset.bin" not in result
        assert "+print('new')" in result
