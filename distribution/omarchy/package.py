#!/usr/bin/env python3
"""Assemble the Omarchy installer from verified, already built Linux programs."""

import gzip
import hashlib
import json
import shutil
import tarfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODULES = ("runtime.py", "setup.py", "agent_installation.py", "system_install.py", "i18n.py")
DOCUMENTS = (
    "README.md",
    "README_zh.md",
    "BUILDING.md",
    "CONTRIBUTING.md",
    "CONTRIBUTING_zh.md",
    "CHANGELOG.md",
    "CHANGELOG_zh.md",
)


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def settings() -> dict:
    return json.loads((HERE / "release.json").read_text(encoding="utf-8"))


def executable(relative: str) -> bool:
    return (
        relative == "install"
        or relative.startswith("payload/bin/")
        or relative.rsplit("/", 1)[-1] in {"cosh-core", "cosh-shell", "cosh-gateway"}
    )


def assemble(
    payload: Path, destination: Path, receipt: dict, components: dict, epoch: int
) -> tuple[Path, dict]:
    """Record all shipped files and normalize archive ownership, modes and time."""
    version = settings()["version"]
    stage = payload.parent / f"anolisa-omarchy-{version}"
    stage.mkdir()
    shutil.move(str(payload), stage / "payload")
    payload = stage / "payload"
    shutil.copytree(
        HERE / "plugin", payload / "plugin", ignore=shutil.ignore_patterns("__pycache__")
    )
    for name in MODULES:
        shutil.copy2(HERE / name, payload / name)
    shutil.copy2(HERE / "translations.json", payload)
    for name in ("install", "install.py", "verify_release.py", *DOCUMENTS):
        shutil.copy2(HERE / name, stage / name)
    # Ship the canonical manual as local files; source wrappers preserve old links.
    for language, name, other in (("en", "USER_GUIDE.md", "zh"), ("zh", "USER_GUIDE_zh.md", "en")):
        guide = HERE.parents[1] / f"docs/user-guide/{language}/omarchy/index.md"
        counterpart = "USER_GUIDE_zh.md" if other == "zh" else "USER_GUIDE.md"
        text = guide.read_text(encoding="utf-8").replace(
            f"../../{other}/omarchy/index.md", counterpart
        )
        (stage / name).write_text(text, encoding="utf-8", newline="\n")
    shutil.copy2(HERE.parents[1] / "LICENSE", stage / "LICENSE")
    licenses = payload / "share/licenses/anolisa-omarchy"
    licenses.mkdir(parents=True, exist_ok=True)
    shutil.copy2(HERE.parents[1] / "LICENSE", licenses / "ANOLISA-APACHE-2.0")
    files = {}
    for path in sorted(payload.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"Payload symlink is not allowed: {path.relative_to(payload)}")
        if path.is_file():
            relative = path.relative_to(stage).as_posix()
            path.chmod(0o755 if executable(relative) else 0o644)
            files[path.relative_to(payload).as_posix()] = digest(path)
    # Bind sudo inputs to the installer itself, not to a later user-owned manifest.
    # The caller must obtain this installer from a trusted, verified release archive.
    pins = {name: files[name] for name in ("system_install.py", "bin/agent-sec-daemon")}
    installer = stage / "install.py"
    source = installer.read_text(encoding="utf-8")
    if source.count("SYSTEM_INPUT_DIGESTS = {}") != 1:
        raise ValueError("Privileged input pin marker is missing or ambiguous")
    installer.write_text(
        source.replace("SYSTEM_INPUT_DIGESTS = {}", "SYSTEM_INPUT_DIGESTS = " + repr(pins)),
        encoding="utf-8",
        newline="\n",
    )
    module_pins = {name: files[name] for name in MODULES if name != "system_install.py"}
    source = installer.read_text(encoding="utf-8")
    if source.count("INSTALL_MODULE_DIGESTS = {}") != 1:
        raise ValueError("Installer module pin marker is missing or ambiguous")
    installer.write_text(
        source.replace(
            "INSTALL_MODULE_DIGESTS = {}", "INSTALL_MODULE_DIGESTS = " + repr(module_pins)
        ),
        encoding="utf-8",
        newline="\n",
    )
    install_files = {
        path.relative_to(stage).as_posix(): digest(path)
        for path in sorted(stage.iterdir())
        if path.is_file()
    }
    manifest = {
        "schema": 1,
        "version": version,
        "platform": "linux-x86_64",
        "source_commit": receipt["commit"],
        "source": receipt,
        "binary_provenance": {"kind": "pinned_official_inputs_and_source_build"},
        "components": components,
        "files": files,
        "install_files": install_files,
        "optional_components": {"qodercli": "Downloaded separately from Qoder's official source"},
    }
    (stage / "bundle.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    archive = destination / f"anolisa-omarchy-{version}-linux-x86_64.tar.gz"

    def normalize(info: tarfile.TarInfo) -> tarfile.TarInfo:
        relative = info.name.removeprefix(stage.name + "/")
        info.mode = 0o755 if info.isdir() or executable(relative) else 0o644
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mtime = epoch
        info.pax_headers = {}
        return info

    # Gzip headers must not inherit the temporary filename or wall-clock time.
    with (
        archive.open("wb") as stream,
        gzip.GzipFile(filename="", fileobj=stream, mode="wb", mtime=epoch) as zipped,
    ):
        with tarfile.open(fileobj=zipped, mode="w", format=tarfile.PAX_FORMAT) as tar:
            tar.add(stage, arcname=stage.name, filter=normalize)
    return archive, manifest


if __name__ == "__main__":
    # The public build entry owns input verification and atomic publication.
    from release import main

    main()
