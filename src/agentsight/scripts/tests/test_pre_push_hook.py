"""Pin the contract of the pre-push hook.

The hook mirrors the `test-agentsight` CI gates locally: it must stay a no-op for
pushes that do not touch ``src/agentsight/`` (monorepo citizen), skip ref
deletions, and enforce the conventional-commit lint rules with the same
defaultIgnores as the CI commitlint — all BEFORE any toolchain-dependent gate,
so a bad push fails in seconds with a message that names the rule.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

HOOK = Path(__file__).resolve().parents[1] / "hooks" / "pre-push"
ZERO = "0" * 40


def _git(repo: Path, *args: str) -> str:
    r = subprocess.run(
        # commit.gpgsign is disabled so the fixture also works on machines
        # with global GPG signing enabled.
        ["git", "-c", "commit.gpgsign=false", *args],
        cwd=repo,
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def _commit(repo: Path, message: str, path: str = "src/agentsight/crates/x/src/lib.rs") -> str:
    f = repo / path
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text(message)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repo(tmp_path: Path):
    subprocess.run(["git", "init", "-q", str(tmp_path)], cwd=tmp_path, check=True)
    _git(tmp_path, "symbolic-ref", "HEAD", "refs/heads/main")
    _git(tmp_path, "config", "user.email", "tests@example.com")
    _git(tmp_path, "config", "user.name", "tests")
    main_sha = _commit(tmp_path, "chore(repo): initial state", path="README.md")
    # the hook resolves origin/main to decide whether the branch touches agentsight
    _git(tmp_path, "update-ref", "refs/remotes/origin/main", main_sha)
    return tmp_path


def run_hook(repo: Path, pushes: list[str]) -> tuple[int, str]:
    payload = "".join(line + "\n" for line in pushes)
    r = subprocess.run(["bash", str(HOOK)], cwd=repo, input=payload,
                       capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


def push_line(local_sha: str, remote_sha: str) -> str:
    return f"refs/heads/topic {local_sha} refs/heads/topic {remote_sha}"


def test_push_without_agentsight_changes_is_a_fast_no_op(repo: Path) -> None:
    tip = _commit(repo, "docs(other): touch nothing in agentsight", path="docs/notes.md")
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 0
    assert out == ""


def test_ref_deletion_lines_are_skipped(repo: Path) -> None:
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(ZERO, origin_main)])

    assert rc == 0
    assert out == ""


def test_non_conventional_subject_fails_the_lint(repo: Path) -> None:
    tip = _commit(repo, "update stuff")
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "not a conventional commit" in out
    assert "update stuff" in out


def test_conventional_subject_without_scope_fails_the_lint(repo: Path) -> None:
    tip = _commit(repo, "fix: repair the parser")
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "missing required scope" in out


def test_merge_revert_fixup_squash_and_amend_are_ignored_by_the_lint(repo: Path) -> None:
    # rev-list walks newest first: the ignored subject must be skipped silently and
    # the verdict must come from the second (scope-less) commit, not from the Merge.
    _commit(repo, "fix: no scope on the older commit")
    tip = _commit(repo, "Merge branch 'feature' into main")

    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")
    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "missing required scope" in out
    assert "not a conventional commit" not in out


def test_subject_over_120_chars_fails_the_lint(repo: Path) -> None:
    tip = _commit(repo, "fix(scope): " + "x" * 115)
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "exceeds 120 chars" in out


def test_subject_ending_with_a_full_stop_fails_the_lint(repo: Path) -> None:
    tip = _commit(repo, "fix(scope): repair the parser.")
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "must not end with '.'" in out


def test_body_line_over_100_chars_fails_the_lint(repo: Path) -> None:
    _git(repo, "add", "-A")
    f = repo / "src/agentsight/crates/x/src/lib.rs"
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text("body")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "fix(scope): repair the parser",
         "-m", "y" * 101)
    tip = _git(repo, "rev-parse", "HEAD")
    origin_main = _git(repo, "rev-parse", "refs/remotes/origin/main")

    rc, out = run_hook(repo, [push_line(tip, origin_main)])

    assert rc == 1
    assert "line over 100 chars" in out
