"""Exercise hash failure propagation through the real signing CLI with a GPG test double."""

import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[3] / "tools" / "sign-skill.sh"


def run(args: list[str], env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args, env=env, capture_output=True, text=True, timeout=60, check=False
    )


@pytest.fixture
def signing_env(tmp_path: Path) -> dict[str, str]:
    for tool in ("bash", "jq", "sha256sum", "shasum"):
        if not shutil.which(tool):
            pytest.skip(f"missing required tool: {tool}")
    env = os.environ.copy()
    env.pop("GPG_PRIVATE_KEY", None)
    env.pop("GPG_PASSPHRASE", None)
    env.update(LC_ALL="C", LANG="C", GPG_CALL_LOG=str(tmp_path / "gpg-calls"))
    return env


def isolated_path(tmp_path: Path, engine: str) -> str:
    """Hide sha256sum for the shasum case; fail even when running as root."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for tool in (
        "bash",
        "jq",
        "awk",
        "hostname",
        "dirname",
        "basename",
        "find",
        "sort",
        "date",
        "mkdir",
        "mktemp",
        "mv",
        "rm",
        "sed",
        "grep",
        "head",
        "stat",
    ):
        executable = shutil.which(tool)
        assert executable, tool
        (bin_dir / tool).symlink_to(executable)
    # Record signing attempts without accessing the developer's GPG keyring.
    gpg = bin_dir / "gpg"
    gpg.write_text(
        "#!/bin/bash\n"
        'if [[ "$*" == *--list-secret-keys* ]]; then\n'
        '  echo "sec:test"; exit 0\n'
        "fi\n"
        'if [[ "$*" != *--detach-sign* ]]; then exit 2; fi\n'
        'printf "sign\\n" >> "$GPG_CALL_LOG"\n'
        "while [[ $# -gt 0 ]]; do\n"
        '  if [[ "$1" == --output ]]; then\n'
        '    printf "test-signature\\n" > "$2"; exit 0\n'
        "  fi\n"
        "  shift\n"
        "done\n"
        "exit 2\n"
    )
    gpg.chmod(0o755)
    wrapper = bin_dir / engine
    wrapper.write_text(
        "#!/bin/bash\n"
        'if [[ "${FAIL_HASH:-0}" == 1 ]]; then\n'
        '  printf "%064d  partial-output\\n" 0\n'
        '  echo "injected hash failure" >&2\n'
        "  exit 42\n"
        "fi\n"
        f'exec "{shutil.which(engine)}" "$@"\n'
    )
    wrapper.chmod(0o755)
    return str(bin_dir)


@pytest.mark.parametrize("engine", ["sha256sum", "shasum"])
@pytest.mark.parametrize("batch", [False, True], ids=["single", "batch"])
@pytest.mark.parametrize("existing", [False, True], ids=["new", "force"])
def test_hash_failure_never_signs(
    tmp_path: Path,
    signing_env: dict[str, str],
    engine: str,
    batch: bool,
    existing: bool,
) -> None:
    parent = tmp_path / "skills"
    skill = parent / "example"
    skill.mkdir(parents=True)
    payload = skill / "hello world.txt"
    payload.write_text("skill payload\n")
    env = dict(signing_env, PATH=isolated_path(tmp_path, engine))
    args = ["bash", str(SCRIPT)]
    args += ["--batch", str(parent)] if batch else [str(skill)]
    meta = skill / ".skill-meta"
    manifest = meta / "Manifest.json"
    signature = meta / ".skill.sig"
    originals = None
    if existing:
        result = run(args, env)
        assert result.returncode == 0, result.stdout + result.stderr
        originals = (manifest.read_bytes(), signature.read_bytes())
        args += ["--force"]
    calls = Path(env["GPG_CALL_LOG"])
    before = calls.read_bytes() if calls.exists() else b""
    result = run(args, dict(env, FAIL_HASH="1"))
    assert (calls.read_bytes() if calls.exists() else b"") == before
    assert result.returncode != 0, result.stdout + result.stderr
    assert "Failed to hash file:" in result.stderr
    if batch:
        assert "0/1 skills signed successfully" in result.stdout
    if originals:
        assert (manifest.read_bytes(), signature.read_bytes()) == originals
    else:
        assert not signature.exists()
        assert not manifest.exists()
    assert not list(meta.glob("Manifest.json.*"))


@pytest.mark.parametrize("engine", ["sha256sum", "shasum"])
@pytest.mark.parametrize("batch", [False, True], ids=["single", "batch"])
def test_success_signs_correct_hash(
    tmp_path: Path, signing_env: dict[str, str], engine: str, batch: bool
) -> None:
    parent = tmp_path / "skills"
    skill = parent / "example"
    skill.mkdir(parents=True)
    payload = skill / "hello world.txt"
    payload.write_text("skill payload\n")
    env = dict(signing_env, PATH=isolated_path(tmp_path, engine))
    args = ["bash", str(SCRIPT)]
    args += ["--batch", str(parent)] if batch else [str(skill)]
    result = run(args, env)
    assert result.returncode == 0, result.stdout + result.stderr
    meta = skill / ".skill-meta"
    manifest = meta / "Manifest.json"
    data = json.loads(manifest.read_text())
    assert data["files"] == [
        {"path": payload.name, "hash": hashlib.sha256(payload.read_bytes()).hexdigest()}
    ]
    assert (meta / ".skill.sig").read_text() == "test-signature\n"
    assert Path(env["GPG_CALL_LOG"]).read_text() == "sign\n"
