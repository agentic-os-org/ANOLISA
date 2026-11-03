#!/usr/bin/env python3
"""Exercise RPM file ownership with fake binaries, without compiling or installing."""

import fnmatch
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "packaging/rpm/cosh-ng.spec.in"
SECTION = re.compile(
    r"^%(?:package|description|prep|build|install|files|preun|post|postun|posttrans|changelog)(?:\s|$)",
    re.MULTILINE,
)


def section(source: str, header: str) -> str:
    """Read one exact RPM section while preserving scriptlet macro calls."""
    body = source.split(header + "\n", 1)[1]
    match = SECTION.search(body)
    return body[: match.start()] if match else body


def expand(source: str, macros: dict[str, str]) -> str:
    """Expand only the path macros used by the fixture's install recipe."""
    for _ in range(8):
        updated = re.sub(r"%\{([^}]+)\}", lambda m: macros.get(m[1], m[0]), source)
        if updated == source:
            break
        source = updated
    return source.replace("%%", "%")


def file_entries(source: str, header: str, macros: dict[str, str]) -> list[tuple[str, bool]]:
    """List explicit ownership patterns and recursive directory entries."""
    entries: list[tuple[str, bool]] = []
    for line in section(source, header).splitlines():
        line = expand(line.strip(), macros)
        if not line or line.startswith("#"):
            continue
        path = line.rsplit(None, 1)[-1]
        if path.startswith("/"):
            entries.append((path, "%dir" not in line and path.endswith("/")))
    return entries


class RpmPackageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SPEC.read_text()

    def test_single_package_owns_runtime_lifecycle(self) -> None:
        meta = section(self.source, "Name:           cosh-ng").split("%description", 1)[0]
        self.assertIn("Provides:       anolisa-component(cosh-ng)", meta)
        self.assertNotIn("%package ", self.source)
        self.assertNotIn("Requires:       %{name}-", meta)
        self.assertNotIn("Conflicts:      %{name}", meta)
        self.assertNotIn("Obsoletes:", self.source)
        self.assertRegex(self.source, r"(?m)^%define anolis_release 1$")
        for header in ("%preun", "%post -p <lua>", "%postun", "%posttrans"):
            self.assertIn(header + "\n", self.source)
        self.assertIn('if [ "$1" -eq 0 ]; then', section(self.source, "%preun"))

    def test_install_recipe_owns_every_file_once(self) -> None:
        for custom in (False, True):
            with self.subTest(custom_roles=custom), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                source = work / "source"
                source.mkdir()
                stage = work / "buildroot"
                for relative in (
                    "LICENSE",
                    "README.md",
                    "packaging/payload.sh",
                    "packaging/systemd/cosh-gateway@.service.in",
                    "skills/manage-task-checkpoints/SKILL.md",
                ):
                    target = source / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ROOT / relative, target)
                binaries = source / "target/release"
                binaries.mkdir(parents=True)
                for name in ("cosh-cli", "cosh-core", "cosh-gateway", "cosh-shell"):
                    binary = binaries / name
                    binary.write_text("#!/bin/sh\nexit 0\n")
                    binary.chmod(0o755)
                macros = {
                    "name": "cosh-ng",
                    "buildroot": str(stage),
                    "_bindir": "/usr/bin",
                    "_docdir": "/usr/share/doc",
                    "_datadir": "/usr/share",
                    "_unitdir": "/usr/lib/systemd/system",
                    "_libexecdir_cosh": (
                        "/opt/cosh/runtime" if custom else "/usr/libexec/anolisa/cosh-ng"
                    ),
                    "_skilldir_cosh": "/opt/cosh/skills" if custom else "/usr/share/anolisa/skills",
                }
                subprocess.run(
                    ["bash", "-e", "-c", expand(section(self.source, "%install"), macros)],
                    cwd=source,
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                ownership = file_entries(self.source, "%files", macros)
                files = [p for p in stage.rglob("*") if p.is_symlink() or p.is_file()]
                self.assertEqual(len(files), 10)
                for path in files:
                    installed = "/" + str(path.relative_to(stage))
                    matches = sum(
                        (
                            installed.startswith(pattern)
                            if recursive
                            else fnmatch.fnmatchcase(installed, pattern)
                        )
                        for pattern, recursive in ownership
                    )
                    self.assertEqual(matches, 1, installed)
                self.assertFalse((stage / "usr/share/anolisa/components").exists())
                unit = stage / "usr/lib/systemd/system/cosh-gateway@.service"
                self.assertIn(macros["_libexecdir_cosh"] + "/cosh-gateway", unit.read_text())
                self.assertEqual(
                    os.readlink(stage / "usr/bin/cosh"), macros["_libexecdir_cosh"] + "/cosh-shell"
                )
                self.assertIn("cosh-ng)", (stage / "usr/bin/cosh-switch").read_text())

    @unittest.skipUnless(
        shutil.which("rpmspec"), "rpmspec is unavailable; fixture inventory is still checked"
    )
    def test_rpmspec_declares_one_binary_package(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            spec = Path(temporary) / "cosh-ng.spec"
            spec.write_text(self.source.replace("@VERSION@", "9.8.7"))
            result = subprocess.run(
                [
                    "rpmspec",
                    "-q",
                    "--rpms",
                    "--qf",
                    "%{NAME}\n",
                    "--define",
                    "_unitdir /usr/lib/systemd/system",
                    "--define",
                    "systemd_preun() %{nil}",
                    str(spec),
                ],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.stdout.splitlines(), ["cosh-ng"])


if __name__ == "__main__":
    unittest.main()
