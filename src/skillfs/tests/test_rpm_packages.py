"""Check real RPM ownership using fixture files, without compiling Linux code."""

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
        binary = self.source / "target/release/skillfs"
        binary.parent.mkdir(parents=True)
        shutil.copyfile(self.fixture.binary, binary)
        shutil.copyfile(COMPONENT / "CHANGELOG.md", self.source / "CHANGELOG.md")
        self.spec = (COMPONENT / "packaging/rpm/skillfs.spec.in").read_text()
        self.blocks = dict(
            re.findall(
                r"^%files(?:[ \t]+(\w+))?\n(.*?)(?=^%(?:files|post|preun|postun|changelog)(?:[ \t]|$)|\Z)",
                self.spec,
                re.MULTILINE | re.DOTALL,
            )
        )

    def test_matching_packages_and_monolith_upgrade(self) -> None:
        self.assertEqual(set(self.blocks), {"", "core", "skills"})
        header = self.spec.split("%description", 1)[0]
        for role in ("core", "skills"):
            self.assertIn(f"Requires:       %{{name}}-{role} = %{{version}}-%{{release}}", header)
            section = self.spec.split(f"%package {role}\n", 1)[1].split("%description", 1)[0]
            self.assertIn("Conflicts:      %{name} < %{version}", section)
            if role == "skills":
                self.assertIn("Requires:       %{name}-core = %{version}-%{release}", section)
        self.assertEqual(
            int(re.search(r"^%define anolis_release (\d+)$", header, re.MULTILINE).group(1)), 1
        )
        self.assertEqual(self.spec.count("Provides:       anolisa-component(skillfs)"), 1)
        self.assertIn("Requires:       fuse3", self.spec.split("%package core", 1)[1])
        self.assertIn("%license LICENSE", self.blocks["core"])
        self.assertNotIn("%license", self.blocks[""])

    def test_install_inventory_has_exactly_one_file_owner(self) -> None:
        buildroot = self.fixture.work / "rpm-root"
        macros = {
            "buildroot": str(buildroot),
            "name": "skillfs",
            "_bindir": "/usr/bin",
            "_datadir": "/usr/share",
        }

        def expand(value: str) -> str:
            for key, replacement in macros.items():
                value = value.replace(f"%{{{key}}}", replacement)
            return value

        script = expand(self.spec.split("%install\n", 1)[1].split("\n%files", 1)[0])
        result = subprocess.run(
            ["bash", "-e", "-c", script], cwd=self.source, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        owners: dict[str, str] = {}
        for role, block in self.blocks.items():
            for line in block.splitlines():
                path = re.sub(r"^%(?:attr|config)\([^)]*\)\s*", "", line.strip())
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
        self.assertFalse((buildroot / "usr/local/bin/skillfs").exists())
        self.assertNotIn("", owners.values())
        self.assertEqual(owners["usr/bin/skillfs"], "core")
        self.assertEqual(owners["usr/share/anolisa/skills/skillfs-mount/SKILL.md"], "skills")
        self.assertNotIn("%post", self.spec)


if __name__ == "__main__":
    unittest.main()
