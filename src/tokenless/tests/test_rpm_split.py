#!/usr/bin/env python3
"""Exercise RPM subpackage ownership using fake native binaries and plugin builds."""

from __future__ import annotations

import fnmatch
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "packaging/rpm/tokenless.spec.in"
SECTION = re.compile(
    r"^%(?:package|description|prep|build|install|files|preun|post|postun|posttrans|changelog)(?:\s|$)",
    re.MULTILINE,
)


def section(source: str, header: str) -> str:
    """Read an exact RPM section, leaving its scriptlet body intact."""
    start = re.search(r"^" + re.escape(header) + r"(?:[ \t]+-f[ \t]+\S+)?\n", source, re.MULTILINE)
    if start is None:
        raise AssertionError("Missing RPM section: " + header)
    body = source[start.end() :]
    match = SECTION.search(body)
    return body[: match.start()] if match else body


def expand(source: str, macros: dict[str, str]) -> str:
    """Expand only the path macros consumed by the fixture install recipe."""
    for _ in range(8):
        updated = re.sub(r"%\{([^}]+)\}", lambda m: macros.get(m[1], m[0]), source)
        if updated == source:
            break
        source = updated
    return source.replace("%%", "%")


def file_entries(
    source: str, header: str, macros: dict[str, str], builddir: Path
) -> list[tuple[str, bool]]:
    """Evaluate conditional role parents before checking file ownership."""
    entries: list[tuple[str, bool]] = []
    active = True
    body = section(source, header)
    manifest = re.search(r"^" + re.escape(header) + r"[ \t]+-f[ \t]+(\S+)\n", source, re.MULTILINE)
    if manifest:
        body += "\n" + (builddir / manifest[1]).read_text()
    for line in body.splitlines():
        line = expand(line.strip(), macros)
        if line.startswith("%if "):
            match = re.fullmatch(r'%if "([^"]+)" == "([^"]+)"', line)
            if not match:
                raise AssertionError("Unexpected RPM file-list condition: " + line)
            active = match[1] == match[2]
        elif line == "%endif":
            active = True
        elif active and line and not line.startswith("#"):
            path = line.rsplit(None, 1)[-1]
            if path.startswith("/"):
                entries.append((path, "%dir" not in line and path.endswith("/")))
    return entries


def install_fixture(
    work: Path, spec: str, *, custom: bool = False
) -> tuple[Path, Path, dict[str, str]]:
    """Run the actual RPM install recipe with prebuilt inputs for ownership checks."""
    source = work / "source"
    source.mkdir()
    stage = work / "buildroot"
    shutil.copytree(
        ROOT / "adapters/tokenless",
        source / "adapters/tokenless",
        symlinks=True,
        ignore=shutil.ignore_patterns("node_modules", "__pycache__", "*.pyc", ".git"),
    )
    for template in (source / "adapters/tokenless").rglob("*.in"):
        template.with_suffix("").write_text(template.read_text().replace("@VERSION@", "9.8.7"))
    for relative in ("openclaw/dist/index.js", "dsh/dist/index.js"):
        entry = source / "adapters/tokenless" / relative
        entry.parent.mkdir(parents=True, exist_ok=True)
        entry.write_text("export default {};\n")
    (source / "adapters/tokenless/openclaw/dist/unlisted.js").write_text("export default {};\n")
    for relative in ("target/release/tokenless", "third_party/rtk/target/release/rtk"):
        binary = source / relative
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o755)
    for relative in ("LICENSE", "docs/response-compression.md"):
        target = source / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, target)
    macros = {
        "name": "tokenless",
        "buildroot": str(stage),
        "_bindir": "/usr/bin",
        "_docdir": "/usr/share/doc",
        "_datadir": "/usr/share",
        "_libexecdir_tokenless": (
            "/opt/tokenless/runtime" if custom else "/usr/libexec/anolisa/tokenless"
        ),
        "_adapterdir_tokenless": (
            "/opt/tokenless/plugins" if custom else "/usr/share/anolisa/adapters/tokenless"
        ),
        "_extensiondir_tokenless": (
            "/opt/tokenless/cosh" if custom else "/usr/share/anolisa/extensions/tokenless"
        ),
    }
    subprocess.run(
        ["bash", "-e", "-c", expand(section(spec, "%install"), macros)],
        cwd=source,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return source, stage, macros


class RpmSplitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SPEC.read_text()

    def test_exact_compatibility_dependencies_and_role_headers(self) -> None:
        meta = section(self.source, "Name:           tokenless")
        for role in ("core", "plugins"):
            self.assertIn(f"Requires:       %{{name}}-{role} = %{{version}}-%{{release}}", meta)
            header = section(self.source, "%package " + role)
            self.assertIn("Conflicts:      %{name} < %{version}", header)
            self.assertNotIn("anolisa-component", header)
        plugins = section(self.source, "%package plugins")
        self.assertIn("Requires:       %{name}-core = %{version}-%{release}", plugins)
        for dependency in ("python3", "jq", "bash"):
            self.assertIn("Requires:       " + dependency, plugins)
        self.assertIn("BuildArch:      noarch", plugins)
        self.assertIn("%defattr(0644,root,root,0755)", section(self.source, "%files plugins"))
        self.assertIn("Provides:       anolisa-component(tokenless)", meta)
        self.assertNotIn("%package skills", self.source)
        self.assertNotRegex(self.source, r"(?m)^%package plugin$")
        self.assertNotIn("Obsoletes:", self.source)
        self.assertRegex(self.source, r"(?m)^%define anolis_release 1$")

    def test_scriptlets_follow_the_runtime_owner_and_skip_upgrade_cleanup(self) -> None:
        core = section(self.source, "%post core")
        plugins = section(self.source, "%post plugins")
        removal = section(self.source, "%preun plugins")
        self.assertIn("$HOME/.local/bin/tokenless", core)
        self.assertNotIn("adapters/tokenless", core)
        self.assertNotIn(".openclaw", core)
        self.assertIn("adapters/tokenless", plugins)
        self.assertNotIn("$HOME/.local/bin/tokenless", plugins)
        self.assertIn("if [ $1 -eq 0 ]; then", removal)
        self.assertNotRegex(self.source, r"(?m)^%(?:post|preun)$")
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            log = work / "unexpected-command"
            for command in ("bash", "rm", "jq", "mv", "openclaw"):
                stub = work / command
                stub.write_text(f"#!/bin/sh\nprintf called >> '{log}'\nexit 99\n")
                stub.chmod(0o755)
            subprocess.run(
                [shutil.which("bash"), "-e", "-c", removal, "rpm-preun", "1"],
                env={**os.environ, "PATH": str(work), "HOME": str(work)},
                check=True,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertFalse(log.exists(), "Upgrade must not deregister the existing host plugins")

    def test_install_recipe_owns_every_file_once(self) -> None:
        for custom in (False, True):
            with self.subTest(custom_roles=custom), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                source, stage, macros = install_fixture(work, self.source, custom=custom)
                ownership = {
                    role: file_entries(
                        self.source, "%files" + (" " + role if role else ""), macros, source
                    )
                    for role in ("", "core", "plugins")
                }
                counts = dict.fromkeys(ownership, 0)
                for path in stage.rglob("*"):
                    if not path.is_symlink() and not path.is_file():
                        continue
                    installed = "/" + str(path.relative_to(stage))
                    owners = [
                        role
                        for role, entries in ownership.items()
                        if any(
                            (
                                installed.startswith(pattern)
                                if recursive
                                else fnmatch.fnmatchcase(installed, pattern)
                            )
                            for pattern, recursive in entries
                        )
                    ]
                    self.assertEqual(len(owners), 1, (installed, owners))
                    counts[owners[0]] += 1
                self.assertEqual(counts[""], 0)
                self.assertFalse((stage / "usr/share/anolisa/components").exists())
                self.assertEqual(counts["core"], 5)
                self.assertGreater(counts["plugins"], 70)
                self.assertEqual(
                    os.readlink(stage / "usr/bin/rtk"), macros["_libexecdir_tokenless"] + "/rtk"
                )
                plugin_root = stage / macros["_adapterdir_tokenless"].lstrip("/")
                manifest = (source / "tokenless-plugins.files").read_text().splitlines()
                for relative in (
                    "common/hooks/hook_utils.py",
                    "qwenwork/hooks/shared/hook_utils.py",
                    "hermes/__init__.py",
                    "qwenpaw/plugin.py",
                ):
                    self.assertIn(
                        "%attr(0755,root,root) " + macros["_adapterdir_tokenless"] + "/" + relative,
                        manifest,
                    )
                self.assertIn(
                    "%attr(0755,root,root) "
                    + macros["_extensiondir_tokenless"]
                    + "/hooks/hook_utils.py",
                    manifest,
                )
                for relative in (
                    "codex/scripts/check-tokenless",
                    "codex/scripts/response-diagnostics",
                    "codex/scripts/rewrite-hook",
                    "codex/scripts/tool-ready",
                    "claude-code/hooks/run-hook.sh",
                    "qwencode/hooks/run-hook.sh",
                    "qwenwork/hooks/shared/rewrite_hook.py",
                ):
                    installed = plugin_root / relative
                    self.assertFalse(installed.is_symlink(), relative)
                    self.assertEqual(installed.stat().st_mode & 0o777, 0o755, relative)
                for relative in (
                    "codex/scripts/_common.sh",
                    "qwenwork/hooks/shared/tool_categories.json",
                ):
                    self.assertEqual(
                        (plugin_root / relative).stat().st_mode & 0o777, 0o644, relative
                    )
                self.assertFalse((plugin_root / "qwenwork/hooks/shared").is_symlink())
                for relative in (
                    "codex/README.md",
                    "codex/.gitignore",
                    "openclaw/index.ts",
                    "openclaw/package-lock.json",
                    "openclaw/dist/unlisted.js",
                    "qwenpaw/requirements.txt.in",
                ):
                    self.assertFalse((plugin_root / relative).exists(), relative)
                for framework in (
                    "openclaw",
                    "dsh",
                    "hermes",
                    "qoder",
                    "qwenwork",
                    "claude-code",
                    "codex",
                    "opencode",
                    "qwencode",
                    "qwenpaw",
                    "common",
                ):
                    self.assertTrue((plugin_root / framework).is_dir(), framework)
                if custom:
                    paths = {path for entries in ownership.values() for path, _ in entries}
                    self.assertNotIn("/usr/share/anolisa/adapters", paths)
                    self.assertNotIn("/usr/share/anolisa/extensions", paths)

    @unittest.skipUnless(
        shutil.which("rpmspec"), "rpmspec is unavailable; fixture inventory is still checked"
    )
    def test_rpmspec_declares_real_binary_packages(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            spec = Path(temporary) / "tokenless.spec"
            spec.write_text(self.source.replace("@VERSION@", "9.8.7"))
            result = subprocess.run(
                ["rpmspec", "-q", "--rpms", "--qf", "%{NAME}\n", str(spec)],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(
                set(result.stdout.splitlines()),
                {"tokenless", "tokenless-core", "tokenless-plugins"},
            )


if __name__ == "__main__":
    unittest.main()
