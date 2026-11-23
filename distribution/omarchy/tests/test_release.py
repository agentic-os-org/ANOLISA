"""Exercise installer archives, input verification and release failure boundaries."""

import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tomllib
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import checkout_install
import package
import release
import sbom
import verify_release


@pytest.fixture
def candidate(tmp_path: Path) -> Path:
    payload = tmp_path / "payload"
    for name in verify_release.REQUIRED:
        path = payload / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"native-program-fixture")
    # Assembly supplies these files from the source under test.
    (payload / "runtime.py").unlink()
    (payload / "plugin/manifest.json").unlink()
    (payload / "plugin").rmdir()
    out = tmp_path / "artifacts"
    out.mkdir()
    archive, _ = package.assemble(
        payload,
        out,
        {"commit": "a" * 40, "dirty": True, "tree_sha256": "b" * 64},
        {"aw": "0.1.1"},
        1234567890,
    )
    return archive


def rewrite(archive: Path, change: object) -> None:
    records = []
    with tarfile.open(archive) as source:
        for member in source:
            body = source.extractfile(member).read() if member.isfile() else None
            records.append((member, body))
    change(records)
    with tarfile.open(archive, "w:gz") as target:
        for member, body in records:
            if body is not None:
                member.size = len(body)
            target.addfile(member, io.BytesIO(body) if body is not None else None)


def test_candidate_complete_and_root_owned(candidate: Path) -> None:
    result = verify_release.verify(
        candidate, package.digest(candidate), package.settings()["version"]
    )
    assert result["source"]["dirty"] is True
    assert verify_release.REQUIRED <= result["files"].keys()
    with tarfile.open(candidate) as archive:
        assert archive.getmember(f"anolisa-omarchy-{result['version']}/install").mode == 0o755
        assert all(member.uid == member.gid == 0 for member in archive)


def test_checksum_and_version_are_checked(candidate: Path) -> None:
    with pytest.raises(ValueError, match="checksum"):
        verify_release.verify(candidate, "0" * 64)
    with pytest.raises(ValueError, match="version"):
        verify_release.verify(candidate, version="99.0.0")


def test_changed_payload_is_not_accepted(candidate: Path) -> None:
    def corrupt(records: list) -> None:
        index = next(
            n for n, (member, _) in enumerate(records) if member.name.endswith("payload/bin/aw")
        )
        member, body = records[index]
        records[index] = (member, body + b"changed")

    rewrite(candidate, corrupt)
    with pytest.raises(ValueError, match="checksum"):
        verify_release.verify(candidate)


@pytest.mark.parametrize("kind", ["traversal", "link", "duplicate", "extra", "missing", "mode"])
def test_unsafe_or_incomplete_archive_rejected(candidate: Path, kind: str) -> None:
    def mutate(records: list) -> None:
        file_index = next(
            n for n, (member, _) in enumerate(records) if member.name.endswith("payload/bin/aw")
        )
        member, body = records[file_index]
        if kind == "missing":
            records.pop(file_index)
        elif kind == "mode":
            member.mode = 0o777
        elif kind == "duplicate":
            records.append((member, body))
        elif kind == "traversal":
            member.name = "../outside"
        elif kind == "link":
            member.type, member.linkname = tarfile.SYMTYPE, "/etc/passwd"
            records[file_index] = (member, None)
        else:
            extra = tarfile.TarInfo(member.name + ".extra")
            extra.mode = 0o755
            records.append((extra, b"unexpected"))

    rewrite(candidate, mutate)
    with pytest.raises(ValueError):
        verify_release.verify(candidate)


def test_sidecar_identifies_exact_filename(candidate: Path, tmp_path: Path) -> None:
    path = tmp_path / "checksum"
    path.write_text(f"{package.digest(candidate)}  {candidate.name}\n", encoding="ascii")
    assert checkout_install.checksum(path, candidate.name) == package.digest(candidate)
    path.write_text(f"{'0' * 64}  another-package.tar.gz\n", encoding="ascii")
    with pytest.raises(ValueError, match="exact"):
        checkout_install.checksum(path, candidate.name)
    path.write_text(f"{'0' * 64}  {candidate.name}\n\n", encoding="ascii")
    with pytest.raises(ValueError, match="one line"):
        checkout_install.checksum(path, candidate.name)


def test_download_digest_failure_leaves_no_cached_candidate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    response = io.BytesIO(b"corrupt input")
    response.url = "https://release-assets.githubusercontent.com/input"
    monkeypatch.setattr(release.urllib.request, "urlopen", lambda *_args, **_kwargs: response)
    with pytest.raises(ValueError, match="checksum"):
        release.fetch(
            {
                "url": "https://github.com/agentic-os-org/ANOLISA/releases/download/tag/input.tar.gz",
                "sha256": "0" * 64,
            },
            tmp_path,
        )
    assert list(tmp_path.iterdir()) == []


def test_source_build_requires_clean_checkout(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(release, "git", lambda *_args: " M runtime.py")
    with pytest.raises(ValueError, match="clean checkout"):
        release.source_receipt(False)


def test_integration_versions_agree() -> None:
    version = package.settings()["version"]
    assert json.loads((ROOT / "plugin/manifest.json").read_text())["version"] == version
    assert tomllib.loads((ROOT / "pyproject.toml").read_text())["project"]["version"] == version
    for name in ("CHANGELOG.md", "CHANGELOG_zh.md"):
        changelog = (ROOT / name).read_text()
        released = re.findall(r"^## \[(\d+\.\d+\.\d+)\]", changelog, re.MULTILINE)
        # New components keep an Unreleased stub until the version-bump PR.
        if released:
            assert released[0] == version
        else:
            assert "## [Unreleased]" in changelog


@pytest.mark.parametrize(
    "dirty,tag,target",
    [
        (True, "omarchy/v0.1.3", "a" * 40),
        (False, "omarchy-v0.1.3", "a" * 40),
        (False, "omarchy/v0.1.3", "b" * 40),
    ],
)
def test_formal_release_refuses_wrong_source(
    dirty: bool, tag: str, target: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(release, "source_receipt", lambda *_: {"commit": "a" * 40, "dirty": dirty})
    monkeypatch.setattr(release, "git", lambda *_: target)
    with pytest.raises(ValueError, match="tag"):
        release.build(tmp_path / "output", tmp_path / "cache", "0.1.3", tag, allow_dirty=True)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize("kind", ["missing", "list", "no-files", "invalid-digest"])
def test_malformed_manifest_has_clear_error(candidate: Path, kind: str) -> None:
    def mutate(records: list) -> None:
        index = next(
            i for i, (member, _) in enumerate(records) if member.name.endswith("/bundle.json")
        )
        member, body = records[index]
        if kind == "missing":
            records.pop(index)
            return
        document = json.loads(body)
        if kind == "list":
            document = []
        elif kind == "no-files":
            document.pop("files")
        else:
            document["files"]["bin/aw"] = False
        records[index] = (member, json.dumps(document).encode())

    rewrite(candidate, mutate)
    with pytest.raises(ValueError, match="manifest|inventory"):
        verify_release.verify(candidate)


def test_failed_compilation_exposes_no_release(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    receipt = {"commit": "a" * 40, "dirty": True, "tree_sha256": "b" * 64}
    config = dict(package.settings(), upstream={})
    monkeypatch.setattr(release.package, "settings", lambda: config)
    monkeypatch.setattr(release, "source_receipt", lambda *_: receipt)
    monkeypatch.setattr(release, "git", lambda *_: "1234567890")

    def fail(*_args: object) -> None:
        raise subprocess.CalledProcessError(1, ["cargo", "build"])

    monkeypatch.setattr(release, "build_bridges", fail)
    output = tmp_path / "candidate"
    with pytest.raises(subprocess.CalledProcessError):
        release.build(output, tmp_path / "cache", config["version"], allow_dirty=True)
    assert not output.exists()
    assert not list(tmp_path.glob(".omarchy-build-*"))
    output.mkdir()
    sentinel = output / "existing-release"
    sentinel.write_text("keep")
    with pytest.raises(ValueError, match="never overwritten"):
        release.build(output, tmp_path / "cache", config["version"], allow_dirty=True)
    assert sentinel.read_text() == "keep"


def test_checkout_installer_checks_before_launch(
    candidate: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    Path(str(candidate) + ".sha256").write_text(f"{'0' * 64}  {candidate.name}\n")
    monkeypatch.setattr(checkout_install.os, "geteuid", lambda: 1000)
    monkeypatch.setattr(checkout_install.shutil, "which", lambda *_: "/usr/bin/omarchy")
    calls = []
    monkeypatch.setattr(
        checkout_install.subprocess, "call", lambda *args, **kwargs: calls.append(args)
    )
    with pytest.raises(ValueError, match="checksum"):
        checkout_install.install(candidate, [])
    assert calls == []


def test_upstream_sbom_must_bind_selected_archive() -> None:
    component = {"version": "1.0.0", "assets": {"archive": {"sha256": "a" * 64}}}
    document = {
        "bomFormat": "CycloneDX",
        "metadata": {
            "component": {"version": "1.0.0", "hashes": [{"alg": "SHA-256", "content": "b" * 64}]}
        },
    }
    with pytest.raises(ValueError, match="bind"):
        release.verify_inventory(document, component)


def cargo_fixture(name: str) -> dict:
    return {
        "packages": [{"id": "root", "name": name, "version": "0.1.1", "license": "Apache-2.0"}],
        "resolve": {"nodes": [{"id": "root", "deps": []}]},
    }


@pytest.mark.parametrize("tools", [None, {}, [{"name": "unknown", "version": "1"}]])
def test_sbom_and_shared_release_verifier_agree(candidate: Path, tools: object) -> None:
    manifest = verify_release.verify(candidate)
    metadata = {name: cargo_fixture(name) for name in release.BRIDGES}
    inventory = sbom.write_sbom(candidate, manifest, {}, metadata)
    for path in (candidate, inventory):
        Path(str(path) + ".sha256").write_text(
            f"{package.digest(path)}  {path.name}\n", encoding="ascii"
        )
    command = [
        sys.executable,
        str(ROOT.parents[1] / ".github/actions/prebuilt-rust-common/verify-artifacts.py"),
        "--directory",
        str(candidate.parent),
        "--component",
        "omarchy",
        "--version",
        manifest["version"],
        "--os",
        "linux",
        "--arch",
        "x86_64",
        "--layout",
        "flat",
    ]
    subprocess.run(command, check=True)
    malformed = json.loads(inventory.read_text(encoding="utf-8"))
    malformed["metadata"]["tools"] = tools
    inventory.write_text(json.dumps(malformed), encoding="utf-8")
    Path(str(inventory) + ".sha256").write_text(f"{package.digest(inventory)}  {inventory.name}\n")
    rejected = subprocess.run(command, capture_output=True, text=True)
    assert rejected.returncode != 0 and "does not identify" in rejected.stderr
    inventory.write_text(inventory.read_text(encoding="utf-8") + "corrupt", encoding="utf-8")
    assert subprocess.run(command, capture_output=True).returncode != 0


@pytest.mark.parametrize(
    "path,expected",
    [
        ("distribution/omarchy/runtime.py", "true"),
        ("docs/user-guide/en/omarchy/index.md", "true"),
        ("docs/user-guide/zh/omarchy/index.md", "true"),
        (".github/actions/build-omarchy-prebuilt/action.yaml", "true"),
        (".github/workflows/release.yaml", "true"),
        ("src/tokenless/example.rs", "false"),
    ],
)
def test_real_ci_detection_script(path: str, expected: str, tmp_path: Path) -> None:
    document = yaml.safe_load(
        (ROOT.parents[1] / ".github/workflows/ci.yaml").read_text(encoding="utf-8")
    )
    step = next(
        item for item in document["jobs"]["detect-changes"]["steps"] if item.get("id") == "changes"
    )
    import re

    script = re.sub(r"\$\{\{ inputs\.[a-z_]+ \}\}", "false", step["run"])
    tools = tmp_path / "tools"
    tools.mkdir()
    shim = tools / "git"
    shim.write_text(
        '#!/bin/sh\nif [ "$1" = diff ]; then printf "%s\\n" "$CHANGED_FIXTURE"; fi\n',
        encoding="utf-8",
    )
    shim.chmod(0o755)
    output = tmp_path / "output"
    subprocess.run(
        ["bash", "-c", script],
        env=dict(
            os.environ,
            PATH=str(tools) + ":" + os.environ["PATH"],
            CHANGED_FIXTURE=path,
            GITHUB_OUTPUT=str(output),
            GITHUB_STEP_SUMMARY=str(tmp_path / "summary"),
        ),
        check=True,
    )
    assert f"omarchy={expected}\n" in output.read_text(encoding="utf-8")
