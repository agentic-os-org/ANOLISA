"""Small executable consumer contrasts; not full upstream-Skill exploit claims."""

import base64
import io
import json
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest
from agent_sec_cli.skill_score.contracts import ContractError, Fixture
from agent_sec_cli.skill_score.fixture_formats import build_deb
from agent_sec_cli.skill_score.probes import validate_deb_fixture


def test_literal_shell_argument_is_data_not_shell_source(tmp_path: Path) -> None:
    payload = "$(touch marker)"
    result = subprocess.run(
        ["sh", "-c", 'printf "%s" "$1"', "sh", payload],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=True,
    )
    assert result.stdout == payload and not (tmp_path / "marker").exists()
    subprocess.run(
        ["sh", "-c", f'printf "%s" "{payload}"'],
        cwd=tmp_path,
        capture_output=True,
        check=True,
    )
    assert (tmp_path / "marker").exists()


def test_validator_python_source_interpolation_differs_from_shell_and_argv(
    tmp_path: Path,
) -> None:
    (tmp_path / "safe.json").write_text("{}")
    path = "safe.json', 'r') as f:\n    pass\nfrom pathlib import Path\nPath('marker').write_text('observed')\nwith open('safe.json"
    source = f"import json\nwith open('{path}', 'r') as f:\n    data = json.load(f)\nprint(data)\n"
    result = subprocess.run(
        [sys.executable, "-c", source],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0 and (tmp_path / "marker").read_text() == "observed"
    (tmp_path / "marker").unlink()
    # The argv control opens the literal filename; even a valid file with that name is data.
    (tmp_path / path).write_text("{}")
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "import json,sys; print(json.load(open(sys.argv[1])))",
            path,
        ],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0 and not (tmp_path / "marker").exists()


@pytest.mark.parametrize("bounded", [False, True])
def test_archive_filter_changes_traversal_not_archive_capability(
    tmp_path: Path, bounded: bool
) -> None:
    archive = tmp_path / "control.tar"
    destination = tmp_path / "extract"
    destination.mkdir()
    with tarfile.open(archive, "w") as stream:
        info = tarfile.TarInfo("../marker")
        info.size = 8
        stream.addfile(info, io.BytesIO(b"observed"))
    with tarfile.open(archive) as stream:
        if bounded:
            with pytest.raises(tarfile.FilterError):
                stream.extractall(destination, filter="data")
        else:
            stream.extractall(destination, filter="fully_trusted")
    assert (tmp_path / "marker").exists() is not bounded


def test_multiline_json_failure_is_functional_not_a_shell_injection() -> None:
    item = {"pattern": "file", "type": "file"}
    pretty = json.dumps(item, indent=2)
    assert all(not line.strip().startswith("$(") for line in pretty.splitlines())
    for line in pretty.splitlines():
        with pytest.raises(json.JSONDecodeError):
            json.loads(line)
    assert json.loads(json.dumps(item))["type"] == "file"


def deb_fixture(traversal: bool = False) -> bytes:
    archive = b"!<arch>\n"
    members = {"debian-binary": b"2.0\n"}
    for name, path in (
        ("control.tar.gz", "control"),
        ("data.tar.gz", "../marker" if traversal else "usr/share/demo.txt"),
    ):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as stream:
            value = (
                b"Package: demo\nVersion: 1.0\nArchitecture: all\n"
                if path == "control"
                else b"demo"
            )
            member = tarfile.TarInfo(path)
            member.size = len(value)
            stream.addfile(member, io.BytesIO(value))
        members[name] = buffer.getvalue()
    for name, value in members.items():
        archive += (
            f"{name:<16}{0:<12}{0:<6}{0:<6}{100644:<8}{len(value):<10}`\n".encode()
            + value
        )
        if len(value) % 2:
            archive += b"\n"
    return archive


@pytest.mark.parametrize("traversal", [True, False])
def test_binary_probe_checks_format_not_safety(traversal: bool, tmp_path: Path) -> None:
    content = deb_fixture(traversal)
    validate_deb_fixture(content)
    assert list(tmp_path.iterdir()) == []
    for broken in (
        content[:-80],
        b"not an archive",
        content.replace(b"control.tar.gz", b"missing.tar.gz"),
    ):
        with pytest.raises(ContractError):
            validate_deb_fixture(broken)


@pytest.mark.parametrize(
    "member", ["usr/share/example.txt", "../../../guard/protected.txt"]
)
def test_deb_recipe_is_valid_deterministic_and_preserves_member(member: str) -> None:
    specification = json.dumps(
        {
            "package": "score-fixture",
            "version": "1.2.3",
            "architecture": "amd64",
            "files": {member: "observed"},
        }
    )
    content = build_deb(specification)
    assert content == build_deb(specification)
    validate_deb_fixture(content)
    fixture = Fixture(kind="deb", content=specification)
    assert fixture.kind == "binary"
    assert base64.b64decode(fixture.content) == content
    assert Fixture.model_validate(fixture.model_dump()) == fixture
    offset = 8
    while offset < len(content):
        header = content[offset : offset + 60]
        length = int(header[48:58])
        data = content[offset + 60 : offset + 60 + length]
        if header.startswith(b"data.tar.gz"):
            with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                assert archive.getnames() == [member]
                assert archive.extractfile(member).read() == b"observed"
        offset += 60 + length + length % 2


@pytest.mark.parametrize(
    "specification",
    [
        "[]",
        "null",
        '{"command":"touch file"}',
        '{"files":{}}',
        '{"files":{"x":3}}',
        '{"package":"bad name","files":{"x":"ok"}}',
    ],
)
def test_deb_recipe_rejects_invalid_or_executable_preparation(
    specification: str,
) -> None:
    with pytest.raises(ValueError):
        build_deb(specification)
