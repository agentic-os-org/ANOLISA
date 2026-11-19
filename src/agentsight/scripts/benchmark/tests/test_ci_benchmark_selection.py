"""用本地 Git 改动验证两层 CI 选择，特别是打包入口与重命名。"""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import textwrap

import pytest

WORKFLOW = Path(__file__).resolve().parents[5] / ".github/workflows/ci.yaml"


def run_block(name: str, event: str = "pull_request") -> str:
    source = WORKFLOW.read_text(encoding="utf-8")
    section = source.split(f"      - name: {name}\n", 1)[1].split("\n      - ", 1)[0]
    match = re.search(r"        run: \|\n((?:          [^\n]*(?:\n|$)|\n)+)", section)
    assert match is not None, f"missing run block: {name}"
    block = textwrap.dedent(match.group(1))
    # event 名是工作流固定字段，其他表达式在测试里不启用手动组件覆盖。
    block = block.replace("${{ github.event_name }}", event)
    return re.sub(r"\$\{\{.*?\}\}", "false", block)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    git(tmp_path, "init", "-q")
    file = tmp_path / "src/agentsight/src/ffi.rs"
    file.parent.mkdir(parents=True)
    file.write_text("// synthetic FFI fixture\n" * 100, encoding="utf-8")
    (file.parent / "lib.rs").write_text("pub mod ffi;\n", encoding="utf-8")
    commit(tmp_path)
    git(tmp_path, "update-ref", "refs/remotes/origin/main", "HEAD")
    return tmp_path


def git(repo: Path, *args: str) -> str:
    # fixture 仓库不沿用宿主机的 hook/template 配置，不涉及真实工作区的分支。
    result = subprocess.run(
        ["git", *args],
        cwd=repo,
        env=dict(
            os.environ,
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_SYSTEM=os.devnull,
            GIT_AUTHOR_NAME="fixture",
            GIT_AUTHOR_EMAIL="fixture@example.invalid",
            GIT_COMMITTER_NAME="fixture",
            GIT_COMMITTER_EMAIL="fixture@example.invalid",
        ),
        capture_output=True,
        text=True,
        check=True,
        timeout=10,
    )
    return result.stdout


def commit(repo: Path) -> None:
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "synthetic fixture")


def selected(
    repo: Path, name: str, *, event: str = "pull_request", base: str = "main"
) -> dict[str, str]:
    output = repo / "github-output"
    output.write_text("", encoding="utf-8")
    result = subprocess.run(
        ["bash", "-e", "-o", "pipefail", "-c", run_block(name, event)],
        cwd=repo,
        env=dict(
            os.environ,
            BASE_REF=base,
            GITHUB_OUTPUT=str(output),
            GITHUB_STEP_SUMMARY=str(repo / "github-summary"),
        ),
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    return dict(line.split("=", 1) for line in output.read_text().splitlines())


@pytest.mark.parametrize(
    "path, outer, inner",
    [
        ("src/agentsight/packaging/build.mk", True, True),
        ("src/agentsight/packaging/install.mk", True, True),
        (".github/actions/package-source/action.yaml", True, True),
        ("src/agentsight/scripts/benchmark/single_run/load/agents.py", True, True),
        ("src/agentsight/src/ffi.rs", True, True),
        ("src/agentsight/Cargo.lock", True, True),
        (".github/workflows/ci.yaml", True, True),
        ("src/agentsight/src/server/cache.rs", True, False),
        ("src/agentsight/dashboard/src/App.tsx", True, False),
        ("README.md", False, False),
    ],
)
def test_changed_path_selects_both_layers(repo: Path, path: str, outer: bool, inner: bool) -> None:
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("changed fixture\n", encoding="utf-8")
    commit(repo)
    component = selected(repo, "Check which components changed")
    benchmark = selected(repo, "Decide whether the benchmark Python suite applies")
    assert component["agentsight"] == str(outer).lower()
    assert benchmark["run"] == str(inner).lower()


@pytest.mark.parametrize("destination", ["src/agentsight/src/abi.rs", "moved/abi.rs"])
def test_moved_ffi_keeps_original_path_in_both_layers(repo: Path, destination: str) -> None:
    source = repo / "src/agentsight/src/ffi.rs"
    target = repo / destination
    target.parent.mkdir(parents=True, exist_ok=True)
    source.rename(target)
    (repo / "src/agentsight/src/lib.rs").write_text("pub mod abi;\n", encoding="utf-8")
    commit(repo)
    # 先证实普通 diff 确实把它识别为重命名并只返回目标路径。
    paths = git(repo, "diff", "--name-only", "--find-renames", "origin/main...HEAD").splitlines()
    assert "src/agentsight/src/ffi.rs" not in paths
    assert destination in paths
    assert selected(repo, "Check which components changed")["agentsight"] == "true"
    assert selected(repo, "Decide whether the benchmark Python suite applies")["run"] == "true"


@pytest.mark.parametrize("event", ["push", "workflow_dispatch"])
def test_push_and_manual_keep_full_benchmark_scope(repo: Path, event: str) -> None:
    result = selected(repo, "Decide whether the benchmark Python suite applies", event=event)
    assert result["run"] == "true"


def test_unreadable_diff_does_not_skip_benchmark(repo: Path) -> None:
    result = selected(repo, "Decide whether the benchmark Python suite applies", base="missing")
    assert result["run"] == "true"
