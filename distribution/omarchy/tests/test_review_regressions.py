"""Cover installation trust, recovery and polling cases found during PR review."""

import ast
import hashlib
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest
import yaml
from test_integration import ROOT, load
from test_integration import bundle as bundle
from test_integration import paths as paths
from test_release import candidate as candidate


@pytest.mark.parametrize(
    "output,accepted",
    [
        ("extra/example\n", True),
        ("extra/example\nextra/example\n", True),
        ("extra/example\ncore/other\n", False),
        ("", False),
        ("extra/example;whoami\n", False),
        ("extra/example/other\n", False),
    ],
)
def test_qualified_pacman_matches(
    output: str, accepted: bool, monkeypatch: pytest.MonkeyPatch
) -> None:
    installer = load("install")
    checks = iter([{"libexample.so"}, set()])
    monkeypatch.setattr(installer, "required_libraries", lambda _release: next(checks))
    monkeypatch.setattr(installer.shutil, "which", lambda _name: "/usr/bin/pacman")
    commands = []

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess:
        commands.append(command)
        return subprocess.CompletedProcess(command, 0, stdout=output)

    monkeypatch.setattr(installer.subprocess, "run", run)
    if accepted:
        assert installer.dependencies(Path("/fixture"), ["sudo", "-n"]) == ["extra/example"]
        assert [
            "sudo",
            "-n",
            "pacman",
            "-S",
            "--needed",
            "--noconfirm",
            "--",
            "extra/example",
        ] in commands
    else:
        with pytest.raises(ValueError, match="No unique pacman package"):
            installer.dependencies(Path("/fixture"), ["sudo"])
        assert not any("-S" in command for command in commands)


def test_custom_xdg_keeps_host_plugin_under_home(
    bundle: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    installer = load("install")
    home = tmp_path / "home"
    home.mkdir()
    config = tmp_path / "custom-config"
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(config))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path / "run"))
    installer.install(bundle, enable=False)
    assert (home / ".config/omarchy/plugins/community.anolisa/manifest.json").is_file()
    assert not (config / "omarchy").exists()
    assert (config / "anolisa-omarchy/aw.json").is_file()


def test_managed_tokenless_ignores_both_database_overrides(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    monkeypatch.setenv("DSH_SHELL", "1")
    for name in ("TOKENLESS_STATS_DB", "DSH_TOKENLESS_STATS_DB"):
        monkeypatch.setenv(name, "/alternate/stats.db")
    env = runtime.environment(paths)
    assert env["TOKENLESS_DATA_DIR"] == str(paths["state"] / "tokenless")
    assert "TOKENLESS_STATS_DB" not in env and "DSH_TOKENLESS_STATS_DB" not in env
    assert os.environ["DSH_TOKENLESS_STATS_DB"] == "/alternate/stats.db"


def test_poll_cache_persists_but_launch_always_hashes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    agents = load("agent_installation")
    binary = tmp_path / "qodercli"
    binary.write_bytes(b"tested fixture")
    binary.chmod(0o755)
    checksum = hashlib.sha256(binary.read_bytes()).hexdigest()
    monkeypatch.setattr(agents, "BINARY_SHA256", checksum)
    hashed = []
    original = agents.digest
    monkeypatch.setattr(agents, "digest", lambda path: hashed.append(path) or original(path))
    assert agents.inspect(binary, "existing", full=False)["installation"] == "compatible"
    assert agents.inspect(binary, "existing", full=False)["installation"] == "compatible"
    assert len(hashed) == 1
    # A new status process can reuse the receipt; execution still bypasses it.
    reloaded = load("agent_installation")
    monkeypatch.setattr(reloaded, "BINARY_SHA256", checksum)
    monkeypatch.setattr(reloaded, "digest", lambda _path: pytest.fail("Unexpected polling hash"))
    assert reloaded.inspect(binary, "existing", full=False)["installation"] == "compatible"
    assert agents.inspect(binary, "existing")["installation"] == "compatible"
    assert len(hashed) == 2
    original_time = binary.stat().st_mtime_ns
    binary.write_bytes(b"edited fixture")
    os.utime(binary, ns=(original_time, original_time))
    assert agents.inspect(binary, "existing", full=False)["installation"] == "incompatible"
    assert len(hashed) == 3
    # Even a forged display receipt cannot authorize an altered executable.
    receipt = agents.data_directory() / "display-inspection.json"
    cached = json.loads(receipt.read_text())
    cached["sha256"] = checksum
    receipt.write_text(json.dumps(cached))
    assert agents.inspect(binary, "existing")["installation"] == "incompatible"


def test_built_installer_pins_privileged_files(candidate: Path) -> None:
    with tarfile.open(candidate) as archive:
        root = "anolisa-omarchy-0.1.3/"
        manifest = json.load(archive.extractfile(root + "bundle.json"))
        tree = ast.parse(archive.extractfile(root + "install.py").read())
    pins = next(
        ast.literal_eval(node.value)
        for node in tree.body
        if isinstance(node, ast.Assign)
        and any(
            isinstance(target, ast.Name) and target.id == "SYSTEM_INPUT_DIGESTS"
            for target in node.targets
        )
    )
    assert pins == {
        name: manifest["files"][name] for name in ("system_install.py", "bin/agent-sec-daemon")
    }


def test_packaged_modules_load_in_fresh_isolated_process(candidate: Path) -> None:
    root = candidate.parent.parent / "anolisa-omarchy-0.1.3"
    # No pre-imported test modules may hide missing or wrongly ordered imports.
    script = (
        "import runpy,sys; from pathlib import Path; "
        "root=Path(sys.argv[1]); installer=runpy.run_path(str(root/'install.py')); "
        "runtime=installer['verified_runtime'](root); "
        "assert callable(runtime.configuration_plan); print('ISOLATED_IMPORT_OK')"
    )
    result = subprocess.run(
        [sys.executable, "-I", "-B", "-c", script, str(root)], capture_output=True, text=True
    )
    assert result.returncode == 0, result.stderr
    assert "ISOLATED_IMPORT_OK" in result.stdout


@pytest.mark.parametrize("rewrite_manifest", [False, True])
def test_caller_module_replacement_is_rejected(
    candidate: Path, monkeypatch: pytest.MonkeyPatch, rewrite_manifest: bool
) -> None:
    installer = load("install")
    root = candidate.parent.parent / "anolisa-omarchy-0.1.3"
    metadata = installer.verify(root)
    monkeypatch.setattr(
        installer,
        "INSTALL_MODULE_DIGESTS",
        {
            name: metadata["files"][name]
            for name in ("setup.py", "i18n.py", "agent_installation.py", "runtime.py")
        },
    )
    runtime = root / "payload/runtime.py"
    runtime.write_text("raise AssertionError('Unverified caller code executed')")
    if rewrite_manifest:
        metadata["files"]["runtime.py"] = hashlib.sha256(runtime.read_bytes()).hexdigest()
        (root / "bundle.json").write_text(json.dumps(metadata))
        installer.verify(root)
    with pytest.raises(ValueError, match="Installer module checksum mismatch: runtime"):
        installer.verified_runtime(root)


def test_module_execution_uses_verified_snapshot(
    candidate: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import builtins

    installer = load("install")
    root = candidate.parent.parent / "anolisa-omarchy-0.1.3"
    metadata = installer.verify(root)
    names = ("setup", "i18n", "agent_installation", "runtime")
    monkeypatch.setattr(
        installer,
        "INSTALL_MODULE_DIGESTS",
        {name + ".py": metadata["files"][name + ".py"] for name in names},
    )
    # Preserve the test process's module table after the isolated loader check.
    for name in names:
        monkeypatch.setitem(sys.modules, name, sys.modules.get(name))
    execute = builtins.exec

    def replace_after_verification(
        code: object, namespace: dict, *args: object, **kwargs: object
    ) -> None:
        if getattr(code, "co_filename", None) == str(root / "payload/setup.py"):
            (root / "payload/runtime.py").write_text(
                "raise AssertionError('Late replacement executed')"
            )
        execute(code, namespace, *args, **kwargs)

    monkeypatch.setattr(builtins, "exec", replace_after_verification)
    assert callable(installer.verified_runtime(root).configuration_plan)


def test_bootstrap_rejects_replaced_helper_before_touching_system(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    installer = load("install")
    import io
    from types import SimpleNamespace

    monkeypatch.setattr(os, "geteuid", lambda: 0)
    monkeypatch.setattr(sys, "argv", ["-c", hashlib.sha256(b"trusted").hexdigest(), "prepare"])
    monkeypatch.setattr(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(b"replaced")))
    monkeypatch.setattr(Path, "mkdir", lambda *args, **kwargs: pytest.fail("Touched system paths"))
    with pytest.raises(ValueError, match="Privileged helper checksum mismatch"):
        exec(installer.SYSTEM_BOOTSTRAP, {})


def test_daemon_pin_does_not_follow_mutable_manifest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    service = load("system_install")
    parent = tmp_path / "root-stage"
    parent.mkdir()
    monkeypatch.setattr(service, "PARENT", parent)
    payload = tmp_path / "payload"
    payload.mkdir()
    binary = payload / "agent-sec-daemon"
    binary.write_bytes(b"replaced daemon")
    expected = hashlib.sha256(b"trusted daemon").hexdigest()
    (payload / "bundle.json").write_text(
        json.dumps(
            {
                "files": {
                    "bin/agent-sec-daemon": hashlib.sha256(binary.read_bytes()).hexdigest(),
                }
            }
        )
    )
    with pytest.raises(ValueError, match="Security daemon checksum mismatch"):
        service.install_binary(binary, expected)
    assert not (parent / expected).exists()


def test_interrupted_prepare_has_explicit_recovery_then_retry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    service = load("system_install")
    for name, value in {
        "PARENT": tmp_path,
        "SERVICE": tmp_path / "service",
        "SKILLSEC": tmp_path / "settings.json",
        "JOURNAL": tmp_path / "journal.json",
    }.items():
        monkeypatch.setattr(service, name, value)
    monkeypatch.setattr(service, "secure_directory", lambda *args: None)
    monkeypatch.setattr(service, "install_binary", lambda *args: tmp_path / ("a" * 64))
    monkeypatch.setattr(service, "systemctl", lambda *args: None)
    monkeypatch.setattr(service, "service_state", lambda *args: False)
    monkeypatch.setattr(os, "geteuid", lambda: 0)
    first, second = "a" * 32, "b" * 32
    service.prepare(tmp_path / "binary", "a" * 64, first)
    with pytest.raises(ValueError, match="bash ./install --recover"):
        service.prepare(tmp_path / "binary", "a" * 64, second)
    monkeypatch.setattr(sys, "argv", ["system_install.py", "recover", second])
    service.main()
    assert not service.JOURNAL.exists() and not service.SERVICE.exists()
    service.prepare(tmp_path / "binary", "a" * 64, second)
    monkeypatch.setattr(sys, "argv", ["system_install.py", "commit", second])
    service.main()
    assert not service.JOURNAL.exists() and service.SERVICE.is_file()


def test_source_archive_rejects_stale_staging(tmp_path: Path) -> None:
    document = yaml.safe_load(
        (ROOT.parents[1] / ".github/actions/package-source/action.yaml").read_text()
    )
    script = next(
        step["run"]
        for step in document["runs"]["steps"]
        if step.get("name") == "Create source archive"
    )
    branch = script.split('elif [ "${{ inputs.component }}" = "omarchy" ]; then\n', 1)[1].split(
        "elif [ ", 1
    )[0]
    # Run the real guard against a disposable staging tree, before git archive.
    guard = branch.split("# Archive only committed", 1)[0]
    staging = tmp_path / "candidate"
    staging.mkdir()
    (staging / "stale-secret.txt").write_text("fixture only")
    script = guard.replace("/tmp/build/${ARCHIVE_NAME}", str(staging))
    result = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    assert result.returncode == 1 and "must be empty" in result.stderr
    assert (staging / "stale-secret.txt").read_text() == "fixture only"


def test_clean_source_archive_ships_committed_guides(tmp_path: Path) -> None:
    document = yaml.safe_load(
        (ROOT.parents[1] / ".github/actions/package-source/action.yaml").read_text()
    )
    script = next(
        step["run"]
        for step in document["runs"]["steps"]
        if step.get("name") == "Create source archive"
    )
    branch = script.split('elif [ "${{ inputs.component }}" = "omarchy" ]; then\n', 1)[1].split(
        "elif [ ", 1
    )[0]
    repo, stage = tmp_path / "repo", tmp_path / "stage"
    repo.mkdir()
    stage.mkdir()
    files = {
        "distribution/omarchy/install": "fixture installer\n",
        "LICENSE": "fixture license\n",
        "docs/user-guide/en/omarchy/index.md": "English [中文](../../zh/omarchy/index.md)\n",
        "docs/user-guide/zh/omarchy/index.md": "中文 [English](../../en/omarchy/index.md)\n",
    }
    for name, content in files.items():
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "add", "--", *files], check=True)
    # A tree object exercises git archive/show without creating a fixture commit.
    tree = subprocess.check_output(["git", "-C", str(repo), "write-tree"], text=True).strip()
    (repo / "distribution/omarchy/untracked-secret.txt").write_text("must stay out")
    branch = branch.replace("/tmp/build/${ARCHIVE_NAME}", str(stage)).replace("HEAD", tree)
    subprocess.run(
        ["bash", "-e", "-o", "pipefail", "-c", branch],
        cwd=repo,
        env=dict(os.environ, SRC_DIR="distribution/omarchy"),
        check=True,
    )
    assert (stage / "install").read_text() == "fixture installer\n"
    assert (stage / "LICENSE").read_text() == "fixture license\n"
    assert "USER_GUIDE_zh.md" in (stage / "USER_GUIDE.md").read_text()
    assert "USER_GUIDE.md" in (stage / "USER_GUIDE_zh.md").read_text()
    assert not (stage / "untracked-secret.txt").exists()
