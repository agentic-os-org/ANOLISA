import os
import shutil
import subprocess
from pathlib import Path

COMPONENT_ROOT = Path(__file__).resolve().parents[2]


def create_sandbox(tmp_path: Path) -> tuple[Path, Path, dict[str, str]]:
    sandbox = tmp_path / "agent-sec-core"
    fake_bin = sandbox / "fake-bin"
    fake_bin.mkdir(parents=True)
    (sandbox / "agent-sec-cli").mkdir()
    shutil.copy2(COMPONENT_ROOT / "Makefile", sandbox / "Makefile")

    env = os.environ.copy()
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    return sandbox, fake_bin, env


def write_executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def run_make(
    sandbox: Path,
    env: dict[str, str],
    target: str,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["make", target, "BUILD_DIR=build"],
        cwd=sandbox,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def install_fake_pip(fake_bin: Path, env: dict[str, str], marker: Path) -> None:
    env["PIP_MARKER"] = str(marker)
    write_executable(
        fake_bin / "pip3",
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        'printf "%s\\n" "$@" > "$PIP_MARKER"\n',
    )


def test_build_cli_replaces_stale_source_and_staged_wheels(tmp_path: Path) -> None:
    sandbox, fake_bin, env = create_sandbox(tmp_path)
    source_wheels = sandbox / "agent-sec-cli/target/wheels"
    staged_wheels = sandbox / "build/wheels"
    source_wheels.mkdir(parents=True)
    staged_wheels.mkdir(parents=True)

    stale_name = "agent_sec_cli-0.7.1-cp311-cp311-linux_aarch64.whl"
    current_name = "agent_sec_cli-0.8.0-cp311-cp311-linux_aarch64.whl"
    (source_wheels / stale_name).touch()
    (staged_wheels / stale_name).touch()

    write_executable(
        fake_bin / "uv",
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        "if [[ ${1:-} == run ]]; then\n"
        "    mkdir -p target/wheels\n"
        f"    touch target/wheels/{current_name}\n"
        "fi\n",
    )

    result = run_make(sandbox, env, "build-cli")

    assert result.returncode == 0, result.stderr
    assert [path.name for path in source_wheels.glob("agent_sec_cli-*.whl")] == [
        current_name
    ]
    assert [path.name for path in staged_wheels.glob("agent_sec_cli-*.whl")] == [
        current_name
    ]


def test_install_cli_passes_the_only_project_wheel_to_pip(tmp_path: Path) -> None:
    sandbox, fake_bin, env = create_sandbox(tmp_path)
    staged_wheels = sandbox / "build/wheels"
    marker = sandbox / "pip-args"
    staged_wheels.mkdir(parents=True)
    wheel = staged_wheels / "agent_sec_cli-0.8.0-cp311-cp311-linux_aarch64.whl"
    wheel.touch()
    (staged_wheels / "dependency-1.0.0-py3-none-any.whl").touch()
    install_fake_pip(fake_bin, env, marker)

    result = run_make(sandbox, env, "install-cli")

    assert result.returncode == 0, result.stderr
    assert marker.read_text(encoding="utf-8").splitlines() == [
        "install",
        str(wheel.relative_to(sandbox)),
    ]


def test_install_cli_rejects_missing_project_wheel(tmp_path: Path) -> None:
    sandbox, fake_bin, env = create_sandbox(tmp_path)
    marker = sandbox / "pip-called"
    (sandbox / "build/wheels").mkdir(parents=True)
    install_fake_pip(fake_bin, env, marker)

    result = run_make(sandbox, env, "install-cli")

    assert result.returncode != 0
    assert "expected exactly one agent-sec-cli wheel" in result.stderr
    assert not marker.exists()


def test_install_cli_rejects_conflicting_project_wheels(tmp_path: Path) -> None:
    sandbox, fake_bin, env = create_sandbox(tmp_path)
    staged_wheels = sandbox / "build/wheels"
    marker = sandbox / "pip-called"
    staged_wheels.mkdir(parents=True)
    (staged_wheels / "agent_sec_cli-0.7.1-cp311-cp311-linux_aarch64.whl").touch()
    (staged_wheels / "agent_sec_cli-0.8.0-cp311-cp311-linux_aarch64.whl").touch()
    install_fake_pip(fake_bin, env, marker)

    result = run_make(sandbox, env, "install-cli")

    assert result.returncode != 0
    assert "expected exactly one agent-sec-cli wheel" in result.stderr
    assert not marker.exists()
