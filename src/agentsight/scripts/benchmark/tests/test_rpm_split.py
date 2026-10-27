"""Check physical RPM ownership and compact source payloads without compilation."""

import fnmatch
import hashlib
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SPEC = ROOT / "packaging/rpm/agentsight.spec.in"


def sections(text):
    result = {}
    current = None
    for line in text.splitlines():
        match = re.match(
            r"^%(package|description|files|post|preun|postun|prep|build|install|changelog)(?:\s+(.*))?$",
            line,
        )
        if match:
            kind, suffix = match.groups()
            name = "agentsight" if not suffix else "agentsight-" + suffix
            current = kind, name
            result[current] = []
        elif current:
            result[current].append(line)
    return {key: "\n".join(value) for key, value in result.items()}


class RpmSplitTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="agentsight-rpm-split-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.source = self.work / "source"
        for source in (
            "target/release/agentsight",
            "agentsight.json",
            "README.md",
            "README_zh.md",
            "LICENSE",
            "dsh-plugin/package.json",
            "dsh-plugin/cordis.patch.yml",
            "dsh-plugin/lib/index.js",
            "dsh-plugin/lib/types/index.d.ts",
        ):
            path = self.source / source
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        shutil.copytree(ROOT / "packaging/systemd", self.source / "packaging/systemd")
        helper = self.source / "scripts/copy-enforcer.py"
        helper.parent.mkdir()
        shutil.copyfile(ROOT / "scripts/copy-enforcer.py", helper)
        self.enforcer = self.work / "enforcer"
        self.enforcer.write_bytes(b"attested fixture")
        self.enforcer.chmod(0o755)
        Path(str(self.enforcer) + ".sha256").write_text(
            hashlib.sha256(self.enforcer.read_bytes()).hexdigest()
        )
        self.payload = self.work / "payload"

    def stage(self):
        environment = os.environ.copy()
        environment.update(
            AGENTSIGHT_PROJECT_ROOT=str(self.source), ENFORCER_BIN=str(self.enforcer)
        )
        return subprocess.run(
            [
                "bash",
                str(ROOT / "packaging/rpm/stage-rpm-payload.sh"),
                str(self.payload),
            ],
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_emitted_packages_dependencies_and_service_ownership(self):
        text = SPEC.read_text()
        parsed = sections(text)
        self.assertEqual(
            {name for kind, name in parsed if kind == "files"},
            {"agentsight", "agentsight-core", "agentsight-plugins"},
        )
        header = text.split("%description", 1)[0]
        self.assertIn("Requires:       %{name}-core = %{version}-%{release}", header)
        self.assertIn("Provides:       anolisa-component(agentsight)", header)
        core = parsed["package", "agentsight-core"]
        self.assertIn("Conflicts:      agentsight < %{version}", core)
        self.assertNotIn("anolisa-component", core)
        self.assertIn(
            "Requires:       %{name}-core = %{version}-%{release}",
            parsed["package", "agentsight-plugins"],
        )
        self.assertNotIn("%post\n", text)
        self.assertIn("if ! rpm -q agentsight", parsed["post", "agentsight-core"])
        for kind in ("post", "preun", "postun"):
            self.assertIn((kind, "agentsight-core"), parsed)
            self.assertNotIn((kind, "agentsight-plugins"), parsed)
        self.assertIn("%define anolis_release 1", text)

    def test_source_payload_and_installed_file_ownership_are_complete(self):
        result = self.stage()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (self.payload / "agentsight-enforcer").read_bytes(),
            self.enforcer.read_bytes(),
        )
        self.assertTrue((self.payload / "dsh-plugin/lib/types/index.d.ts").is_file())
        parsed = sections(SPEC.read_text())
        self.assertNotIn("sed ", parsed["install", "agentsight"])
        buildroot = self.work / "buildroot"
        macros = {
            "buildroot": str(buildroot),
            "_unitdir": "/usr/lib/systemd/system",
            "_docdir": "/usr/share/doc",
            "_datadir": "/usr/share",
            "_bindir": "/usr/bin",
            "_sysconfdir": "/etc",
        }

        def expand(value):
            for name, replacement in macros.items():
                value = value.replace("%{" + name + "}", replacement)
            return value.replace("$RPM_BUILD_ROOT", str(buildroot))

        result = subprocess.run(
            ["sh", "-eu", "-c", expand(parsed["install", "agentsight"])],
            cwd=self.payload,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        files = {
            "/" + str(path.relative_to(buildroot))
            for path in buildroot.rglob("*")
            if path.is_file()
        }
        owners = {}
        for package in ("agentsight", "agentsight-core", "agentsight-plugins"):
            for line in parsed["files", package].splitlines():
                if not line.strip() or line.startswith("%defattr"):
                    continue
                pattern = re.sub(r"%(?:attr\([^)]*\)|config\([^)]*\)|doc|license)\s*", "", line)
                pattern = expand(pattern).strip()
                for path in files:
                    if fnmatch.fnmatchcase(path, pattern) or path.startswith(
                        pattern.rstrip("/") + "/"
                    ):
                        self.assertNotIn(path, owners, (path, owners.get(path), package))
                        owners[path] = package
        self.assertEqual(set(owners), files)
        self.assertEqual(owners["/usr/bin/agentsight"], "agentsight-core")
        self.assertEqual(
            owners["/usr/share/agentsight/adapters/dsh/lib/index.js"],
            "agentsight-plugins",
        )
        self.assertFalse((buildroot / "usr/share/anolisa/components").exists())
        self.assertNotIn("agentsight", owners.values())
        self.assertFalse((self.payload / "component.toml").exists())
        self.assertFalse((buildroot / "usr/local/bin").exists())
        for name in ("agentsight", "agentsight-enforcer", "agentsight-start"):
            self.assertEqual((buildroot / "usr/bin" / name).stat().st_mode & 0o777, 0o755)
        for relative in (
            "usr/lib/systemd/system/agentsight.service",
            "usr/lib/systemd/system/agentsight-enforcer.service",
            "usr/share/doc/agentsight/README.md",
            "usr/share/doc/agentsight/README_zh.md",
            "usr/share/doc/agentsight/LICENSE",
            "etc/agentsight/config.json",
        ):
            self.assertEqual((buildroot / relative).stat().st_mode & 0o777, 0o644)
        for unit, binary in (
            ("agentsight.service", "agentsight-start"),
            ("agentsight-enforcer.service", "agentsight-enforcer"),
        ):
            text = (buildroot / "usr/lib/systemd/system" / unit).read_text()
            self.assertIn("ExecStart=/usr/bin/" + binary, text)
            self.assertNotIn("/usr/local/bin/", text)
            template = (self.payload / "packaging/systemd" / (unit + ".in")).read_text()
            self.assertEqual(text, template.replace("@BINDIR@", "/usr/bin"))
        launcher = (buildroot / "usr/bin/agentsight-start").read_text()
        self.assertIn("/usr/bin/agentsight trace &", launcher)
        self.assertIn("/usr/bin/agentsight serve --host", launcher)
        template = (self.payload / "packaging/systemd/agentsight-start.sh.in").read_text()
        self.assertEqual(
            launcher,
            template.replace("@AGENTSIGHT_COMMAND@", "/usr/bin/agentsight").replace(
                "@CONFIG_ARGUMENT@", ""
            ),
        )

    def test_rpm_bindir_override_renders_launcher_and_units(self):
        staged = self.stage()
        self.assertEqual(staged.returncode, 0, staged.stdout + staged.stderr)
        buildroot = self.work / "custom-buildroot"
        install = sections(SPEC.read_text())["install", "agentsight"]
        macros = {
            "buildroot": str(buildroot),
            "_unitdir": "/usr/lib/systemd/system",
            "_docdir": "/usr/share/doc",
            "_datadir": "/usr/share",
            "_bindir": "/opt/agentsight/bin",
            "_sysconfdir": "/etc",
        }
        for name, value in macros.items():
            install = install.replace("%{" + name + "}", value)
        install = install.replace("$RPM_BUILD_ROOT", str(buildroot))
        result = subprocess.run(
            ["sh", "-eu", "-c", install], cwd=self.payload, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        launcher = buildroot / "opt/agentsight/bin/agentsight-start"
        self.assertIn("/opt/agentsight/bin/agentsight trace &", launcher.read_text())
        self.assertEqual(launcher.stat().st_mode & 0o777, 0o755)
        for name, binary in (
            ("agentsight.service", "agentsight-start"),
            ("agentsight-enforcer.service", "agentsight-enforcer"),
        ):
            unit = buildroot / "usr/lib/systemd/system" / name
            self.assertIn(f"ExecStart=/opt/agentsight/bin/{binary}", unit.read_text())
            self.assertEqual(unit.stat().st_mode & 0o777, 0o644)

    def test_missing_plugin_and_stale_enforcer_receipt_fail(self):
        (self.source / "dsh-plugin/lib/index.js").unlink()
        result = self.stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing AgentSight RPM plugin source", result.stderr)
        self.assertFalse(self.payload.exists())
        (self.source / "dsh-plugin/lib/index.js").write_text("fixture")
        self.enforcer.write_bytes(b"changed after attestation")
        result = self.stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("enforcer SHA256 receipt mismatch", result.stderr)
        self.assertFalse((self.payload / "agentsight-enforcer").exists())

    def test_artifact_verifier_distinguishes_all_package_roles(self):
        verifier = ROOT / "packaging/rpm/verify-rpm-package.sh"
        inventories = {
            "agentsight": [],
            "agentsight-core": [
                "/usr/bin/agentsight",
                "/usr/bin/agentsight-enforcer",
                "/usr/bin/agentsight-start",
                "/usr/lib/systemd/system/agentsight.service",
                "/usr/lib/systemd/system/agentsight-enforcer.service",
                "/etc/agentsight/config.json",
            ],
            "agentsight-plugins": [
                "/usr/share/agentsight/adapters/dsh/lib/index.js",
                "/usr/share/agentsight/adapters/dsh/package.json",
                "/usr/share/agentsight/adapters/dsh/cordis.patch.yml",
            ],
        }
        listing = self.work / "rpm-files"
        for package, paths in inventories.items():
            with self.subTest(package=package):
                listing.write_text("\n".join(paths) + "\n")
                result = subprocess.run(
                    [
                        "bash",
                        str(verifier),
                        "--package",
                        package,
                        "--file-list",
                        str(listing),
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                listing.write_text(
                    "\n".join([*paths, "/usr/share/anolisa/components/agentsight/component.toml"])
                    + "\n"
                )
                result = subprocess.run(
                    ["bash", str(verifier), "--package", package, "--file-list", str(listing)],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("must not own ANOLISA discovery contracts", result.stderr)

    @unittest.skipUnless(shutil.which("rpmspec"), "rpmspec is unavailable")
    def test_rpmspec_emits_all_three_binary_packages(self):
        spec = self.work / "agentsight.spec"
        spec.write_text(SPEC.read_text().replace("@VERSION@", "9.8.7"))
        command = ["rpmspec", "-q", "--rpms", "--qf", "%{NAME}\n"]
        for macro in (
            "_unitdir /usr/lib/systemd/system",
            "systemd_post() %{nil}",
            "systemd_preun() %{nil}",
            "systemd_postun() %{nil}",
        ):
            command.extend(["--define", macro])
        result = subprocess.run(command + [str(spec)], capture_output=True, text=True, check=True)
        self.assertEqual(
            set(result.stdout.splitlines()),
            {"agentsight", "agentsight-core", "agentsight-plugins"},
        )


if __name__ == "__main__":
    unittest.main()
