"""Assemble native AW archives and independent Provider packages."""

from __future__ import annotations

import gzip
import json
import os
import re
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any, BinaryIO, Callable

from common import Cancellation, copy_file, digest

COMPONENTS = {
    "core": "aw-core",
    "sec-core": "aw-provider-sec-core",
    "tokenless": "aw-provider-tokenless",
}
MACHINES = {"x86_64": 62, "aarch64": 183}
TEMPLATES = Path(__file__).resolve().parent / "templates"


def components(selection: str | list[str] | tuple[str, ...]) -> tuple[str, ...]:
    """Validate an explicit, nonempty list of independent packages."""
    values = [selection] if isinstance(selection, str) else selection
    selected = tuple(word for value in values for word in value.split())
    if (
        not selected
        or len(selected) != len(set(selected))
        or any(v not in COMPONENTS for v in selected)
    ):
        raise ValueError("components must be a nonempty unique list of core, sec-core, tokenless")
    return selected


def validate(
    version: str, selection: str | list[str] | tuple[str, ...], arch: str, source: str
) -> None:
    """Reject identifiers that could change archive paths or recorded provenance."""
    number = r"(?:0|[1-9][0-9]*)"
    match = re.fullmatch(
        rf"{number}\.{number}\.{number}(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
        version,
    )
    if match is None or (
        match[1]
        and any(v.isdigit() and len(v) > 1 and v.startswith("0") for v in match[1].split("."))
    ):
        raise ValueError("version must be SemVer")
    components(selection)
    if arch not in MACHINES:
        raise ValueError("unsupported Linux architecture")
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("source commit must be 40 lowercase hex characters")


def _elf(path: Path, arch: str) -> None:
    with path.open("rb") as source:
        header = source.read(20)
    if (
        len(header) != 20
        or header[:6] != b"\x7fELF\x02\x01"
        or int.from_bytes(header[18:20], "little") != MACHINES[arch]
        or not path.stat().st_mode & 0o111
    ):
        raise ValueError(f"expected native executable ELF for {arch}: {path}")


def _add(
    root: Path,
    name: str,
    source: Path,
    executable: bool,
    files: dict[str, Any],
    arch: str,
    cancel: Cancellation,
) -> None:
    if executable:
        _elf(source, arch)
    mode = 0o755 if executable else 0o644
    destination = root / "payload" / name
    copy_file(source, destination, cancel, mode)
    files[name] = {"sha256": digest(destination, cancel), "mode": mode}


def _core(
    repo: Path, root: Path, version: str, source_commit: str, arch: str, cancel: Cancellation
) -> None:
    """AW core is an ordinary native installation, outside the Provider database."""
    _elf(repo / "src/aw/target/release/aw", arch)
    copy_file(repo / "src/aw/target/release/aw", root / "bin/aw", cancel, 0o755)
    for notice in ("LICENSE", "NOTICE"):
        copy_file(repo / notice, root / "share/doc/aw" / notice, cancel, 0o644)
    receipt = root / "share/doc/aw/source.json"
    receipt.write_text(
        json.dumps(
            {
                "package_id": "aw-core",
                "version": version,
                "source_commit": source_commit,
                "os": "linux",
                "arch": arch,
            },
            indent=2,
        )
    )
    copy_file(Path(__file__).with_name("core.Makefile"), root / "Makefile", cancel, 0o644)
    copy_file(repo / "src/aw/packaging/install.mk", root / "packaging/install.mk", cancel, 0o644)


def _provider(
    repo: Path,
    root: Path,
    component: str,
    version: str,
    source_commit: str,
    arch: str,
    cancel: Cancellation,
) -> None:
    """A Provider owns its bridge and data; backend executables remain external."""
    role = component.removeprefix("aw-provider-")
    source = repo / "providers" / ("agent-sec-core" if role == "sec-core" else role)
    files: dict[str, Any] = {}
    _add(
        root,
        f"libexec/aw/providers/{role}/{component}",
        source / "target/release" / component,
        True,
        files,
        arch,
        cancel,
    )
    for resource in ("schemas", "examples"):
        resources = source / resource
        if resources.is_dir():
            for path in sorted(resources.rglob("*")):
                if "__pycache__" in path.relative_to(resources).parts or path.suffix == ".pyc":
                    continue
                if path.is_symlink():
                    raise ValueError(f"Provider resource must be an ordinary file: {path}")
                if path.is_file():
                    _add(
                        root,
                        f"share/aw/providers/{role}/{resource}/{path.relative_to(resources).as_posix()}",
                        path,
                        False,
                        files,
                        arch,
                        cancel,
                    )
    for notice in ("LICENSE", "NOTICE"):
        _add(root, f"share/doc/{component}/{notice}", repo / notice, False, files, arch, cancel)
    description = {
        "schema_version": 1,
        "package_id": component,
        "version": version,
        "source_commit": source_commit,
        "os": "linux",
        "arch": arch,
        "dependencies": [],
        "provider": {"protocol": "aw-provider/v1alpha1"},
        "files": dict(sorted(files.items())),
    }
    (root / "package.json").write_text(json.dumps(description, indent=2))
    copy_file(TEMPLATES / f"{role}.json", root / "templates" / f"{role}.json", cancel, 0o644)
    copy_file(
        TEMPLATES / "sec-core-tokenless.json",
        root / "templates/sec-core-tokenless.json",
        cancel,
        0o644,
    )


def _readme(repo: Path, language: str) -> str:
    text = (repo / f"docs/user-guide/{language}/aw/installation.md").read_text()
    text = text.replace("../../zh/aw/installation.md", "README_zh.md")
    text = text.replace("../../en/aw/installation.md", "README.md")
    locale = "/zh" if language == "zh" else ""
    for name in ("configuration", "tokenless", "security", "operations", "packages"):
        text = text.replace(
            f"]({name}.md)", f"](https://agentic-os.sh{locale}/docs/user-guide/aw/{name}/)"
        )
    return text.replace(
        f"../../../../contribute/{language}/aw/building.md",
        f"https://agentic-os.sh{locale}/docs/contribute/aw/building/",
    )


class _Writer:
    """Check signals during compression as well as between archive members."""

    def __init__(self, output: BinaryIO, cancel: Cancellation) -> None:
        self.output, self.cancel = output, cancel

    def write(self, data: bytes) -> int:
        self.cancel.check()
        return self.output.write(data)

    def flush(self) -> None:
        self.output.flush()


def _archive(root: Path, destination: Path, cancel: Cancellation) -> None:
    with destination.open("xb") as output:
        with gzip.GzipFile(
            filename="", fileobj=_Writer(output, cancel), mode="wb", mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                archive.add(root, arcname=root.name)
    cancel.check()


def publish(
    artifacts: list[Path],
    output: Path,
    cancel: Cancellation,
    link: Callable[[Path, Path], None] = os.link,
) -> list[Path]:
    """Publish without replacement and roll back only links this operation created."""
    published = []
    try:
        for artifact in artifacts:
            cancel.check()
            target = output / artifact.name
            link(artifact, target)
            published.append(target)
        cancel.check()
        return published
    except BaseException:
        for target in reversed(published):
            try:
                target.unlink()
            except OSError as error:
                print(f"aw-packages: rollback could not remove {target}: {error}", file=sys.stderr)
        raise


def assemble(
    repo: Path,
    version: str,
    source_commit: str,
    output: Path,
    selection: str | list[str] | tuple[str, ...],
    arch: str,
    cancel: Cancellation,
) -> list[Path]:
    """Publish ordinary core archives and independently managed Provider packages."""
    validate(version, selection, arch, source_commit)
    cancel.check()
    if not output.is_absolute():
        raise ValueError("output must be absolute")
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError("output directory must be empty")
    with tempfile.TemporaryDirectory(prefix="stage-", dir=output) as temporary:
        stage = Path(temporary)
        roots = {}
        for role in components(selection):
            component = COMPONENTS[role]
            root = stage / f"{component}-{version}-linux-{arch}"
            root.mkdir()
            if component == "aw-core":
                _core(repo, root, version, source_commit, arch, cancel)
            else:
                _provider(repo, root, component, version, source_commit, arch, cancel)
            roots[component] = root
        archives = []
        for root in roots.values():
            for language, filename in (("en", "README.md"), ("zh", "README_zh.md")):
                (root / filename).write_text(_readme(repo, language))
            archive = stage / f"{root.name}.tar.gz"
            _archive(root, archive, cancel)
            archives.append(archive)
        checksum = stage / "SHA256SUMS"
        checksum.write_text("".join(f"{digest(path, cancel)}  {path.name}\n" for path in archives))
        return publish([*archives, checksum], output, cancel)[:-1]
