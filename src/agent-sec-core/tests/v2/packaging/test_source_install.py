"""Source-install lifecycle contracts: build/install pairing and ownership."""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
REPO_ROOT = ROOT.parents[1]


def _run(args, *, check=False):
    return subprocess.run(
        args,
        cwd=ROOT,
        check=check,
        capture_output=True,
        text=True,
    )


def _make_args(tmp_path, *, build_dir=None):
    prefix = tmp_path / "prefix"
    args = [
        "make",
        f"PREFIX={prefix}",
        f"BUILD_DIR={build_dir if build_dir is not None else tmp_path / 'build'}",
    ]
    return args, prefix


def _install_v1_runtime(tmp_path):
    args, prefix = _make_args(tmp_path)
    bindir = prefix / "bin"
    venv = prefix / "lib/anolisa/sec-core/venv"
    bindir.mkdir(parents=True)
    (venv / "bin").mkdir(parents=True)
    for name in ("agent-sec-cli", "agent-sec-daemon"):
        (venv / "bin" / name).write_text("#!/bin/sh\n")
        (bindir / name).symlink_to(venv / "bin" / name)
    sandbox = bindir / "linux-sandbox"
    sandbox.write_text("#!/bin/sh\n")
    return args, bindir, venv, sandbox


def test_default_build_target_builds_the_v2_runtime():
    result = _run(["make", "-n", "build-all"], check=True)
    assert "cargo build --release" in result.stdout
    assert "agent-sec-daemon-health" in result.stdout
    assert "maturin build" not in result.stdout


def test_legacy_removal_requires_every_v2_artifact(tmp_path):
    args, bindir, venv, sandbox = _install_v1_runtime(tmp_path)

    result = _run([*args, "remove-legacy-source-v1-runtime"])

    assert result.returncode != 0
    assert "missing V2 artifact" in result.stderr
    assert (bindir / "agent-sec-cli").is_symlink()
    assert (bindir / "agent-sec-daemon").is_symlink()
    assert venv.is_dir()
    assert sandbox.is_file()


def test_legacy_removal_replaces_v1_runtime_when_v2_artifacts_exist(tmp_path):
    args, bindir, venv, sandbox = _install_v1_runtime(tmp_path)
    build_dir = tmp_path / "build"
    (build_dir / "v2/bin").mkdir(parents=True)
    for name in (
        "agent-sec-cli",
        "agent-sec-daemon",
        "agent-sec-daemon-health",
    ):
        (build_dir / "v2/bin" / name).write_text("")
    (build_dir / "linux-sandbox").write_text("")

    _run([*args, "remove-legacy-source-v1-runtime"], check=True)

    assert not (bindir / "agent-sec-cli").exists()
    assert not (bindir / "agent-sec-daemon").exists()
    assert not venv.exists()
    assert not sandbox.exists()


def test_user_uninstall_leaves_the_shared_host_runtime_alone():
    result = _run(
        [
            "bash",
            str(REPO_ROOT / "scripts/build-all.sh"),
            "--dry-run",
            "--component",
            "sec-core",
            "--uninstall",
        ],
        check=True,
    )

    assert "uninstall-source-v2-assets INSTALL_PROFILE=user" in result.stdout
    assert "uninstall-source-v2-sandbox INSTALL_PROFILE=user" in result.stdout
    assert "uninstall-source-v2-host" not in result.stdout
    assert "uninstall-all-source-v2 INSTALL_PROFILE=system" not in result.stdout


def test_system_uninstall_removes_the_host_runtime():
    result = _run(
        [
            "bash",
            str(REPO_ROOT / "scripts/build-all.sh"),
            "--dry-run",
            "--system",
            "--component",
            "sec-core",
            "--uninstall",
        ],
        check=True,
    )

    assert "uninstall-all-source-v2 INSTALL_PROFILE=system" in result.stdout
