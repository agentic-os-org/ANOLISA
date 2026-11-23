"""Check official downloads, integrity and per-user selection without model calls."""

import hashlib
import io
import json
import tarfile
from pathlib import Path
from types import ModuleType

import pytest
from test_integration import load
from test_integration import paths as paths


def archive(version: str) -> tuple[bytes, bytes]:
    binary = f"#!/bin/sh\nprintf '{version}\\n'\n".encode()
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as tar:
        info = tarfile.TarInfo("qodercli")
        info.size = len(binary)
        tar.addfile(info, io.BytesIO(binary))
    return data.getvalue(), binary


def server(
    agents: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    version: str = "9.8.7",
    url: str | None = None,
) -> tuple[dict, bytes, list[str]]:
    data, binary = archive(version)
    url = url or agents.BASE_URL + f"/releases/{version}/qodercli-linux-x64.tar.gz"
    manifest = {
        "latest": version,
        "files": [
            {"os": "linux", "arch": "amd64", "url": url, "sha256": hashlib.sha256(data).hexdigest()}
        ],
    }
    calls = []

    def fetch(address: str, **kwargs: object) -> io.BytesIO:
        calls.append(address)
        return io.BytesIO(
            json.dumps(manifest).encode() if address.endswith("manifest.json") else data
        )

    monkeypatch.setattr(agents.urllib.request, "urlopen", fetch)
    return manifest, binary, calls


def test_latest_install_is_private_persistent_and_checked(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    agents = load("agent_installation")
    _, binary, calls = server(agents, monkeypatch)
    report = agents.install("latest")
    selected = agents.selection(report)
    assert selected["version"] == "9.8.7" and selected["channel"] == "latest"
    assert report["build_validation"] == "not_verified"
    assert Path(selected["path"]).read_bytes() == binary
    assert (agents.data_directory() / "current.json").stat().st_mode & 0o777 == 0o600
    assert len(calls) == 2
    assert agents.candidates(tmp_path / "system-prefix")[0]["path"] == selected["path"]
    Path(selected["path"]).write_bytes(b"changed")
    assert agents.validate(tmp_path, selected)["installation"] == "incompatible"


@pytest.mark.parametrize("change", ["url", "checksum", "version"])
def test_bad_manifest_is_rejected(monkeypatch: pytest.MonkeyPatch, change: str) -> None:
    agents = load("agent_installation")
    manifest, _, _ = server(agents, monkeypatch)
    if change == "url":
        manifest["files"][0]["url"] = "https://example.invalid/qoder.tar.gz"
    elif change == "checksum":
        manifest["files"][0]["sha256"] = "invalid"
    else:
        manifest["latest"] = "../../escape"
    with pytest.raises(ValueError, match="manifest"):
        agents.release("latest")


def test_bad_archive_keeps_existing_program(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    agents = load("agent_installation")
    manifest, _, _ = server(agents, monkeypatch)
    resolved = agents.release("latest")
    resolved["sha256"] = "0" * 64
    target = tmp_path / "qodercli"
    target.write_bytes(b"existing")
    with pytest.raises(ValueError, match="verification"):
        agents.stage_qoder(tmp_path / "cache", target, resolved)
    assert target.read_bytes() == b"existing"


def test_latest_install_preserves_account_and_managed_aw_config(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    server(runtime.agents, monkeypatch)
    runtime.configure(paths)
    credential = runtime.credential_path(paths)
    runtime.setup.atomic_write(credential, b'{"token":"account-sentinel"}')
    runtime.install_qoder(paths, "latest")
    selected = runtime.agents.read_selection(paths["config"])
    assert selected["source"] == "downloaded"
    assert runtime.check_managed_config(paths, selected) is not None
    runtime.configure(paths)
    assert runtime.agents.read_selection(paths["config"]) == selected
    assert runtime.read_credential(paths) == "account-sentinel"


def test_configuration_without_optional_qoder(
    paths: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    runtime = load("runtime")
    monkeypatch.setenv("PATH", "/usr/bin:/bin")
    runtime.configure(paths)
    assert runtime.agents.validate(paths["prefix"], None)["installation"] == "missing"
    assert (paths["config"] / "aw.json").exists()
