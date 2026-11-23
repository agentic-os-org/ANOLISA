"""Check RPM file groups and service ownership without building the daemon."""

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
        binary = self.source / "src/target/release/ws-ckpt"
        binary.parent.mkdir(parents=True)
        shutil.copyfile(self.fixture.binary, binary)
        shutil.copyfile(COMPONENT / "README.md", self.source / "README.md")
        self.spec = (COMPONENT / "packaging/rpm/ws-ckpt.spec.in").read_text()
        self.blocks = dict(
            re.findall(
                r"^%files(?:[ \t]+(\w+))?\n(.*?)(?=^%(?:files|post|preun|postun|changelog)(?:[ \t]|$)|\Z)",
                self.spec,
                re.MULTILINE | re.DOTALL,
            )
        )

    def test_matching_packages_and_monolith_upgrade(self) -> None:
        self.assertEqual(set(self.blocks), {"", "core", "plugins", "skills"})
        header = self.spec.split("%description", 1)[0]
        for role in ("core", "plugins", "skills"):
            self.assertIn(f"Requires:       %{{name}}-{role} = %{{version}}-%{{release}}", header)
            section = self.spec.split(f"%package {role}\n", 1)[1].split("%description", 1)[0]
            self.assertIn("Conflicts:      %{name} < %{version}", section)
            if role != "core":
                self.assertIn("Requires:       %{name}-core = %{version}-%{release}", section)
        self.assertEqual(
            int(re.search(r"^%define anolis_release (\d+)$", header, re.MULTILINE).group(1)), 1
        )
        self.assertEqual(self.spec.count("Provides:       anolisa-component(ws-ckpt)"), 1)
        self.assertIn("%license LICENSE", self.blocks["core"])
        self.assertEqual(
            set(re.findall(r"^%(post|preun|postun)(?:[ \t]+(\w+))?$", self.spec, re.MULTILINE)),
            {("post", "core"), ("post", "plugins"), ("preun", "core"), ("postun", "core")},
        )
        for section in ("preun core", "postun core"):
            self.assertIn("if [ $1 -eq 0 ]; then", self.spec.split(f"%{section}\n", 1)[1])
        self.assertIn("%config(noreplace) /etc/ws-ckpt/config.toml", self.blocks["core"])

    def test_install_inventory_has_exactly_one_file_owner(self) -> None:
        buildroot = self.fixture.work / "rpm-root"
        macros = {
            "buildroot": str(buildroot),
            "name": "ws-ckpt",
            "_bindir": "/usr/bin",
            "_datadir": "/usr/share",
            "_unitdir": "/usr/lib/systemd/system",
        }

        def expand(value: str) -> str:
            for key, replacement in macros.items():
                value = value.replace(f"%{{{key}}}", replacement)
            return value

        script = expand(self.spec.split("%install\n", 1)[1].split("\n%files", 1)[0])
        result = subprocess.run(
            ["bash", "-e", "-c", script],
            cwd=self.source,
            env={
                **os.environ,
                "RPM_BUILD_ROOT": str(buildroot),
                "PATH": f"{self.fixture.traps}:{os.environ['PATH']}",
            },
            capture_output=True,
            text=True,
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
        self.assertFalse((buildroot / "usr/local/bin/ws-ckpt").exists())
        self.assertNotIn("", owners.values())
        self.assertEqual(owners["usr/bin/ws-ckpt"], "core")
        self.assertEqual(owners["usr/lib/systemd/system/ws-ckpt.service"], "core")
        unit = (buildroot / "usr/lib/systemd/system/ws-ckpt.service").read_text()
        template = (self.source / "packaging/systemd/ws-ckpt.service.in").read_text()
        self.assertEqual(unit, template.replace("{bindir}", "/usr/bin"))
        self.assertEqual(
            (buildroot / "usr/lib/systemd/system/ws-ckpt.service").stat().st_mode & 0o777,
            0o644,
        )
        self.assertIn("ExecStart=/usr/bin/ws-ckpt daemon", unit)
        self.assertIn("ExecReload=/usr/bin/ws-ckpt reload", unit)
        self.assertNotIn("/usr/local/bin/", unit)
        cron = buildroot / "usr/share/anolisa/plugins/ws-ckpt/hermes/cron.py"
        self.assertEqual(
            cron.read_bytes(), (self.source / "src/plugins/hermes/cron.py").read_bytes()
        )
        self.fixture.assert_installed_cron_uses_path_binary(cron, buildroot / "usr/bin/ws-ckpt")
        self.assertEqual(owners["usr/share/anolisa/skills/ws-ckpt/SKILL.md"], "skills")
        self.assertEqual(
            owners["usr/share/anolisa/adapters/ws-ckpt/openclaw/install-openclaw.sh"], "plugins"
        )
        self.assertEqual(
            owners["usr/share/anolisa/plugins/ws-ckpt/openclaw/dist/src/index.js"],
            "plugins",
        )


if __name__ == "__main__":
    unittest.main()
