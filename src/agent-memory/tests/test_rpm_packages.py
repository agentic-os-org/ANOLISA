"""Check RPM payload owners and dependencies without compiling Linux code."""

import os
import re
import shutil
import subprocess
import unittest
from pathlib import Path

import test_packaging_layout as layout

COMPONENT = Path(__file__).resolve().parents[1]


class RpmPackages(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = layout.PackagingLayout("runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.source = self.fixture.source
        shutil.rmtree(self.source / "packaging/anolisa")
        binary = self.source / "target/release/agent-memory"
        binary.parent.mkdir(parents=True)
        shutil.copyfile(self.fixture.binary, binary)
        shutil.copyfile(COMPONENT / "CHANGELOG.md", self.source / "CHANGELOG.md")
        self.spec = (COMPONENT / "packaging/rpm/agent-memory.spec.in").read_text()
        self.blocks = dict(
            re.findall(
                r"^%files(?:[ \t]+(\w+))?\n(.*?)(?=^%(?:files|post|preun|postun|changelog)(?:[ \t]|$)|\Z)",
                self.spec,
                re.MULTILINE | re.DOTALL,
            )
        )

    def test_matching_packages_and_monolith_upgrade(self) -> None:
        self.assertEqual(set(self.blocks), {"", "core", "plugins"})
        header = self.spec.split("%description", 1)[0]
        for role in ("core", "plugins"):
            self.assertIn(f"Requires:       %{{name}}-{role} = %{{version}}-%{{release}}", header)
            section = self.spec.split(f"%package {role}\n", 1)[1].split("%description", 1)[0]
            self.assertIn("Conflicts:      %{name} < %{version}", section.splitlines())
            if role == "plugins":
                self.assertIn("Requires:       %{name}-core = %{version}-%{release}", section)
        self.assertEqual(
            int(re.search(r"^%define anolis_release (\d+)$", header, re.MULTILINE).group(1)), 1
        )
        provide = "Provides:       anolisa-component(agent-memory)"
        core_provide = "Provides:       anolisa-component-core(agent-memory)"
        core_header = self.spec.split("%package core\n", 1)[1].split("%description", 1)[0]
        self.assertIn("Conflicts:      anolisa < 0.3.21", core_header.splitlines())
        self.assertIn(provide, header.splitlines())
        self.assertNotIn(provide, core_header.splitlines())
        self.assertEqual(self.spec.count(provide), 1)
        self.assertNotIn(core_provide, header.splitlines())
        self.assertIn(core_provide, core_header.splitlines())
        self.assertEqual(self.spec.count(core_provide), 1)
        self.assertIn("%license LICENSE", self.blocks["core"])
        self.assertEqual(
            set(re.findall(r"^%(post|preun|postun)(?:[ \t]+(\w+))?$", self.spec, re.MULTILINE)),
            {("post", "core"), ("preun", "plugins")},
        )
        self.assertIn("%tmpfiles_create", self.spec.split("%post core\n", 1)[1])
        self.assertIn("if [ $1 -eq 0 ]; then", self.spec.split("%preun plugins\n", 1)[1])

    def test_install_inventory_has_exactly_one_file_owner(self) -> None:
        buildroot = self.fixture.work / "rpm-root"
        macros = {
            "buildroot": str(buildroot),
            "name": "agent-memory",
            "_bindir": "/usr/bin",
            "_datadir": "/usr/share",
            "_docdir": "/usr/share/doc",
            "_userunitdir": "/usr/lib/systemd/user",
            "_tmpfilesdir": "/usr/lib/tmpfiles.d",
        }

        def expand(value: str) -> str:
            for key, replacement in macros.items():
                value = value.replace(f"%{{{key}}}", replacement)
            return value

        script = expand(self.spec.split("%install\n", 1)[1].split("\n%files", 1)[0])
        result = subprocess.run(
            ["bash", "-e", "-c", script],
            cwd=self.source,
            env={**os.environ, "PATH": f"{self.fixture.traps}:{os.environ['PATH']}"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        owners: dict[str, str] = {}
        for role, block in self.blocks.items():
            for line in block.splitlines():
                path = re.sub(r"^%(?:attr|config)\([^)]*\)\s*", "", line.strip())
                if path.startswith("%doc "):
                    path = path[5:]
                if path.startswith("%dir "):
                    self.assertTrue((buildroot / expand(path[5:]).lstrip("/")).is_dir())
                elif path.startswith(("/", "%{")):
                    target = buildroot / expand(path).lstrip("/")
                    self.assertTrue(target.exists(), path)
                    files = target.rglob("*") if target.is_dir() else [target]
                    for item in files:
                        if item.is_file():
                            relative = str(item.relative_to(buildroot))
                            self.assertNotIn(relative, owners, relative)
                            owners[relative] = role
        actual = {str(p.relative_to(buildroot)) for p in buildroot.rglob("*") if p.is_file()}
        self.assertEqual(set(owners), actual)
        self.assertFalse((buildroot / "usr/share/anolisa/components").exists())
        self.assertNotIn("", owners.values())
        for path in (
            "usr/bin/agent-memory",
            "usr/share/anolisa/agent-memory/default.toml",
            "usr/share/anolisa/mcp-servers/agent-memory.json",
            "usr/lib/systemd/user/anolisa-memory@.service",
            "usr/lib/tmpfiles.d/anolisa-memory.conf",
        ):
            self.assertEqual(owners[path], "core")
        self.assertEqual(
            owners["usr/share/anolisa/adapters/agent-memory/openclaw/dist/index.js"], "plugins"
        )
        self.assertEqual(owners["usr/share/anolisa/adapters/agent-memory/manifest.json"], "plugins")
        unit_path = buildroot / "usr/lib/systemd/user/anolisa-memory@.service"
        unit = unit_path.read_text()
        template = (COMPONENT / "packaging/systemd/anolisa-memory@.service.in").read_text()
        self.assertEqual(unit, template.replace("{bindir}", "/usr/bin"))
        self.assertEqual(unit_path.stat().st_mode & 0o777, 0o644)
        self.assertIn("ExecStart=/usr/bin/agent-memory serve", unit)
        self.assertNotIn("{bindir}", unit)

        custom_root = self.fixture.work / "rpm-custom-root"
        macros["buildroot"] = str(custom_root)
        macros["_bindir"] = "/opt/agent-memory/bin"
        custom_script = expand(self.spec.split("%install\n", 1)[1].split("\n%files", 1)[0])
        result = subprocess.run(
            ["bash", "-e", "-c", custom_script],
            cwd=self.source,
            env={**os.environ, "PATH": f"{self.fixture.traps}:{os.environ['PATH']}"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        custom_unit = custom_root / "usr/lib/systemd/user/anolisa-memory@.service"
        self.assertIn("ExecStart=/opt/agent-memory/bin/agent-memory serve", custom_unit.read_text())
        self.assertNotIn(str(custom_root), custom_unit.read_text())


if __name__ == "__main__":
    unittest.main()
