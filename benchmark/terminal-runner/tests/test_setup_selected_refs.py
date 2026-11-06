"""Exercise dependency-ref setup against real local repositories without installs."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

SETUP = Path(__file__).resolve().parents[1] / "scripts" / "setup.sh"


def git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *args],
        text=True,
        capture_output=True,
        check=True,
    )
    return result.stdout.strip()


@pytest.fixture
def repositories(tmp_path: Path) -> tuple[Path, Path]:
    origins = []
    for name in ("harbor-origin", "dataset-origin"):
        repository = tmp_path / name
        repository.mkdir()
        git(repository, "init", "-b", "main")
        git(repository, "config", "user.name", "Setup Fixture")
        git(repository, "config", "user.email", "setup@fixture.invalid")
        (repository / "version.txt").write_text("selected\n", encoding="utf-8")
        git(repository, "add", "version.txt")
        git(repository, "commit", "-m", "fixture initial")
        git(repository, "tag", "selected")
        (repository / "version.txt").write_text("latest\n", encoding="utf-8")
        git(repository, "commit", "-am", "fixture latest")
        origins.append(repository)
    return origins[0], origins[1]


@pytest.fixture
def setup_environment(
    tmp_path: Path, repositories: tuple[Path, Path]
) -> tuple[Path, dict[str, str]]:
    root = tmp_path / "runner"
    (root / "scripts").mkdir(parents=True)
    # A Windows checkout can use CRLF; Linux shell fixtures need the Git LF text.
    (root / "scripts" / "setup.sh").write_text(
        SETUP.read_text(encoding="utf-8"), encoding="utf-8"
    )
    (root / ".venv" / "bin").mkdir(parents=True)
    (root / ".venv" / "bin" / "activate").write_text("", encoding="utf-8")
    fake_bin = tmp_path / "fixture-bin"
    fake_bin.mkdir()
    for name, body in (
        ("pip", '#!/bin/sh\nprintf "%s\\n" "$*" >> "$SETUP_INSTALL_LOG"\n'),
        ("git-lfs", "#!/bin/sh\nexit 0\n"),
    ):
        executable = fake_bin / name
        executable.write_text(body, encoding="utf-8")
        executable.chmod(0o755)
    environment = dict(os.environ)
    environment.update(
        {
            "PATH": str(fake_bin) + os.pathsep + environment["PATH"],
            "HARBOR_URL": str(repositories[0]),
            "DATASET_URL": str(repositories[1]),
            "HARBOR_REF": "selected",
            "DATASET_REF": "selected",
            "SETUP_INSTALL_LOG": str(root / "install.log"),
            "GIT_TERMINAL_PROMPT": "0",
        }
    )
    return root, environment


def run_setup(
    root: Path, environment: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(root / "scripts" / "setup.sh")],
        env=environment,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )


def clone_existing(root: Path, repositories: tuple[Path, Path]) -> None:
    for name, origin in zip(("harbor", "dataset"), repositories, strict=True):
        subprocess.run(
            ["git", "clone", "--quiet", str(origin), str(root / name)], check=True
        )


@pytest.mark.parametrize("existing", [False, True])
def test_setup_selects_both_requested_refs(setup_environment, repositories, existing):
    root, environment = setup_environment
    if existing:
        clone_existing(root, repositories)
    result = run_setup(root, environment)
    assert result.returncode == 0, result.stdout + result.stderr
    for name, origin in zip(("harbor", "dataset"), repositories, strict=True):
        assert git(root / name, "rev-parse", "HEAD") == git(
            origin, "rev-parse", "selected"
        )
    assert len((root / "install.log").read_text(encoding="utf-8").splitlines()) == 1


@pytest.mark.parametrize("existing", [False, True])
@pytest.mark.parametrize("variable", ["HARBOR_REF", "DATASET_REF"])
def test_unavailable_ref_fails_before_install(
    setup_environment, repositories, existing, variable
):
    root, environment = setup_environment
    if existing:
        clone_existing(root, repositories)
    environment[variable] = "missing-fixture-ref"
    result = run_setup(root, environment)
    assert result.returncode != 0, result.stdout + result.stderr
    assert "missing-fixture-ref" in result.stdout + result.stderr
    assert "Setup complete" not in result.stdout
    assert not (root / "install.log").exists()


def test_existing_main_dataset_remains_valid(setup_environment, repositories):
    root, environment = setup_environment
    clone_existing(root, repositories)
    environment["DATASET_REF"] = "main"
    result = run_setup(root, environment)
    assert result.returncode == 0, result.stdout + result.stderr
    assert git(root / "dataset", "rev-parse", "HEAD") == git(
        repositories[1], "rev-parse", "main"
    )


def test_cached_refs_work_when_local_origins_are_unavailable(
    setup_environment, repositories
):
    root, environment = setup_environment
    clone_existing(root, repositories)
    for name in ("harbor", "dataset"):
        git(
            root / name,
            "remote",
            "set-url",
            "origin",
            str(root / "missing-local-origin"),
        )
    result = run_setup(root, environment)
    assert result.returncode == 0, result.stdout + result.stderr
    for name, origin in zip(("harbor", "dataset"), repositories, strict=True):
        assert git(root / name, "rev-parse", "HEAD") == git(
            origin, "rev-parse", "selected"
        )


@pytest.mark.parametrize("name", ["harbor", "dataset"])
def test_conflicting_dirty_checkout_preserves_edits_and_stops_install(
    setup_environment, repositories, name
):
    root, environment = setup_environment
    clone_existing(root, repositories)
    changed = root / name / "version.txt"
    changed.write_text("user edits\n", encoding="utf-8")
    previous_head = git(root / name, "rev-parse", "HEAD")
    result = run_setup(root, environment)
    assert result.returncode != 0, result.stdout + result.stderr
    assert "selected" in result.stdout + result.stderr
    assert changed.read_text(encoding="utf-8") == "user edits\n"
    assert git(root / name, "rev-parse", "HEAD") == previous_head
    assert not (root / "install.log").exists()
