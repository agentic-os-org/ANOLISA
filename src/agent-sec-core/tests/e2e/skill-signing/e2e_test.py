#!/usr/bin/env python3
"""Pytest E2E tests for skill signing and verification.

The default tests exercise the source-tree ``sign-skill.sh`` against temporary
skills, trusted keys, and verifier config.  When a source-build installation is
detected, the installed-path test also runs the user workflow:

  /usr/local/bin/sign-skill.sh --init
  /usr/local/bin/sign-skill.sh --batch /usr/share/anolisa/skills --force
  agent-sec-cli verify
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional

import pytest

# ---------------------------------------------------------------------------
# Path and import resolution
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parents[3]  # agent-sec-core/
PROJECT_ROOT = Path(__file__).resolve().parents[5]  # repository root
SIGN_SKILL_SH = REPO_ROOT / "tools" / "sign-skill.sh"
SOURCE_PYTHONPATH = REPO_ROOT / "agent-sec-cli" / "src"

SIGNING_DIR = ".skill-meta"

# Prefer the source-tree package, even when pytest is launched from the
# installed source-build venv or an RPM test environment.
sys.path.insert(0, str(SOURCE_PYTHONPATH))

from agent_sec_cli.asset_verify.errors import (  # noqa: E402
    ErrHashMismatch,
    ErrSigInvalid,
    ErrSigMissing,
    ErrUnexpectedFile,
)
from agent_sec_cli.asset_verify.verifier import (  # noqa: E402
    load_trusted_keys,
    verify_skill,
)


@dataclass
class Workspace:
    """Shared source-tree signing workspace."""

    root: Path
    gnupg_home: Path
    trusted_keys: Path
    skills_dir: Path
    config_file: Path

    def env(self, extra: Optional[dict[str, str]] = None) -> dict[str, str]:
        env = os.environ.copy()
        env["GNUPGHOME"] = str(self.gnupg_home)
        env["LC_ALL"] = "C"
        env["LANG"] = "C"
        if extra:
            env.update(extra)
        return env


def require_tools(*tools: str) -> None:
    missing = [tool for tool in tools if shutil.which(tool) is None]
    if missing:
        pytest.skip(f"missing required tool(s): {', '.join(missing)}")


def require_passwordless_sudo() -> None:
    sudo_bin = shutil.which("sudo")
    if not sudo_bin:
        pytest.skip("installed paths require sudo, but sudo is not available")

    probe = run_command([sudo_bin, "-n", "true"], timeout=10)
    if probe.returncode != 0:
        pytest.skip("installed paths require sudo, but sudo -n is not available")


def run_command(
    args: Iterable[str | Path],
    *,
    cwd: Optional[Path] = None,
    env: Optional[dict[str, str]] = None,
    input_text: Optional[str] = None,
    timeout: int = 120,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(arg) for arg in args],
        capture_output=True,
        text=True,
        cwd=str(cwd) if cwd else None,
        env=env,
        input=input_text,
        timeout=timeout,
    )


def run_sign_skill(
    args: list[str],
    *,
    ws: Optional[Workspace] = None,
    env_extra: Optional[dict[str, str]] = None,
    script: Path = SIGN_SKILL_SH,
    timeout: int = 120,
) -> subprocess.CompletedProcess:
    """Run sign-skill.sh with an isolated environment when a workspace is given."""
    env = ws.env(env_extra) if ws else os.environ.copy()
    return run_command(["bash", script, *args], env=env, timeout=timeout)


def run_maybe_sudo(
    args: Iterable[str | Path],
    *,
    env: Optional[dict[str, str]] = None,
    sudo: bool = False,
    timeout: int = 120,
) -> subprocess.CompletedProcess:
    cmd = [str(arg) for arg in args]
    run_env = os.environ.copy()
    if env:
        run_env.update(env)

    if sudo and os.geteuid() != 0:
        sudo_bin = shutil.which("sudo")
        if not sudo_bin:
            pytest.fail("sudo is required for installed skill signing e2e")
        preserved = [
            f"{key}={run_env[key]}"
            for key in ("GNUPGHOME", "PATH", "LC_ALL", "LANG")
            if key in run_env
        ]
        cmd = [sudo_bin, "-n", "env", *preserved, *cmd]
        run_env = os.environ.copy()

    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=run_env,
        timeout=timeout,
    )


def resolve_installed_asset_verify_dir() -> Optional[Path]:
    """Resolve installed verifier package data without importing agent_sec_cli."""
    search_roots = [
        Path("/opt/agent-sec/venv/lib"),
        Path("/opt/agent-sec/lib"),
    ]

    for root in search_roots:
        if not root.is_dir():
            continue
        for asset_dir in sorted(
            root.glob("python*/site-packages/agent_sec_cli/asset_verify")
        ):
            if asset_dir.is_dir() and (asset_dir / "config.conf").is_file():
                return asset_dir

    return None


def make_skill(parent: Path, name: str, files: dict[str, str]) -> Path:
    """Create a fake skill directory with the given files."""
    skill_dir = parent / name
    for rel_path, content in files.items():
        path = skill_dir / rel_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    return skill_dir


def assert_success(result: subprocess.CompletedProcess, context: str) -> None:
    assert result.returncode == 0, (
        f"{context} failed with exit {result.returncode}\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )


@pytest.fixture(scope="module")
def signing_ws(tmp_path_factory: pytest.TempPathFactory) -> Workspace:
    """Initialize one isolated source-tree signing workspace for the module."""
    require_tools("gpg", "jq")
    assert SIGN_SKILL_SH.exists(), f"{SIGN_SKILL_SH} not found"

    tmp_root = Path(os.environ.get("ANOLISA_E2E_TMPDIR", "/tmp"))
    if not tmp_root.is_dir():
        tmp_root = tmp_path_factory.mktemp("e2e_sign_root")
    root = Path(tempfile.mkdtemp(prefix="agent-sec-e2e-sign-", dir=tmp_root))
    gnupg_home = root / "gnupg"
    gnupg_home.mkdir(mode=0o700)
    trusted_keys = root / "trusted-keys"
    trusted_keys.mkdir()
    skills_dir = root / "skills"
    skills_dir.mkdir()
    config_file = root / "config.conf"
    config_file.write_text("skills_dir = [\n]\n")

    ws = Workspace(
        root=root,
        gnupg_home=gnupg_home,
        trusted_keys=trusted_keys,
        skills_dir=skills_dir,
        config_file=config_file,
    )

    result = run_sign_skill(
        ["--init", "--trusted-keys-dir", str(ws.trusted_keys)],
        ws=ws,
    )
    assert_success(result, "--init")

    try:
        yield ws
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_reports_prerequisites() -> None:
    """--check should report all prerequisites OK."""
    require_tools("gpg", "jq")
    result = run_sign_skill(["--check"])
    assert_success(result, "--check")
    assert "All prerequisites satisfied" in result.stdout + result.stderr


def test_init_exports_public_key(signing_ws: Workspace) -> None:
    """The module fixture should generate and export a signing public key."""
    asc_files = list(signing_ws.trusted_keys.glob("*.asc"))
    assert asc_files, f"No .asc in {signing_ws.trusted_keys}"
    assert asc_files[0].stat().st_size > 0, "Exported .asc is empty"


def test_single_sign_and_verify(signing_ws: Workspace) -> None:
    """Sign a single skill, then verify with the verifier module."""
    skill = make_skill(
        signing_ws.skills_dir,
        "skill-a",
        {
            "main.py": "print('hello')\n",
            "README.md": "# Skill A\n",
        },
    )

    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "single sign")

    signing = skill / SIGNING_DIR
    assert (signing / "Manifest.json").exists(), ".skill-meta/Manifest.json missing"
    assert (signing / ".skill.sig").exists(), ".skill-meta/.skill.sig missing"

    manifest = json.loads((signing / "Manifest.json").read_text())
    paths_in_manifest = {file_entry["path"] for file_entry in manifest["files"]}
    assert "main.py" in paths_in_manifest
    assert "README.md" in paths_in_manifest
    assert "Manifest.json" not in paths_in_manifest
    assert ".skill.sig" not in paths_in_manifest
    assert not [path for path in paths_in_manifest if path.startswith(".skill-meta")]

    keys = load_trusted_keys(signing_ws.trusted_keys)
    ok, name = verify_skill(str(skill), keys)
    assert ok
    assert name == "skill-a"


def test_batch_sign_registers_explicit_config_and_verifies(
    signing_ws: Workspace,
) -> None:
    """Batch-sign multiple skills and register only the temporary config."""
    batch_root = signing_ws.root / "batch_skills"
    batch_root.mkdir()
    for skill_name, content in [("alpha", "A"), ("beta", "B"), ("gamma", "C")]:
        make_skill(batch_root, skill_name, {"data.txt": content})

    result = run_sign_skill(
        [
            "--batch",
            str(batch_root),
            "--force",
            "--config-file",
            str(signing_ws.config_file),
        ],
        ws=signing_ws,
    )
    assert_success(result, "batch sign")
    assert "3/3" in result.stdout
    assert str(batch_root.resolve()) in signing_ws.config_file.read_text()

    keys = load_trusted_keys(signing_ws.trusted_keys)
    for skill_name in ("alpha", "beta", "gamma"):
        ok, name = verify_skill(str(batch_root / skill_name), keys)
        assert ok, f"verify_skill failed for {skill_name}"
        assert name == skill_name


def test_legacy_ci_batch_invocation_with_private_key(signing_ws: Workspace) -> None:
    """Package-source CI's historical --batch call remains compatible."""
    archive_skills = signing_ws.root / "tmp_build" / "anolisa-ci" / "skills"
    archive_skills.mkdir(parents=True)
    for skill_name, content in [("ci-alpha", "A"), ("ci-beta", "B")]:
        make_skill(archive_skills, skill_name, {"SKILL.md": f"# {content}\n"})

    ci_key_home = signing_ws.root / "ci_key_gpg"
    ci_key_home.mkdir(mode=0o700)
    env = signing_ws.env()
    generate = run_command(
        ["gpg", "--homedir", ci_key_home, "--batch", "--gen-key"],
        env=env,
        input_text=(
            "Key-Type: RSA\n"
            "Key-Length: 2048\n"
            "Name-Real: CI Signing Key\n"
            "Name-Email: ci-sign@test.local\n"
            "Expire-Date: 0\n"
            "%no-protection\n"
            "%commit\n"
        ),
    )
    assert_success(generate, "generate CI signing key")

    private_key = run_command(
        [
            "gpg",
            "--homedir",
            ci_key_home,
            "--armor",
            "--export-secret-keys",
            "ci-sign@test.local",
        ],
        env=env,
    )
    assert_success(private_key, "export CI private key")

    public_key = run_command(
        ["gpg", "--homedir", ci_key_home, "--armor", "--export", "ci-sign@test.local"],
        env=env,
    )
    assert_success(public_key, "export CI public key")
    ci_trusted_keys = signing_ws.root / "ci_trusted_keys"
    ci_trusted_keys.mkdir()
    (ci_trusted_keys / "ci-sign.asc").write_text(public_key.stdout)

    blank_home = signing_ws.root / "legacy_ci_gpg"
    blank_home.mkdir(mode=0o700)
    ci_env = signing_ws.env(
        {
            "GNUPGHOME": str(blank_home),
            "GPG_PRIVATE_KEY": private_key.stdout,
        }
    )

    installed_asset_verify_dir = resolve_installed_asset_verify_dir()
    installed_config = (
        installed_asset_verify_dir / "config.conf"
        if installed_asset_verify_dir is not None
        else None
    )
    original_installed_config = (
        installed_config.read_text()
        if installed_config is not None
        and installed_config.is_file()
        and os.access(installed_config, os.W_OK)
        else None
    )

    try:
        result = run_command(
            [
                "bash",
                "src/agent-sec-core/tools/sign-skill.sh",
                "--batch",
                archive_skills,
            ],
            cwd=PROJECT_ROOT,
            env=ci_env,
            timeout=180,
        )
        assert_success(result, "legacy CI batch invocation")
        assert "GPG private key imported and trusted" in result.stdout + result.stderr
        assert "2/2 skills signed successfully" in result.stdout + result.stderr

        keys = load_trusted_keys(ci_trusted_keys)
        for skill_name in ("ci-alpha", "ci-beta"):
            skill_dir = archive_skills / skill_name
            assert (skill_dir / SIGNING_DIR / "Manifest.json").is_file()
            assert (skill_dir / SIGNING_DIR / ".skill.sig").is_file()
            ok, name = verify_skill(str(skill_dir), keys)
            assert ok
            assert name == skill_name
    finally:
        if original_installed_config is not None and installed_config is not None:
            installed_config.write_text(original_installed_config)


def test_force_overwrite(signing_ws: Workspace) -> None:
    """--force overwrites existing manifest and signature."""
    skill = make_skill(signing_ws.skills_dir, "skill-force", {"f.txt": "v1"})

    first = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(first, "initial sign")
    sig1 = (skill / SIGNING_DIR / ".skill.sig").read_text()

    (skill / "f.txt").write_text("v2")
    second = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(second, "re-sign")
    sig2 = (skill / SIGNING_DIR / ".skill.sig").read_text()

    assert sig1 != sig2, "Signature should differ after content change"

    keys = load_trusted_keys(signing_ws.trusted_keys)
    ok, _ = verify_skill(str(skill), keys)
    assert ok


def test_no_force_rejects_existing(signing_ws: Workspace) -> None:
    """Without --force, existing manifest/sig blocks signing."""
    skill = make_skill(signing_ws.skills_dir, "skill-noforce", {"x.txt": "x"})

    first = run_sign_skill([str(skill)], ws=signing_ws)
    assert_success(first, "initial sign")

    second = run_sign_skill([str(skill)], ws=signing_ws)
    assert second.returncode != 0, "Expected non-zero exit without --force"
    assert "already exists" in second.stdout + second.stderr


def test_no_secret_key_error_is_actionable(signing_ws: Workspace) -> None:
    """Signing without a secret key should fail before creating .skill.sig."""
    blank_home = signing_ws.root / "no_key_gpg"
    blank_home.mkdir(mode=0o700)
    skill = make_skill(signing_ws.skills_dir, "skill-no-key", {"x.txt": "x"})

    result = run_sign_skill(
        [str(skill), "--force"],
        ws=signing_ws,
        env_extra={"GNUPGHOME": str(blank_home)},
    )

    assert result.returncode != 0, "Expected signing to fail without a secret key"
    combined = result.stdout + result.stderr
    assert "No GPG secret key" in combined
    assert "--init" in combined
    assert "GPG_PRIVATE_KEY" in combined
    assert not (skill / SIGNING_DIR / ".skill.sig").exists()


def test_export_key_to_custom_dir(signing_ws: Workspace) -> None:
    """--export-key exports to a specified directory."""
    custom_dir = signing_ws.root / "custom_keys"
    result = run_sign_skill(["--export-key", str(custom_dir)], ws=signing_ws)
    assert_success(result, "--export-key custom")
    asc_files = list(custom_dir.glob("*.asc"))
    assert asc_files, f"No .asc in {custom_dir}"


def test_skill_name_override(signing_ws: Workspace) -> None:
    """--skill-name overrides the skill name in the manifest."""
    skill = make_skill(signing_ws.skills_dir, "skill-rename", {"a.txt": "a"})
    result = run_sign_skill(
        [str(skill), "--skill-name", "custom-name", "--force"],
        ws=signing_ws,
    )
    assert_success(result, "skill name override")

    manifest = json.loads((skill / SIGNING_DIR / "Manifest.json").read_text())
    assert manifest["skill_name"] == "custom-name"


def test_hidden_files_excluded(signing_ws: Workspace) -> None:
    """Hidden files and directories are excluded from the manifest."""
    skill = make_skill(
        signing_ws.skills_dir,
        "skill-hidden",
        {
            "visible.txt": "ok",
            ".hidden_file": "secret",
            ".hidden_dir/inner.txt": "secret2",
        },
    )
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "hidden file sign")

    manifest = json.loads((skill / SIGNING_DIR / "Manifest.json").read_text())
    paths = {file_entry["path"] for file_entry in manifest["files"]}
    assert "visible.txt" in paths
    assert ".hidden_file" not in paths
    assert ".hidden_dir/inner.txt" not in paths
    assert not [path for path in paths if path.startswith(".skill-meta")]


def test_tampered_file_detected(signing_ws: Workspace) -> None:
    """Verifier detects file content tampering after signing."""
    skill = make_skill(
        signing_ws.skills_dir, "skill-tamper", {"payload.txt": "original"}
    )
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "tamper setup sign")

    (skill / "payload.txt").write_text("TAMPERED")

    keys = load_trusted_keys(signing_ws.trusted_keys)
    with pytest.raises(ErrHashMismatch):
        verify_skill(str(skill), keys)


def test_unsigned_reference_file_detected(signing_ws: Workspace) -> None:
    """Verifier detects new files added under references after signing."""
    skill = make_skill(
        signing_ws.skills_dir,
        "skill-extra-file",
        {
            "SKILL.md": "# Skill\n",
            "references/original.md": "signed\n",
        },
    )
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "extra file setup sign")

    (skill / "references" / "a.md").write_text("")

    keys = load_trusted_keys(signing_ws.trusted_keys)
    with pytest.raises(ErrUnexpectedFile) as exc_info:
        verify_skill(str(skill), keys)
    assert "references/a.md" in str(exc_info.value)


def test_missing_sig_detected(signing_ws: Workspace) -> None:
    """Verifier raises ErrSigMissing when .skill.sig is deleted."""
    skill = make_skill(signing_ws.skills_dir, "skill-nosig", {"f.txt": "f"})
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "missing sig setup sign")

    (skill / SIGNING_DIR / ".skill.sig").unlink()

    keys = load_trusted_keys(signing_ws.trusted_keys)
    with pytest.raises(ErrSigMissing):
        verify_skill(str(skill), keys)


def test_wrong_key_rejected(signing_ws: Workspace) -> None:
    """Signature made with key A is rejected when verified with key B only."""
    alt_dir = signing_ws.root / "alt_gpg"
    alt_dir.mkdir(mode=0o700)
    alt_keys = signing_ws.root / "alt_keys"
    alt_keys.mkdir()
    env = signing_ws.env()

    generate = run_command(
        ["gpg", "--homedir", alt_dir, "--batch", "--gen-key"],
        env=env,
        input_text=(
            "Key-Type: RSA\n"
            "Key-Length: 2048\n"
            "Name-Real: Alt Key\n"
            "Name-Email: alt@test.local\n"
            "Expire-Date: 0\n"
            "%no-protection\n"
            "%commit\n"
        ),
    )
    assert_success(generate, "generate alt key")

    export_alt = run_command(
        ["gpg", "--homedir", alt_dir, "--armor", "--export", "alt@test.local"],
        env=env,
    )
    assert_success(export_alt, "export alt public key")
    alt_pub = alt_keys / "alt.asc"
    alt_pub.write_text(export_alt.stdout)
    assert alt_pub.stat().st_size > 0, "Failed to export alt public key"

    skill = make_skill(signing_ws.skills_dir, "skill-wrongkey", {"z.txt": "z"})
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "wrong key setup sign")

    alt_trusted = load_trusted_keys(alt_keys)
    with pytest.raises(ErrSigInvalid):
        verify_skill(str(skill), alt_trusted)


def test_gpg_private_key_env(signing_ws: Workspace) -> None:
    """GPG_PRIVATE_KEY env var import + signing works end-to-end."""
    env_dir = signing_ws.root / "env_gpg"
    env_dir.mkdir(mode=0o700)
    env = signing_ws.env()

    generate = run_command(
        ["gpg", "--homedir", env_dir, "--batch", "--gen-key"],
        env=env,
        input_text=(
            "Key-Type: RSA\n"
            "Key-Length: 2048\n"
            "Name-Real: Env Key\n"
            "Name-Email: env@test.local\n"
            "Expire-Date: 0\n"
            "%no-protection\n"
            "%commit\n"
        ),
    )
    assert_success(generate, "generate env key")

    private_key = run_command(
        [
            "gpg",
            "--homedir",
            env_dir,
            "--armor",
            "--export-secret-keys",
            "env@test.local",
        ],
        env=env,
    )
    assert_success(private_key, "export env private key")
    assert len(private_key.stdout) > 100, "Private key export was unexpectedly short"

    env_keys = signing_ws.root / "env_keys"
    env_keys.mkdir()
    public_key = run_command(
        ["gpg", "--homedir", env_dir, "--armor", "--export", "env@test.local"],
        env=env,
    )
    assert_success(public_key, "export env public key")
    (env_keys / "env.asc").write_text(public_key.stdout)

    blank_home = signing_ws.root / "blank_gpg"
    blank_home.mkdir(mode=0o700)

    skill = make_skill(signing_ws.skills_dir, "skill-envkey", {"e.txt": "env"})
    result = run_sign_skill(
        [str(skill), "--force"],
        ws=signing_ws,
        env_extra={
            "GNUPGHOME": str(blank_home),
            "GPG_PRIVATE_KEY": private_key.stdout,
        },
    )
    assert_success(result, "GPG_PRIVATE_KEY sign")
    assert "imported and trusted" in result.stdout + result.stderr

    env_trusted = load_trusted_keys(env_keys)
    ok, _ = verify_skill(str(skill), env_trusted)
    assert ok


def test_manifest_structure(signing_ws: Workspace) -> None:
    """Manifest JSON has the expected schema fields."""
    skill = make_skill(
        signing_ws.skills_dir,
        "skill-schema",
        {
            "script.sh": "#!/bin/bash\necho hi\n",
        },
    )
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "manifest schema sign")

    manifest = json.loads((skill / SIGNING_DIR / "Manifest.json").read_text())
    for key in ("version", "skill_name", "algorithm", "created_at", "files"):
        assert key in manifest, f"Missing field '{key}' in manifest"
    assert manifest["version"] == "0.1"
    assert manifest["algorithm"] == "SHA256"
    assert manifest["skill_name"] == "skill-schema"
    assert len(manifest["files"]) == 1
    assert manifest["files"][0]["path"] == "script.sh"
    assert len(manifest["files"][0]["hash"]) == 64


def test_subdirectory_files(signing_ws: Workspace) -> None:
    """Files in nested subdirectories are included in the manifest."""
    skill = make_skill(
        signing_ws.skills_dir,
        "skill-nested",
        {
            "top.txt": "top",
            "sub/deep.txt": "deep",
            "sub/deeper/leaf.txt": "leaf",
        },
    )
    result = run_sign_skill([str(skill), "--force"], ws=signing_ws)
    assert_success(result, "nested files sign")

    manifest = json.loads((skill / SIGNING_DIR / "Manifest.json").read_text())
    paths = {file_entry["path"] for file_entry in manifest["files"]}
    assert paths == {"top.txt", "sub/deep.txt", "sub/deeper/leaf.txt"}

    keys = load_trusted_keys(signing_ws.trusted_keys)
    ok, _ = verify_skill(str(skill), keys)
    assert ok


def test_source_build_installed_signing_and_verify() -> None:
    """Sign installed source-build skills and verify through agent-sec-cli."""
    require_tools("gpg", "jq")

    installed_script = Path(
        os.environ.get("ANOLISA_INSTALLED_SIGN_SKILL", "/usr/local/bin/sign-skill.sh")
    )
    skills_root = Path(
        os.environ.get("ANOLISA_INSTALLED_SKILLS_DIR", "/usr/share/anolisa/skills")
    )
    asset_verify_dir = resolve_installed_asset_verify_dir()
    agent_sec_cli = shutil.which("agent-sec-cli") or "/usr/local/bin/agent-sec-cli"

    if not installed_script.exists() or asset_verify_dir is None:
        pytest.skip(
            "source-build installed sign-skill.sh and verifier asset paths are not present"
        )

    assert installed_script.exists(), f"missing installed script: {installed_script}"
    assert skills_root.is_dir(), f"missing installed skills root: {skills_root}"
    assert Path(
        agent_sec_cli
    ).exists(), f"missing agent-sec-cli binary: {agent_sec_cli}"

    trusted_keys_dir = asset_verify_dir / "trusted-keys"
    config_file = asset_verify_dir / "config.conf"
    assert config_file.is_file(), f"missing verifier config: {config_file}"

    expected_skills = {"code-scanner", "prompt-scanner", "skill-ledger"}
    for skill_name in expected_skills:
        assert (
            skills_root / skill_name
        ).is_dir(), f"missing installed skill: {skill_name}"

    trusted_keys_write_target = (
        trusted_keys_dir if trusted_keys_dir.exists() else trusted_keys_dir.parent
    )
    needs_sudo = os.geteuid() != 0 and (
        not os.access(skills_root, os.W_OK)
        or not os.access(trusted_keys_write_target, os.W_OK)
        or not os.access(config_file, os.W_OK)
    )
    if needs_sudo and os.geteuid() != 0:
        require_passwordless_sudo()

    gnupg_home = Path(
        tempfile.mkdtemp(prefix="agent-sec-pytest-gnupg-", dir=tempfile.gettempdir())
    )
    env = {
        "GNUPGHOME": str(gnupg_home),
        "LC_ALL": "C",
        "LANG": "C",
        "PATH": os.environ.get("PATH", ""),
    }

    try:
        if needs_sudo and os.geteuid() != 0:
            setup_home = run_maybe_sudo(
                ["sh", "-c", f"rm -rf '{gnupg_home}' && mkdir -m 700 '{gnupg_home}'"],
                sudo=True,
            )
            assert_success(setup_home, "create root GNUPGHOME")
        else:
            gnupg_home.chmod(0o700)

        init = run_maybe_sudo(
            ["bash", installed_script, "--init"],
            env=env,
            sudo=needs_sudo,
            timeout=180,
        )
        assert_success(init, "installed --init")
        assert str(trusted_keys_dir) in init.stdout + init.stderr
        assert (
            trusted_keys_dir.is_dir()
        ), f"trusted-keys dir was not created: {trusted_keys_dir}"

        batch = run_maybe_sudo(
            ["bash", installed_script, "--batch", skills_root, "--force"],
            env=env,
            sudo=needs_sudo,
            timeout=180,
        )
        assert_success(batch, "installed --batch")
        assert "3/3 skills signed successfully" in batch.stdout + batch.stderr

        verify = run_command([agent_sec_cli, "verify"], timeout=180)
        assert_success(verify, "agent-sec-cli verify")
        assert "VERIFICATION PASSED" in verify.stdout
        for skill_name in expected_skills:
            assert f"[OK] {skill_name}" in verify.stdout
            assert (skills_root / skill_name / SIGNING_DIR / "Manifest.json").is_file()
            assert (skills_root / skill_name / SIGNING_DIR / ".skill.sig").is_file()
    finally:
        if needs_sudo and os.geteuid() != 0:
            run_maybe_sudo(["rm", "-rf", gnupg_home], sudo=True)
        else:
            shutil.rmtree(gnupg_home, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
