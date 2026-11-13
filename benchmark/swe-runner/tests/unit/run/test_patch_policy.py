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

from swe_runner.run.workspace.patch_policy import (
    PATCH_DIFF_EXCLUDE_PATHSPECS,
    REPO_EXCLUDE_RULES,
    clean_model_patch_diff,
)


def test_policy_keeps_repo_exclude_and_diff_pathspec_rules_in_one_place() -> None:
    assert ".openclaw/" in REPO_EXCLUDE_RULES
    assert "AGENTS.md" in REPO_EXCLUDE_RULES
    assert ":(exclude,glob).openclaw/**" in PATCH_DIFF_EXCLUDE_PATHSPECS
    assert ":(exclude,glob)AGENTS.md" in PATCH_DIFF_EXCLUDE_PATHSPECS


def test_clean_model_patch_diff_returns_none_for_empty_or_filtered_diff() -> None:
    assert clean_model_patch_diff("") is None
    assert (
        clean_model_patch_diff(
            "diff --git a/asset.bin b/asset.bin\n"
            "index 111..222 100644\n"
            "Binary files a/asset.bin and b/asset.bin differ\n"
        )
        is None
    )


def test_clean_model_patch_diff_strips_binary_only_sections_and_keeps_text_sections() -> None:
    cleaned = clean_model_patch_diff(
        "diff --git a/asset.bin b/asset.bin\n"
        "index 111..222 100644\n"
        "Binary files a/asset.bin and b/asset.bin differ\n"
        "diff --git a/foo.py b/foo.py\n"
        "index 111..222 100644\n"
        "--- a/foo.py\n"
        "+++ b/foo.py\n"
        "@@ -1 +1 @@\n"
        "-old\n"
        "+new"
    )

    assert cleaned is not None
    assert cleaned.endswith("\n")
    assert "asset.bin" not in cleaned
    assert "diff --git a/foo.py b/foo.py" in cleaned
    assert "+new" in cleaned


def test_clean_model_patch_diff_drops_new_root_helper_scripts_only() -> None:
    cleaned = clean_model_patch_diff(
        "diff --git a/test_fix.py b/test_fix.py\n"
        "new file mode 100644\n"
        "index 0000000..1111111\n"
        "--- /dev/null\n"
        "+++ b/test_fix.py\n"
        "@@ -0,0 +1 @@\n"
        "+print('temporary')\n"
        "diff --git a/pkg/test_fix.py b/pkg/test_fix.py\n"
        "new file mode 100644\n"
        "index 0000000..1111111\n"
        "--- /dev/null\n"
        "+++ b/pkg/test_fix.py\n"
        "@@ -0,0 +1 @@\n"
        "+print('project file')\n"
    )

    assert cleaned is not None
    assert "diff --git a/test_fix.py b/test_fix.py" not in cleaned
    assert "diff --git a/pkg/test_fix.py b/pkg/test_fix.py" in cleaned


# Git C-quotes a path that holds non-ASCII bytes, a tab, a quote or a newline,
# wrapping the whole a/ and b/ token: diff --git "a/verify_\345\267\245.py" "b/..."
_QUOTED_TOOL_NAME = "verify_\\345\\267\\245\\345\\205\\267.py"  # verify_工具.py
_QUOTED_ACCENT_NAME = "test_\\303\\251t\\303\\251.py"  # test_été.py
_QUOTED_TAB_NAME = "verify_\\tname.py"


def _quoted_new_file_section(name: str) -> str:
    """Return a C-quoted new-file section as git writes it."""
    return (
        f'diff --git "a/{name}" "b/{name}"\n'
        "new file mode 100644\n"
        "index 0000000..1111111\n"
        "--- /dev/null\n"
        f'+++ "b/{name}"\n'
        "@@ -0,0 +1 @@\n"
        "+print('temporary')\n"
    )


def _quoted_modified_section(name: str) -> str:
    return (
        f'diff --git "a/{name}" "b/{name}"\n'
        "index 1111111..2222222 100644\n"
        f'--- "a/{name}"\n'
        f'+++ "b/{name}"\n'
        "@@ -1 +1 @@\n"
        "-old\n"
        "+new\n"
    )


def test_clean_model_patch_diff_drops_c_quoted_new_root_helpers() -> None:
    cleaned = clean_model_patch_diff(_quoted_new_file_section(_QUOTED_TOOL_NAME))

    assert cleaned is None


def test_clean_model_patch_diff_drops_c_quoted_accented_and_tab_helpers() -> None:
    for name in (_QUOTED_ACCENT_NAME, _QUOTED_TAB_NAME):
        assert clean_model_patch_diff(_quoted_new_file_section(name)) is None


def test_clean_model_patch_diff_keeps_c_quoted_nested_helper_and_source() -> None:
    nested = "pkg/" + _QUOTED_TOOL_NAME
    source = "src/m\\303\\263dulo.py"  # src/módulo.py
    cleaned = clean_model_patch_diff(
        _quoted_new_file_section(nested) + _quoted_new_file_section(source)
    )

    assert cleaned is not None
    assert f'diff --git "a/{nested}" "b/{nested}"' in cleaned
    assert f'diff --git "a/{source}" "b/{source}"' in cleaned


def test_clean_model_patch_diff_keeps_c_quoted_modification_of_tracked_helper() -> None:
    cleaned = clean_model_patch_diff(_quoted_modified_section(_QUOTED_TOOL_NAME))

    assert cleaned is not None
    assert "+new" in cleaned


def test_clean_model_patch_diff_drops_only_the_c_quoted_helper_section() -> None:
    kept_section = _quoted_new_file_section("pkg/" + _QUOTED_TOOL_NAME)
    cleaned = clean_model_patch_diff(
        _quoted_new_file_section(_QUOTED_TOOL_NAME) + kept_section
    )

    assert cleaned == kept_section


def test_unquote_git_path_decodes_octal_tab_and_quote_escapes() -> None:
    from swe_runner.run.workspace.patch_policy import _unquote_git_path

    assert _unquote_git_path(f'"{_QUOTED_TOOL_NAME}"') == "verify_工具.py"
    assert _unquote_git_path('"verify_\\tname.py"') == "verify_\tname.py"
    assert _unquote_git_path('"verify_\\"q\\".py"') == 'verify_"q".py'
    assert _unquote_git_path("plain.py") == "plain.py"
