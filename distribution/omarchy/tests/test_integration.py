"""Verify integration ownership, configuration and failure paths without a model."""

import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))


def load(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location("omarchy_" + name, ROOT / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def paths(tmp_path: Path) -> dict[str, Path]:
    return {key: tmp_path / key for key in ("prefix", "config", "state", "run")}


@pytest.fixture
def bundle(tmp_path: Path) -> Path:
    root = tmp_path / "bundle"
    payload = root / "payload"
    (payload / "plugin").mkdir(parents=True)
    (payload / "libexec/anolisa/cosh-ng").mkdir(parents=True)
    (payload / "bin").mkdir()
    for name in ("runtime.py", "agent_installation.py", "setup.py", "system_install.py"):
        shutil.copy2(ROOT / name, payload / name)
    (payload / "plugin/manifest.json").write_text('{"id":"community.anolisa"}')
    for name in ("cosh-shell", "cosh-core", "cosh-gateway"):
        (payload / "libexec/anolisa/cosh-ng" / name).write_text("fixture")
    metadata = {
        "schema": 1,
        "platform": "linux-x86_64",
        "version": "test",
        "source_commit": "test-source",
        "files": {
            file.relative_to(payload).as_posix(): hashlib.sha256(file.read_bytes()).hexdigest()
            for file in payload.rglob("*")
            if file.is_file()
        },
    }
    (root / "bundle.json").write_text(json.dumps(metadata))
    return root


def test_checksum_failure_precedes_install(bundle: Path) -> None:
    installer = load("install")
    (bundle / "payload/runtime.py").write_text("unexpected")
    with pytest.raises(ValueError, match="checksum"):
        installer.verify(bundle)


def test_missing_payload_is_rejected(bundle: Path) -> None:
    installer = load("install")
    (bundle / "payload/runtime.py").unlink()
    with pytest.raises(ValueError, match="incomplete"):
        installer.verify(bundle)


def test_symlink_is_rejected(bundle: Path) -> None:
    installer = load("install")
    (bundle / "payload/link").symlink_to("runtime.py")
    with pytest.raises(ValueError, match="symlinks"):
        installer.verify(bundle)


def test_install_can_resume_but_detects_modified_payload(
    bundle: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installer = load("install")
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.delenv("XDG_DATA_HOME", raising=False)
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path / "run"))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    # Keep the real UID so ownership matches the directories created by this test.
    installer.install(bundle, enable=False)
    installed = home / ".local/share/anolisa-omarchy/current/runtime.py"
    first = installed.read_bytes()
    installer.install(bundle, enable=False)
    assert installed.read_bytes() == first
    installed.write_text("manual edit")
    with pytest.raises(ValueError, match="modified"):
        installer.install(bundle, enable=False)


def test_existing_unowned_directory_is_preserved(tmp_path: Path) -> None:
    installer = load("install")
    (tmp_path / "keep").write_text("operator data")
    with pytest.raises(ValueError, match="unowned"):
        installer.require_owned(tmp_path)
    assert (tmp_path / "keep").read_text() == "operator data"


def test_config_has_both_hooks_and_fail_closed_security(paths: dict[str, Path]) -> None:
    runtime = load("runtime")
    spec = runtime.aw_document(paths)["spec"]
    before = spec["events"]["tool.before"]["steps"]
    after = spec["events"]["tool.after"]["steps"]
    assert before[0]["provider"] == "security"
    assert before[0]["on_error"] == "block"
    assert after[-1]["effects"] == ["replace_result"]
    assert spec["execution"]["guarantee"] == "native_hook"
    assert spec["agents"]["qoder"]["argv"] == [str(paths["prefix"] / "bin/qodercli")]


def test_configuration_does_not_overwrite_operator_changes(paths: dict[str, Path]) -> None:
    runtime = load("runtime")
    runtime.configure(paths)
    target = paths["config"] / "aw.json"
    target.write_text('{"operator":"custom"}')
    with pytest.raises(ValueError, match="differs"):
        runtime.configure(paths)
    assert json.loads(target.read_text())["operator"] == "custom"


def test_unavailable_metrics_are_not_reported_as_zero(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    monkeypatch.setattr(runtime, "invoke", lambda *args, **kwargs: None)
    monkeypatch.delenv("QODER_PERSONAL_ACCESS_TOKEN", raising=False)
    report = runtime.status(paths)
    assert report["tokenless"]["available"] is False
    assert report["security"]["available"] is False
    assert report["tokenless"]["saved"] is None


def test_statistics_follow_public_schema(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    store = paths["state"] / "tokenless"
    store.mkdir(parents=True)
    (store / "stats.db").touch()
    events = [
        {"result": "succeeded", "details": {"result": {"verdict": "deny"}}},
        {"result": "failed", "details": {"result": {"verdict": "deny"}}},
    ]
    metrics = {
        "schema_version": "1.1",
        "total": {"records": 3, "before_tokens": 100, "after_tokens": 40, "tokens_saved": 60},
    }
    monkeypatch.setattr(
        runtime, "invoke", lambda command, **kwargs: events if "events" in command else metrics
    )
    monkeypatch.delenv("QODER_PERSONAL_ACCESS_TOKEN", raising=False)
    report = runtime.status(paths)
    assert report["security"]["risks"] == 1
    assert report["security"]["blocked"] is None
    expected = {
        "available": True,
        "saved": 60,
        "before": 100,
        "after": 40,
        "records": 3,
    }
    assert {key: report["tokenless"][key] for key in expected} == expected


def test_public_state_does_not_echo_environment_secret(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    monkeypatch.setenv("QODER_PERSONAL_ACCESS_TOKEN", "private-test-fixture")
    monkeypatch.setattr(runtime, "invoke", lambda *args, **kwargs: None)
    report = runtime.status(paths)
    assert report["authConfigured"] is True
    assert "private-test-fixture" not in json.dumps(report)


def test_agent_does_not_start_when_security_service_is_inactive(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    monkeypatch.setenv("QODER_PERSONAL_ACCESS_TOKEN", "private-test-fixture")
    launched = []

    def inactive(command: list[str], **kwargs: object) -> None:
        raise subprocess.CalledProcessError(3, command)

    monkeypatch.setattr(runtime.subprocess, "run", inactive)
    monkeypatch.setattr(runtime.subprocess, "call", lambda *args, **kwargs: launched.append(args))
    with pytest.raises(subprocess.CalledProcessError):
        runtime.run_agent(paths, [])
    assert not launched


def test_cosh_owns_missing_commands_without_changing_parent_environment(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture
) -> None:
    runtime = load("runtime")
    binary = paths["prefix"] / "bin/cosh"
    binary.parent.mkdir(parents=True)
    binary.write_text(
        "#!/bin/sh\n"
        'printf "%s\\n" "$MISE_NOT_FOUND_AUTO_INSTALL" '
        '"${QODER_PERSONAL_ACCESS_TOKEN-unset}" "$1"\n'
    )
    binary.chmod(0o755)
    monkeypatch.setenv("MISE_NOT_FOUND_AUTO_INSTALL", "1")
    monkeypatch.setenv(runtime.agents.PAT, "private-test-fixture")

    assert runtime.run_cosh(paths, ["--fixture"]) == 0
    assert capfd.readouterr().out.splitlines()[-3:] == ["0", "unset", "--fixture"]
    assert runtime.environment(paths)["MISE_NOT_FOUND_AUTO_INSTALL"] == "1"
    assert runtime.os.environ[runtime.agents.PAT] == "private-test-fixture"


def test_manifest_entrypoints_and_actions_exist() -> None:
    manifest = json.loads((ROOT / "plugin/manifest.json").read_text())
    assert manifest["id"] == "community.anolisa"
    widget = ROOT / "plugin" / manifest["entryPoints"]["barWidget"]
    assert widget.is_file()
    assert 'moduleName: "community.anolisa"' in widget.read_text()
    assert (ROOT / "plugin/bridge.py").is_file()
