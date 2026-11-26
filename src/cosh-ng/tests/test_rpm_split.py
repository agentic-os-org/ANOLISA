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
    r"^%(?:package|description|prep|build|install|files|pre|preun|post|postun|posttrans|changelog)(?:\s|$)",
    re.MULTILINE,
)


def section(source: str, header: str) -> str:
    """Read one exact RPM section while preserving scriptlet macro calls."""
    body = source.split(header + "\n", 1)[1]
    match = SECTION.search(body)
    return body[: match.start()] if match else body


def expand(source: str, macros: dict[str, str]) -> str:
    """Expand only the path macros used by the fixture's install recipe."""
    source = re.sub(
        r"(?ms)^%if %\{with (login_lifecycle|gateway_service)\}\n(.*?)^%endif\n",
        lambda match: match[2] if macros.get("with " + match[1]) == "1" else "",
        source,
    )
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

    def test_legacy_profile_is_explicit_and_uses_the_current_build(self) -> None:
        self.assertIn("%bcond_with alinux2", self.source)
        self.assertIn("CARGO_TARGET_DIR=target cargo build --locked --workspace --release", self.source)
        self.assertNotIn("Source1:", self.source)
        self.assertNotIn("/var/tmp/cosh-alinux2", self.source)
        self.assertIn("%if %{with alinux2} && %{with login_lifecycle}", self.source)
        pre = section(self.source, "%pre")
        self.assertIn('if [ "%{with login_lifecycle}" -eq 1 ]; then', pre)
        self.assertIn("/login-preflight check-erase || exit 1", pre)

    def test_build_does_not_consume_an_inherited_target_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            source = work / "source"
            source.mkdir()
            tools = work / "tools"
            tools.mkdir()
            cargo = tools / "cargo"
            cargo.write_text(
                "#!/bin/sh\n"
                "[ \"$*\" = 'build --locked --workspace --release' ] || exit 1\n"
                "[ \"$CARGO_TARGET_DIR\" = target ] || exit 1\n"
                "mkdir -p target/release\n"
                "printf 'current-build' > target/release/cosh-shell\n"
            )
            cargo.chmod(0o755)
            foreign = work / "foreign-cache"
            foreign.mkdir()
            (foreign / "cosh-shell").write_text("stale-build")
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"], CARGO_TARGET_DIR=str(foreign))
            subprocess.run(
                ["bash", "-e", "-c", section(self.source, "%build")],
                cwd=source,
                env=environment,
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual((source / "target/release/cosh-shell").read_text(), "current-build")
            self.assertEqual((foreign / "cosh-shell").read_text(), "stale-build")

    def test_single_package_owns_runtime_lifecycle(self) -> None:
        meta = section(self.source, "Name:           cosh-ng").split("%description", 1)[0]
        self.assertIn("Provides:       anolisa-component(cosh-ng)", meta)
        self.assertIn("%bcond_without login_lifecycle", self.source)
        self.assertNotIn("%package ", self.source)
        self.assertNotIn("Requires:       %{name}-", meta)
        self.assertNotIn("Conflicts:      %{name}", meta)
        self.assertNotIn("Obsoletes:", self.source)
        self.assertRegex(self.source, r"(?m)^%define anolis_release 1$")
        for header in ("%preun", "%post -p <lua>", "%postun", "%posttrans"):
            self.assertIn(header + "\n", self.source)
        self.assertIn('if [ "$1" -eq 0 ]; then', section(self.source, "%preun"))

    def test_install_recipe_owns_every_file_once(self) -> None:
        for custom, enabled, gateway in (
            (False, True, True), (True, True, True),
            (False, False, True), (True, False, True),
            (False, False, False), (True, False, False),
        ):
            with (
                self.subTest(custom_roles=custom, lifecycle=enabled, gateway=gateway),
                tempfile.TemporaryDirectory() as temporary,
            ):
                work = Path(temporary)
                source = work / "source"
                source.mkdir()
                stage = work / "buildroot"
                for relative in (
                    "LICENSE",
                    "README.md",
                    "packaging/payload.sh",
                    "packaging/login/login-preflight",
                    "packaging/login/login-capability.json",
                    "packaging/bash/cosh-integration.bash",
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
                    "with login_lifecycle": "1" if enabled else "0",
                    "with gateway_service": "1" if gateway else "0",
                    "buildroot": str(stage),
                    "_bindir": "/usr/bin",
                    "_docdir": "/usr/share/doc",
                    "_libexecdir": "/usr/libexec",
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
                self.assertFalse((stage / "usr/bin/cosh-switch").exists())
                self.assertEqual(len(files), 11 + int(enabled) + int(gateway))
                proof = stage / "usr/share/cosh/login-capability.json"
                self.assertEqual(proof.is_file(), enabled)
                if enabled:
                    self.assertEqual(proof.stat().st_mode & 0o777, 0o644)
                self.assertFalse((stage / "etc/profile.d").exists())
                self.assertFalse((stage / "etc/bashrc").exists())
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
                self.assertEqual(unit.is_file(), gateway)
                if gateway:
                    self.assertIn(macros["_libexecdir_cosh"] + "/cosh-gateway", unit.read_text())
                self.assertEqual(
                    os.readlink(stage / "usr/bin/cosh"), macros["_libexecdir_cosh"] + "/cosh-shell"
                )
                preflight = stage / "usr/libexec/cosh/login-preflight"
                self.assertIn(macros["_libexecdir_cosh"] + "/cosh-shell", preflight.read_text())
                resources = stage / macros["_libexecdir_cosh"].lstrip("/") / "resources.conf"
                self.assertEqual(
                    resources.read_text().splitlines(),
                    [
                        "profile=system",
                        "skill_dir=" + macros["_skilldir_cosh"],
                        "extension_dir=/usr/share/anolisa/extensions",
                    ],
                )
                self.assertEqual(resources.stat().st_mode & 0o777, 0o644)

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
            for enabled in (True, False):
                command = ["rpmspec", "-q", "--rpms", "--provides"]
                if not enabled:
                    command.extend(["--define", "_without_login_lifecycle 1"])
                command.extend(
                    [
                        "--define", "_unitdir /usr/lib/systemd/system",
                        "--define", "systemd_preun() %{nil}", str(spec),
                    ]
                )
                provides = subprocess.run(
                    command, check=True, capture_output=True, text=True, timeout=30,
                ).stdout.splitlines()
                self.assertEqual("cosh-login-lifecycle-v1" in provides, enabled)

            legacy = ["rpmspec", "-q", "--rpms", "--provides", "--define", "_with_alinux2 1", str(spec)]
            provides = subprocess.run(legacy, check=True, capture_output=True, text=True, timeout=30).stdout.splitlines()
            self.assertNotIn("cosh-login-lifecycle-v1", provides)
            expanded = subprocess.run(["rpmspec", "-P", "--define", "_with_alinux2 1", str(spec)], check=True, capture_output=True, text=True, timeout=30).stdout
            self.assertNotIn("if systemd", expanded)
            self.assertNotIn("/cosh-gateway@.service", section(expanded, "%files"))
            for capability in ("login_lifecycle", "gateway_service"):
                rejected = subprocess.run(["rpmspec", "-P", "--define", "_with_alinux2 1", "--define", "_with_" + capability + " 1", str(spec)], capture_output=True, text=True, timeout=30)
                self.assertTrue(rejected.returncode != 0 or "error:" in rejected.stderr)
                if rejected.returncode == 0:
                    prep = section(rejected.stdout, "%prep").split("%setup", 1)[0]
                    rejected_prep = subprocess.run(["bash", "-e", "-c", prep], capture_output=True, text=True, timeout=30)
                    self.assertNotEqual(rejected_prep.returncode, 0)


if __name__ == "__main__":
    unittest.main()
