"""Exercise the new configuration and launch interfaces without personal accounts."""

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest
from test_integration import bundle as bundle
from test_integration import load
from test_integration import paths as paths


def program(agents: ModuleType, prefix: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    binary = prefix / "bin/qodercli"
    binary.parent.mkdir(parents=True)
    binary.write_text("#!/bin/sh\nprintf '1.1.84\\n'\n")
    binary.chmod(0o755)
    checksum = hashlib.sha256(binary.read_bytes()).hexdigest()
    monkeypatch.setattr(agents, "BINARY_SHA256", checksum)
    (prefix / "bundle.json").write_text(json.dumps({"files": {"bin/qodercli": checksum}}))
    return binary


def inputs(monkeypatch: pytest.MonkeyPatch, answers: list[str]) -> None:
    values = iter(answers)
    monkeypatch.setattr("builtins.input", lambda _prompt: next(values))
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True)


@pytest.mark.parametrize("cosh,qoder", [(False, False), (False, True), (True, False), (True, True)])
def test_setup_choices(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch, cosh: bool, qoder: bool
) -> None:
    runtime = load("runtime")
    program(runtime.agents, paths["prefix"], monkeypatch)
    monkeypatch.setenv("PATH", "/usr/bin:/bin")
    monkeypatch.setattr(runtime, "run_cosh", lambda *args: 0)
    monkeypatch.setattr(runtime, "run_agent", lambda *args: 0)
    answers = ["1", "", "y"] if cosh else [""]
    answers += ["1", "1"] if qoder else [""]
    inputs(monkeypatch, [*answers, ""])
    runtime.wizard(paths, None)
    state = runtime.setup.read_state(paths["config"])
    assert state["cosh"]["choice"] == ("configure" if cosh else "skipped")
    assert state["qoder"]["choice"] == ("configure" if qoder else "skipped")
    assert (paths["config"] / "setup.json").stat().st_mode & 0o777 == 0o600
    inputs(monkeypatch, ["", ""])
    runtime.wizard(paths, "cosh")
    assert runtime.setup.read_state(paths["config"])["qoder"] == state["qoder"]


def test_defer_lock_and_retry(paths: dict[str, Path]) -> None:
    runtime = load("runtime")
    with runtime.setup.lock(paths["run"]):
        assert runtime.setup.running(paths["run"])
        with pytest.raises(ValueError, match="already running"):
            runtime.wizard(paths, None, defer=True)
    runtime.wizard(paths, None, defer=True)
    assert not runtime.setup.running(paths["run"])
    result = runtime.setup.read_state(paths["config"])
    assert result["deferred"]
    assert result["cosh"]["choice"] == result["qoder"]["choice"] == "pending"


def test_qoder_paths_and_compatibility(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    runtime = load("runtime")
    binary = program(runtime.agents, paths["prefix"], monkeypatch)
    native = tmp_path / "路径 with spaces" / "qodercli"
    native.parent.mkdir()
    native.write_bytes(binary.read_bytes())
    native.chmod(0o755)
    link_dir = tmp_path / "links"
    link_dir.mkdir()
    (link_dir / "qodercli").symlink_to(native)
    monkeypatch.setenv("PATH", os.pathsep.join((str(link_dir), "/usr/bin")))
    candidate = runtime.agents.candidates(paths["prefix"])[0]
    assert candidate["installation"] == "compatible"
    assert candidate["path"] == str(native)
    selected = runtime.agents.selection(candidate)
    native.write_text("#!/bin/sh\nprintf '9.0.0\\n'\n")
    assert runtime.agents.validate(paths["prefix"], selected)["installation"] == "incompatible"
    native.unlink()
    assert runtime.agents.validate(paths["prefix"], selected)["installation"] == "missing"


@pytest.mark.parametrize("mode", ["native", "pat"])
def test_launch_keeps_home_and_auth_selection(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch, mode: str
) -> None:
    runtime = load("runtime")
    program(runtime.agents, paths["prefix"], monkeypatch)
    runtime.configure(paths)
    state = runtime.setup.read_state(paths["config"])
    state["qoder"]["auth_mode"] = mode
    runtime.setup.save_state(paths["config"], state)
    runtime.setup.atomic_write(runtime.credential_path(paths), b'{"token":"private-test-token"}')
    monkeypatch.setattr(runtime, "health", lambda _paths: None)
    monkeypatch.setenv(runtime.agents.PAT, "private-env-token")
    calls = []
    monkeypatch.setattr(
        runtime.subprocess, "call", lambda argv, env: calls.append((argv, env)) or 0
    )
    argument = "a; $b"
    assert runtime.run_agent(paths, ["-p", argument]) == 0
    argv, env = calls[0]
    assert argv[0].endswith("/aw") and argv[-2:] == ["-p", argument]
    assert env["HOME"] == os.environ["HOME"]
    assert env.get(runtime.agents.PAT) == ("private-env-token" if mode == "pat" else None)
    spec = json.loads(Path(argv[argv.index("--config") + 1]).read_text())["spec"]
    assert spec["execution"]["guarantee"] == "native_hook"
    assert spec["events"]["tool.before"]["steps"][0]["on_error"] == "block"
    assert "private-" not in json.dumps(spec)


def test_legacy_config_and_pat_migrate(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    program(runtime.agents, paths["prefix"], monkeypatch)
    runtime.setup.private_directory(paths["config"])
    old = runtime.config_bytes(paths)
    runtime.setup.atomic_write(paths["config"] / "aw.json", old)
    pat = b'{"token":"fixture-only"}'
    runtime.setup.atomic_write(runtime.credential_path(paths), pat)
    runtime.configure(paths)
    assert (paths["config"] / "aw.demo2.json").read_bytes() == old
    assert runtime.credential_path(paths).read_bytes() == pat
    assert runtime.setup.read_state(paths["config"])["qoder"]["auth_mode"] == "pat"
    receipt = (paths["config"] / "managed-config.json").read_bytes()
    runtime.configure(paths)
    assert (paths["config"] / "managed-config.json").read_bytes() == receipt


def test_legacy_configuration_with_non_ascii_paths(paths: dict[str, Path]) -> None:
    runtime = load("runtime")
    paths["state"] = paths["state"] / "带空格 路径"
    runtime.setup.private_directory(paths["config"])
    old = (json.dumps(runtime.aw_document(paths), indent=2) + "\n").encode()
    runtime.setup.atomic_write(paths["config"] / "aw.json", old)
    runtime.configure(paths)
    assert (paths["config"] / "aw.demo2.json").read_bytes() == old
    assert str(paths["state"]) in (paths["config"] / "aw.json").read_text()


def test_partial_statistics_stay_unknown(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    store = paths["state"] / "tokenless"
    store.mkdir(parents=True)
    (store / "stats.db").touch()
    monkeypatch.setattr(
        runtime,
        "invoke",
        lambda command, **kwargs: (
            []
            if "events" in command
            else {
                "schema_version": "1.1",
                "total": {"records": 1, "tokens_saved": 0},
            }
        ),
    )
    security, tokenless = runtime.dashboards(paths, runtime.timestamp())
    assert security["checked"] == security["risks"] == 0 and security["blocked"] is None
    assert tokenless["saved"] is None and tokenless["state"] == "unavailable"


def test_system_service_rollback_restores_old_unit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    service = load("system_install")
    unit = tmp_path / "anolisa-omarchy-security.service"
    journal = tmp_path / "journal.json"
    skillsec = tmp_path / "skillsec.json"
    monkeypatch.setattr(service, "SERVICE", unit)
    monkeypatch.setattr(service, "JOURNAL", journal)
    monkeypatch.setattr(service, "SKILLSEC", skillsec)
    old = service.unit_text(Path("/opt/anolisa-omarchy/sec-core") / ("a" * 64))
    unit.write_text("new staged unit")
    skillsec.write_text("temporary settings")
    journal.write_text(
        json.dumps(
            {
                "transaction": "demo3-test",
                "unit": old,
                "active": True,
                "enabled": True,
                "changed": True,
                "created_skillsec": True,
            }
        )
    )
    actions = []
    monkeypatch.setattr(service, "service_state", lambda _action: True)
    monkeypatch.setattr(service, "systemctl", lambda *args: actions.append(args))
    service.rollback("demo3-test")
    assert unit.read_text() == old
    assert not skillsec.exists() and not journal.exists()
    assert actions == [
        ("stop", unit.name),
        ("daemon-reload",),
        ("enable", unit.name),
        ("start", unit.name),
    ]


def test_activation_failure_restores_application(
    bundle: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installer = load("install")
    user = tmp_path / "user"
    user.mkdir()
    monkeypatch.setenv("HOME", str(user))
    for key in ("CONFIG", "STATE", "RUNTIME", "DATA"):
        monkeypatch.setenv(
            f"XDG_{key}_{'DIR' if key == 'RUNTIME' else 'HOME'}", str(tmp_path / key)
        )
    installer.install(bundle, enable=False)
    current = tmp_path / "DATA/anolisa-omarchy/current"
    previous = current.readlink()
    manifest = bundle / "bundle.json"
    metadata = json.loads(manifest.read_text())
    metadata["version"] = "next"
    manifest.write_text(json.dumps(metadata))
    monkeypatch.setattr(
        installer,
        "SYSTEM_INPUT_DIGESTS",
        {
            "system_install.py": metadata["files"]["system_install.py"],
            "bin/agent-sec-daemon": "a" * 64,
        },
    )
    monkeypatch.setattr(
        installer.shutil,
        "which",
        lambda name, **kwargs: None if name == "qodercli" else "/usr/bin/" + name,
    )
    monkeypatch.setattr(installer, "dependencies", lambda *args: [])
    import runtime as runtime_module

    monkeypatch.setattr(installer, "verified_runtime", lambda _bundle: runtime_module)
    monkeypatch.setattr(runtime_module, "healthy", lambda *args: True)
    commands = []
    privileged_inputs = []
    registry_reads = 0

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess:
        nonlocal registry_reads
        commands.append(command)
        if command[0] == "sudo":
            privileged_inputs.append(kwargs["input"])
            assert command[2:7] == [
                "/usr/bin/python3",
                "-I",
                "-B",
                "-c",
                installer.SYSTEM_BOOTSTRAP,
            ]
            if "prepare" in command:
                # A same-user edit after prepare must not change rollback's code.
                target = (
                    current.parent
                    / "releases"
                    / hashlib.sha256(manifest.read_bytes()).hexdigest()[:16]
                    / "system_install.py"
                )
                target.write_text("raise SystemExit('replaced helper')")
        if command == ["omarchy-shell", "shell", "listPlugins"]:
            registry_reads += 1
            return subprocess.CompletedProcess(
                command, 0, stdout="[]" if registry_reads == 1 else '[{"id":"community.anolisa"}]'
            )
        if command[:3] == ["omarchy", "plugin", "enable"]:
            raise subprocess.CalledProcessError(1, command)
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(installer.subprocess, "run", run)
    with pytest.raises(subprocess.CalledProcessError):
        installer.install(bundle, enable=True, non_interactive=True)
    assert current.readlink() == previous
    assert registry_reads == 2
    assert any("rollback" in command for command in commands)
    assert len(privileged_inputs) == 2 and privileged_inputs[0] == privileged_inputs[1]
    assert (
        hashlib.sha256(privileged_inputs[0]).hexdigest() == metadata["files"]["system_install.py"]
    )
    assert not any("commit" in command for command in commands)
    receipt = json.loads((tmp_path / "STATE/anolisa-omarchy/installation.json").read_text())
    assert receipt["phase"] == "plugin_enable"
    assert receipt["rollback_errors"] == []
    assert (user / ".config/omarchy/plugins/community.anolisa/manifest.json").exists()
