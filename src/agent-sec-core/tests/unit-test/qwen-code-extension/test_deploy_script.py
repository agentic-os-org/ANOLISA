"""Tests for the Qwen Code extension deployment script."""

import json
import os
import shutil
import subprocess
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[3]
_EXTENSION_DIR = _ROOT / "qwen-code-extension"
_DEPLOY_SCRIPT = _EXTENSION_DIR / "scripts" / "deploy.sh"
_RPM_SPEC = _ROOT / "agent-sec-core.spec.in"


def _write_executable(path, content):
    path.write_text(content)
    path.chmod(0o755)


def _fake_qwen(path):
    _write_executable(
        path,
        """#!/usr/bin/env python3
import json
import os
import shutil
import sys
from pathlib import Path

args = sys.argv[1:]
log_path = Path(os.environ["QWEN_TEST_LOG"])
with log_path.open("a", encoding="utf-8") as stream:
    stream.write(json.dumps(args) + "\\n")

home = Path(os.environ["QWEN_HOME"])
if args[:2] == ["extensions", "install"]:
    source = Path(args[2]).resolve()
    manifest = json.loads((source / "qwen-extension.json").read_text())
    target = home / "extensions" / manifest["name"]
    shutil.copytree(source, target)
    (target / ".qwen-extension-install.json").write_text(
        json.dumps({"type": "local", "source": str(source)})
    )
elif args[:2] == ["extensions", "update"]:
    target = home / "extensions" / args[2]
    metadata = json.loads((target / ".qwen-extension-install.json").read_text())
    source = Path(metadata["source"])
    shutil.rmtree(target)
    shutil.copytree(source, target)
    (target / ".qwen-extension-install.json").write_text(json.dumps(metadata))
elif args[:2] == ["extensions", "enable"]:
    pass
else:
    raise SystemExit(f"unexpected qwen invocation: {args}")
""",
    )


def _run_deploy(
    tmp_path,
    extension_dir,
    *,
    qwen_available=True,
    agent_sec_cli_available=True,
):
    fake_qwen = tmp_path / "qwen"
    fake_cli = tmp_path / "agent-sec-cli"
    support_bin = tmp_path / "support-bin"
    qwen_home = tmp_path / "qwen-home"
    log_path = tmp_path / "qwen.log"
    if log_path.exists():
        log_path.unlink()
    if qwen_available:
        _fake_qwen(fake_qwen)
    if agent_sec_cli_available:
        _write_executable(fake_cli, "#!/usr/bin/env bash\nexit 0\n")
    support_bin.mkdir(exist_ok=True)
    for command_name in ("bash", "dirname", "python3"):
        command_path = shutil.which(command_name)
        assert command_path is not None
        command_link = support_bin / command_name
        if not command_link.exists():
            command_link.symlink_to(command_path)

    env = os.environ.copy()
    env.update(
        {
            "QWEN_BIN": str(fake_qwen),
            "QWEN_HOME": str(qwen_home),
            "QWEN_TEST_LOG": str(log_path),
            "PATH": os.pathsep.join((str(tmp_path), str(support_bin))),
        }
    )
    result = subprocess.run(
        [str(_DEPLOY_SCRIPT), str(extension_dir)],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    calls = (
        [json.loads(line) for line in log_path.read_text().splitlines()]
        if log_path.exists()
        else []
    )
    return result, calls, qwen_home


def test_fresh_deploy_installs_and_enables_user_scope(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)

    result, calls, qwen_home = _run_deploy(tmp_path, source)

    manifest = json.loads((source / "qwen-extension.json").read_text())
    target = qwen_home / "extensions" / manifest["name"]
    assert result.returncode == 0, result.stderr
    assert calls == [
        ["extensions", "install", str(source), "--consent", "--scope", "user"],
        ["extensions", "enable", "--scope", "user", manifest["name"]],
    ]
    assert json.loads((target / "qwen-extension.json").read_text())["version"] == (
        manifest["version"]
    )


def test_deploy_updates_when_manifest_version_changes(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)
    manifest = json.loads((source / "qwen-extension.json").read_text())
    qwen_home = tmp_path / "qwen-home"
    target = qwen_home / "extensions" / manifest["name"]
    shutil.copytree(source, target)
    installed_manifest = dict(manifest)
    installed_manifest["version"] = "0.7.0"
    (target / "qwen-extension.json").write_text(json.dumps(installed_manifest))
    (target / ".qwen-extension-install.json").write_text(
        json.dumps({"type": "local", "source": str(source)})
    )

    result, calls, _ = _run_deploy(tmp_path, source)

    assert result.returncode == 0, result.stderr
    assert calls == [
        ["extensions", "update", manifest["name"]],
        ["extensions", "enable", "--scope", "user", manifest["name"]],
    ]
    assert json.loads((target / "qwen-extension.json").read_text())["version"] == (
        manifest["version"]
    )


def test_deploy_is_idempotent_at_same_version(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)

    first, _, _ = _run_deploy(tmp_path, source)
    second, calls, _ = _run_deploy(tmp_path, source)

    assert first.returncode == 0
    assert second.returncode == 0, second.stderr
    assert calls == [
        [
            "extensions",
            "enable",
            "--scope",
            "user",
            "agent-sec-core-qwen-code-extension",
        ],
    ]
    assert "is already installed" in second.stdout


def test_deploy_rejects_missing_qwen(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)

    result, calls, qwen_home = _run_deploy(tmp_path, source, qwen_available=False)

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "is not available in PATH" in result.stderr


def test_deploy_rejects_missing_agent_sec_cli(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)

    result, calls, qwen_home = _run_deploy(
        tmp_path,
        source,
        agent_sec_cli_available=False,
    )

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "agent-sec-cli is not available in PATH" in result.stderr


def test_deploy_rejects_missing_skill_ledger_hook(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)
    (source / "hooks" / "skill_ledger_hook.py").unlink()

    result, calls, qwen_home = _run_deploy(tmp_path, source)

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "missing skill-ledger hook" in result.stderr


def test_deploy_rejects_non_executable_skill_ledger_hook(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)
    (source / "hooks" / "skill_ledger_hook.py").chmod(0o644)

    result, calls, qwen_home = _run_deploy(tmp_path, source)

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "skill-ledger hook is not executable" in result.stderr


def test_deploy_rejects_missing_code_scanner_hook(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)
    (source / "hooks" / "code_scanner_hook.py").unlink()

    result, calls, qwen_home = _run_deploy(tmp_path, source)

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "missing code scanner hook" in result.stderr


def test_deploy_rejects_missing_trace_context_helper(tmp_path):
    source = tmp_path / "extension-source"
    shutil.copytree(_EXTENSION_DIR, source)
    (source / "hooks" / "trace_context.py").unlink()

    result, calls, qwen_home = _run_deploy(tmp_path, source)

    assert result.returncode == 1
    assert calls == []
    assert not (qwen_home / "extensions").exists()
    assert "missing trace-context helper" in result.stderr


def test_rpm_marks_skill_ledger_hook_executable():
    spec = _RPM_SPEC.read_text(encoding="utf-8")

    assert (
        "%attr(0755,root,root) "
        "/opt/agent-sec/qwen-code-extension/hooks/skill_ledger_hook.py"
    ) in spec
