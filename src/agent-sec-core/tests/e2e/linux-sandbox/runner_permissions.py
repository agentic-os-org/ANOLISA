"""Check helper discovery across noexec mounts and private runner directories."""

import json
import os
import shutil
import subprocess
from pathlib import Path


def make_fixture_executable(directory: Path) -> None:
    """Enable fixture execution only on a run-owned mount in the private namespace."""
    subprocess.run(["mount", "--bind", str(directory), str(directory)], check=True)
    subprocess.run(["mount", "-o", "remount,bind,exec", str(directory)], check=True)


def verify_runner_permissions(root: Path, helper: Path) -> None:
    """Reproduce PATH fallback and user-namespace DAC checks with the real helper."""
    runner, fixtures = root / "runner-home", root / "runner-fixtures"
    for directory in (runner, fixtures):
        directory.mkdir(mode=0o700)
        make_fixture_executable(directory)
        shutil.copy2(helper, directory / "linux-sandbox")
    os.chown(runner, 1001, 1001)
    search_path = f"{fixtures}:{runner}:/usr/bin:/bin"

    def probe() -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bwrap",
                "--unshare-user",
                "--ro-bind",
                "/",
                "/",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--",
                shutil.which("linux-sandbox", path=search_path),
                "--capabilities",
            ],
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

    for mode in (0o755, 0o700):
        runner.chmod(mode)
        for executable in (True, False):
            option = "exec" if executable else "noexec"
            subprocess.run(
                ["mount", "-o", f"remount,bind,{option}", str(fixtures)],
                check=True,
            )
            selected = shutil.which("linux-sandbox", path=search_path)
            expected = (fixtures if executable else runner) / "linux-sandbox"
            assert selected == str(expected), (selected, expected)
            result = probe()
            if executable or mode == 0o755:
                assert result.returncode == 0, result
                assert json.loads(result.stdout)["strict_filesystem"], result
            else:
                assert result.returncode != 0, result
                assert f"execvp {expected}: Permission denied" in result.stderr, result

    make_fixture_executable(fixtures)
    assert shutil.which("linux-sandbox", path=search_path) == str(
        fixtures / "linux-sandbox"
    )
    result = probe()
    assert result.returncode == 0, result
    assert runner.stat().st_uid == 1001 and runner.stat().st_mode & 0o777 == 0o700
    print(
        "PASS helper discovery: noexec fallback, runner 0700 denial and fixture repair",
        flush=True,
    )
