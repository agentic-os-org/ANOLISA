"""Check V2 RPM payload transfer and compatibility without compiling Linux code."""

import fnmatch
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SPEC = ROOT / "packaging/rpm/agent-sec-core.spec.v2.in"
HOOKS = (
    "agent-sec-cosh-hook",
    "agent-sec-openclaw-hook",
    "agent-sec-hermes-hook",
    "agent-sec-qwen-code-hook",
    "agent-sec-codex-hook",
    "agent-sec-qoder-hook",
)


def sections(text):
    result = {}
    current = None
    for line in text.splitlines():
        match = re.match(
            r"^%(package|description|files|pre|post|posttrans|preun|postun|prep|build|install|changelog)(?:\s+(.*))?$",
            line,
        )
        if match:
            kind, suffix = match.groups()
            name = suffix.removeprefix("-n ") if suffix else "agent-sec-core"
            current = kind, name
            if current in result:
                raise AssertionError(f"duplicate RPM section: {current}")
            result[current] = []
        elif current:
            result[current].append(line)
    return {key: "\n".join(value) for key, value in result.items()}


def patterns(text):
    result = []
    for line in text.splitlines():
        if not line.strip() or line.startswith(("%defattr", "%dir", "%license", "#")):
            continue
        line = re.sub(r"%(?:attr\([^)]*\)|config\([^)]*\))\s*", "", line)
        for macro, path in (
            ("_unitdir", "/usr/lib/systemd/system"),
            ("_sysconfdir", "/etc"),
            ("_datadir", "/usr/share"),
        ):
            line = line.replace("%{" + macro + "}", path)
        result.append(line.strip())
    return result


class RpmSplitTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("rpmspec"), "rpmspec is required")
    def test_rpm_build_modes_select_matching_sources_and_commands(self):
        with tempfile.TemporaryDirectory(prefix="sec-rpm-spec-") as temporary:
            spec = Path(temporary) / "agent-sec-core.spec"
            spec.write_text(SPEC.read_text().replace("@VERSION@", "1.1.2"))
            command = [
                "rpmspec", "-P",
                "--define", "systemd_post /bin/true",
                "--define", "systemd_preun /bin/true",
                "--define", "systemd_postun_with_restart /bin/true",
            ]
            offline = subprocess.check_output(command + [str(spec)], text=True)
            online = subprocess.check_output(
                command + ["--define", "_with_nightly_online 1", str(spec)], text=True
            )
        self.assertRegex(offline, r"(?m)^Source1:\s+agent-sec-core-1\.1\.2-vendor\.tar\.gz$")
        self.assertIn("make build-all-v2-offline", offline)
        self.assertNotIn("make build-all-v2 NPM_INSTALL=", offline)
        self.assertNotRegex(online, r"(?m)^Source1:")
        self.assertIn("make build-all-v2 NPM_INSTALL='npm ci ", online)
        self.assertNotIn("make build-all-v2-offline", online)

    def test_exact_package_set_and_compatibility_requirements(self):
        text = SPEC.read_text()
        parsed = sections(text)
        real = {"agent-sec-core", "agent-sec-plugins", "agent-sec-skills"}
        self.assertEqual({name for kind, name in parsed if kind == "files"}, real)
        self.assertEqual(
            {name for kind, name in parsed if kind == "package"}, real - {"agent-sec-core"}
        )
        header = text.split("%description", 1)[0]
        self.assertNotIn("Requires:       agent-sec-plugins", header)
        self.assertNotIn("Requires:       agent-sec-skills", header)
        self.assertIn("Requires:       bubblewrap", header)
        self.assertIn("%define anolis_release 1", header)
        self.assertIn("Provides:       agent-sec-cli = %{version}-%{release}", header)
        self.assertIn("Obsoletes:      agent-sec-cli < 1.1.1-2", header)
        self.assertNotIn("agent-sec-cosh-hook", header)
        for package in ("agent-sec-plugins", "agent-sec-skills"):
            self.assertIn(
                "Requires:       %{name} = %{version}-%{release}", parsed["package", package]
            )
        for previous_owner in HOOKS:
            self.assertIn(
                f"Provides:       {previous_owner} = %{{version}}-%{{release}}",
                parsed["package", "agent-sec-plugins"],
            )
            self.assertIn(
                f"Obsoletes:      {previous_owner} < 1.1.1-2",
                parsed["package", "agent-sec-plugins"],
            )
        self.assertNotIn("components/sec-core", parsed["files", "agent-sec-core"])

    def test_each_runtime_family_has_exactly_one_owner(self):
        parsed = sections(SPEC.read_text())
        inventory = {
            "/usr/bin/agent-sec-cli": "agent-sec-core",
            "/usr/bin/agent-sec-daemon": "agent-sec-core",
            "/usr/bin/agent-sec-daemon-health": "agent-sec-core",
            "/usr/bin/linux-sandbox": "agent-sec-core",
            "/etc/agent-sec/skillsec.json": "agent-sec-core",
            "/usr/lib/systemd/system/agent-sec-core.service": "agent-sec-core",
            "/usr/share/anolisa/extensions/agent-sec-core/hooks/code_scanner_hook.py": "agent-sec-plugins",
            "/opt/agent-sec/openclaw-plugin/dist/index.js": "agent-sec-plugins",
            "/opt/agent-sec/hermes-plugin/src/plugin.yaml": "agent-sec-plugins",
            "/opt/agent-sec/qwen-code-extension/hooks/observability_hook.py": "agent-sec-plugins",
            "/opt/agent-sec/codex-plugin/install.sh": "agent-sec-plugins",
            "/opt/agent-sec/qoder-plugin/install.sh": "agent-sec-plugins",
            "/usr/share/anolisa/skills/code-scanner/SKILL.md": "agent-sec-skills",
        }
        for path, expected in inventory.items():
            with self.subTest(path=path):
                owners = {
                    name
                    for (kind, name), body in parsed.items()
                    if kind == "files"
                    and any(
                        fnmatch.fnmatchcase(path, pattern)
                        or path.startswith(pattern.rstrip("/") + "/")
                        for pattern in patterns(body)
                    )
                }
                self.assertEqual(owners, {expected})

    def test_activation_and_final_erase_migration_belong_to_core(self):
        parsed = sections(SPEC.read_text())
        self.assertIn("%%{NAME}", parsed["pre", "agent-sec-core"])
        self.assertIn("grep -Fxq agent-sec-cli", parsed["pre", "agent-sec-core"])
        self.assertIn("is-enabled", parsed["pre", "agent-sec-core"])
        self.assertIn("is-active", parsed["pre", "agent-sec-core"])
        post = parsed["post", "agent-sec-core"]
        self.assertIn("[ ! -f /run/agent-sec-core-rpm/state ]", post)
        self.assertIn('[ "$1" -eq 1 ]', post)
        self.assertIn("systemctl enable --now", post)
        self.assertIn("cleanup_migration", parsed["posttrans", "agent-sec-core"])
        self.assertIn("legacy-user", parsed["posttrans", "agent-sec-core"])
        self.assertIn("%systemd_preun", parsed["preun", "agent-sec-core"])
        self.assertIn("%systemd_postun_with_restart", parsed["postun", "agent-sec-core"])
        self.assertIn("make build-all-v2-offline", parsed["build", "agent-sec-core"])
        self.assertIn("make install-all-for-rpmbuild-v2", parsed["install", "agent-sec-core"])

    def test_adapter_managed_packages_cover_materialized_rpm_resources(self):
        contract = tomllib.loads((ROOT / "packaging/anolisa/component.toml").read_text())
        parsed = sections(SPEC.read_text())
        file_patterns = {
            package: patterns(body) for (kind, package), body in parsed.items() if kind == "files"
        }

        def installed_owners(path):
            return {
                package
                for package, owned in file_patterns.items()
                if any(
                    fnmatch.fnmatchcase(path, pattern) or path.startswith(pattern.rstrip("/") + "/")
                    for pattern in owned
                )
            }

        def resolve(path):
            return path.replace("{datadir}", "/usr/share/anolisa")

        with tempfile.TemporaryDirectory(prefix="sec-core-rpm-adapters-") as temporary:
            source = Path(temporary) / "source"
            source.mkdir()
            shutil.copyfile(ROOT / "Makefile", source / "Makefile")
            shutil.copytree(ROOT / "packaging", source / "packaging")
            build = source / "target"
            artifacts = (
                "v2/bin/agent-sec-cli",
                "v2/bin/agent-sec-daemon",
                "v2/bin/agent-sec-daemon-health",
                "linux-sandbox",
                "cosh-extension/cosh-extension.json",
                "cosh-extension/hooks/fixture.py",
                "codex-plugin/hooks-plugin/.codex-plugin/plugin.json",
                "codex-plugin/install.sh",
                "qoder-plugin/.qoder-plugin/plugin.json",
                "qoder-plugin/hooks/fixture.py",
                "qoder-plugin/install.sh",
                "openclaw-plugin/openclaw.plugin.json",
                "openclaw-plugin/package.json",
                "openclaw-plugin/dist/index.js",
                "openclaw-plugin/scripts/deploy.sh",
                "hermes-plugin/src/plugin.yaml",
                "hermes-plugin/scripts/deploy.sh",
                "qwen-code-extension/qwen-extension.json",
                "qwen-code-extension/scripts/deploy.sh",
                "qwen-code-extension/hooks/observability_hook.py",
                "qwen-code-extension/hooks/skill_ledger_hook.py",
                "qwen-code-extension/hooks/pii_checker_hook.py",
            )
            for name in artifacts:
                path = build / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture\n")
                path.chmod(0o755)
            for adapter in contract["adapters"]:
                for skill in adapter.get(adapter["framework"], {}).get("skills", []):
                    skill_name = Path(skill["source"]).name
                    path = build / "skills" / skill_name / "SKILL.md"
                    path.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ROOT / "skills" / skill_name / "SKILL.md", path)
            destination = Path(temporary) / "installed"
            environment = os.environ.copy()
            for name in (
                "MAKEFLAGS",
                "MFLAGS",
                "MAKEOVERRIDES",
                "INSTALL_PROFILE",
                "PREFIX",
                "BINDIR",
                "LIBDIR",
                "LIBEXECDIR",
                "ANOLISA_DATADIR",
                "SKILLDIR",
                "EXTENSIONDIR",
                "COMPONENT_DIR",
                "BUILD_DIR",
                "V2_BIN_BUILD_DIR",
                "SYSTEMD_SYSTEM_UNIT_DIR",
                "SYSTEMD_SERVICE_BINDIR",
                "OPENCLAW_PLUGIN_DIR",
                "HERMES_PLUGIN_DIR",
                "CODEX_PLUGIN_DIR",
                "QODER_PLUGIN_DIR",
                "QWEN_CODE_EXTENSION_DIR",
            ):
                environment.pop(name, None)
            subprocess.run(
                [
                    "make",
                    "--no-print-directory",
                    "install-all-for-rpmbuild-v2",
                    f"DESTDIR={destination}",
                    f"BUILD_DIR={build}",
                    "INSTALL_PROFILE=system",
                ],
                cwd=source,
                env=environment,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertFalse((destination / "usr/share/anolisa/components/sec-core").exists())
            sandbox = destination / "usr/bin/linux-sandbox"
            self.assertTrue(sandbox.is_file())
            self.assertTrue(sandbox.stat().st_mode & 0o111)
            self.assertEqual(installed_owners("/usr/bin/linux-sandbox"), {"agent-sec-core"})
            self.assertFalse((destination / "usr/local/bin/linux-sandbox").exists())
            legacy_destination = Path(temporary) / "legacy-installed"
            subprocess.run(
                [
                    "make",
                    "--no-print-directory",
                    "install-cosh-hook",
                    f"DESTDIR={legacy_destination}",
                    f"BUILD_DIR={build}",
                    "INSTALL_PROFILE=system",
                ],
                cwd=source,
                env=environment,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertTrue((legacy_destination / "usr/local/bin/linux-sandbox").is_file())
            self.assertFalse((legacy_destination / "usr/bin/linux-sandbox").exists())
            for adapter in contract["adapters"]:
                with self.subTest(framework=adapter["framework"]):
                    rpm = adapter["backends"]["rpm"]
                    resource_root = destination / resolve(rpm["resource_root"]).lstrip("/")
                    self.assertTrue((resource_root / adapter["bundle"]["entry"]).is_file())
                    files = {path for path in resource_root.rglob("*") if path.is_file()}
                    for skill in adapter.get(adapter["framework"], {}).get("skills", []):
                        skill_root = destination / resolve(skill["source"]).lstrip("/")
                        self.assertTrue((skill_root / "SKILL.md").is_file())
                        files.update(path for path in skill_root.rglob("*") if path.is_file())
                    owners = set()
                    for path in files:
                        installed_path = "/" + str(path.relative_to(destination))
                        actual = installed_owners(installed_path)
                        self.assertEqual(len(actual), 1, installed_path)
                        self.assertTrue(actual.issubset(rpm["managed_packages"]), installed_path)
                        owners.update(actual)
                    self.assertEqual(owners, set(rpm["managed_packages"]))

    @unittest.skipUnless(shutil.which("rpmspec"), "rpmspec is unavailable")
    def test_rpmspec_emits_only_the_three_payload_packages(self):
        with tempfile.TemporaryDirectory() as temporary:
            spec = Path(temporary) / "agent-sec-core.spec"
            spec.write_text(SPEC.read_text().replace("@VERSION@", "9.8.7"))
            command = ["rpmspec", "-q", "--rpms", "--qf", "%{NAME}\n"]
            for macro in (
                "_unitdir /usr/lib/systemd/system",
                "systemd_post() %{nil}",
                "systemd_preun() %{nil}",
                "systemd_postun_with_restart() %{nil}",
            ):
                command.extend(["--define", macro])
            result = subprocess.run(
                command + [str(spec)], capture_output=True, text=True, check=True
            )
            self.assertEqual(
                set(result.stdout.splitlines()),
                {
                    "agent-sec-core",
                    "agent-sec-plugins",
                    "agent-sec-skills",
                },
            )


if __name__ == "__main__":
    unittest.main()
