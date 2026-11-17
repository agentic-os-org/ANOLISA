#!/usr/bin/env python3
"""Exercise source-export and historical-source compatibility without builds."""

import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ACTION = ROOT / ".github/actions/package-source/action.yaml"


def action_script(name: str) -> str:
    """Read the actual Bash block so fixtures exercise the publishing action."""
    lines = ACTION.read_text().splitlines()
    start = lines.index(f"    - name: {name}")
    run = next(i for i in range(start, len(lines)) if lines[i] == "      run: |")
    body = []
    for line in lines[run + 1 :]:
        if line and not line.startswith("        "):
            break
        body.append(line)
    return textwrap.dedent("\n".join(body))


class ContractSourcePublishing(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="anolisa-contract-sources-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.archives = self.work / "archives"
        self.archives.mkdir()
        self.environment = os.environ.copy()
        self.environment.update(ARCHIVE_NAME="tokenless-fixture", ARCHIVE_FILE="source.tar.gz")

    def run_script(self, script: str) -> subprocess.CompletedProcess:
        script = script.replace("/tmp/archives", str(self.archives))
        script = script.replace("/tmp/build", str(self.work / "build"))
        return subprocess.run(
            ["bash", "-c", script],
            env=self.environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def write_source_archive(self, files: dict[str, str]) -> None:
        with tarfile.open(self.archives / "source.tar.gz", "w:gz") as archive:
            for name, content in files.items():
                data = content.encode()
                info = tarfile.TarInfo(f"tokenless-fixture/{name}")
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))

    def test_archive_gate_accepts_current_templates_and_historical_contracts(self) -> None:
        script = action_script("Verify tokenless component contract in archive")
        for contract, version in (
            ("packaging/anolisa/component.toml.in", "@VERSION@"),
            (".anolisa/component.toml", "0.9.0-rc.1"),
        ):
            with self.subTest(contract=contract):
                self.write_source_archive(
                    {
                        "Cargo.toml": '[workspace.package]\nversion = "0.9.0-rc.1"\n',
                        contract: f'[component]\nname = "tokenless"\nversion = "{version}"\n',
                    }
                )
                result = self.run_script(script)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Archive contract version verified: 0.9.0-rc.1", result.stdout)

    def test_archive_gate_handles_large_source_inventories(self) -> None:
        files = {
            "Cargo.toml": '[workspace.package]\nversion = "0.9.0"\n',
            "packaging/anolisa/component.toml.in": '[component]\nversion = "@VERSION@"\n',
        }
        files.update(
            {f"third_party/rtk/vendor/fixture-{index}/source.rs": "" for index in range(3000)}
        )
        self.write_source_archive(files)
        result = self.run_script(action_script("Verify tokenless component contract in archive"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_archive_gate_rejects_missing_or_drifted_metadata(self) -> None:
        script = action_script("Verify tokenless component contract in archive")
        for contract in (None, 'version = "99.0.0"\n'):
            with self.subTest(contract=contract):
                files = {"Cargo.toml": '[workspace.package]\nversion = "0.9.0"\n'}
                if contract is not None:
                    files["packaging/anolisa/component.toml.in"] = contract
                self.write_source_archive(files)
                result = self.run_script(script)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_source_archives_exclude_build_outputs(self) -> None:
        for component in ("tokenless", "agent-memory"):
            with self.subTest(component=component):
                source = self.work / "build" / component
                for name in (
                    "target/packaging/anolisa/component.toml",
                    "target/release/fixture",
                    "packaging/anolisa/component.toml.in",
                    "packaging/anolisa/rpm/component.toml",
                ):
                    path = source / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("fixture")
                self.environment.update(ARCHIVE_NAME=component)
                script = action_script("Create source archive (tar)")
                script = script.replace("${{ inputs.component }}", component)
                result = self.run_script(script)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                with tarfile.open(self.archives / "source.tar.gz") as archive:
                    names = archive.getnames()
                self.assertIn(f"{component}/packaging/anolisa/rpm/component.toml", names)
                self.assertFalse(any(name.startswith(f"{component}/target") for name in names))

    def test_os_skills_source_archive_uses_the_release_catalog(self) -> None:
        source = self.work / "src" / "os-skills"
        packaging = source / "packaging"
        packaging.mkdir(parents=True)
        shutil.copyfile(
            ROOT / "src/os-skills/packaging/release_catalog.py",
            packaging / "release_catalog.py",
        )
        (packaging / "released-skills.txt").write_text("devops/stable\n")
        shutil.copyfile(ROOT / "src/os-skills/Makefile", source / "Makefile")
        (source / "cosh-extension.json").write_text(
            '{"name":"test-skills","skills":["devops/stable"]}'
        )
        (source / "component.toml").write_text(
            '[component]\nname = "os-skills"\nversion = "1.2.3"\n\n'
            '[[adapters]]\nframework = "openclaw"\n\n'
            '[[adapters.openclaw.skills]]\nname = "stable"\n'
            'source = "{datadir}/skills/stable/"\n\n'
            '[[adapters.openclaw.skills]]\nname = "incubating"\n'
            'source = "{datadir}/skills/incubating/"\n'
        )
        for name in ("stable", "incubating"):
            skill = source / "devops" / name
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text(f"---\nname: {name}\n---\n")
            (skill / "example.sh").write_text("#!/bin/sh\nexit 0\n")
        self.environment.update(
            ARCHIVE_NAME="os-skills-1.2.3",
            SRC_DIR=str(source),
            LICENSE_RESOLVED="false",
        )
        staging = self.work / "build" / "os-skills-1.2.3"
        staging.mkdir(parents=True)
        stale = staging / "stale-skill"
        stale.touch()
        script = action_script("Create source archive")
        result = self.run_script(script.replace("${{ inputs.component }}", "os-skills"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("archive staging directory must be empty", result.stderr)
        stale.unlink()
        for action in ("Create source archive", "Create source archive (tar)"):
            script = action_script(action).replace("${{ inputs.component }}", "os-skills")
            result = self.run_script(script)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        with tarfile.open(self.archives / "source.tar.gz") as archive:
            root = "os-skills-1.2.3"
            self.assertIn(f"{root}/skills/stable/example.sh", archive.getnames())
            self.assertFalse(any("/incubating" in name for name in archive.getnames()))
            contract = archive.extractfile(f"{root}/component.toml")
            self.assertIsNotNone(contract)
            data = tomllib.loads(contract.read().decode())
            self.assertEqual(
                [skill["name"] for skill in data["adapters"][0]["openclaw"]["skills"]],
                ["stable"],
            )
            projected = archive.extractfile(f"{root}/packaging/released-skills.txt")
            self.assertEqual(projected.read().decode(), "skills/stable\n")
            cosh = archive.extractfile(f"{root}/cosh-extension.json")
            self.assertEqual(json.load(cosh)["skills"], ["skills/stable"])
            extracted = self.work / "extracted"
            archive.extractall(extracted, filter="data")
        installed = self.work / "installed"
        result = subprocess.run(
            [
                "make",
                "-C",
                str(extracted / root),
                "install",
                "INSTALL_PROFILE=system",
                f"DESTDIR={installed}",
            ],
            capture_output=True,
            check=False,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        skills = installed / "usr/local/share/anolisa/skills"
        self.assertEqual({path.name for path in skills.iterdir()}, {"stable"})

    def test_memory_source_export_builds_plugin_without_component_owned_metadata(self) -> None:
        source = self.work / "build" / "agent-memory"
        (source / "packaging/anolisa/rpm").mkdir(parents=True)
        executable = self.work / "bin"
        executable.mkdir()
        (executable / "jq").write_text("#!/bin/sh\nexit 0\n")
        (executable / "make").write_text(
            '#!/bin/sh\n[ "$#" -eq 3 ] && [ "$3" = build-openclaw-plugin ] || exit 91\n'
            'mkdir -p "$2/adapters/agent-memory/openclaw/dist"\n'
            "printf '// prebuilt plugin\\n' > \"$2/adapters/agent-memory/openclaw/dist/index.js\"\n"
        )
        for command in executable.iterdir():
            command.chmod(0o755)
        self.environment.update(
            PATH=str(executable) + os.pathsep + self.environment["PATH"],
            ARCHIVE_NAME="agent-memory",
        )
        result = self.run_script(action_script("Build OpenClaw plugin (agent-memory)"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((source / "adapters/agent-memory/openclaw/dist/index.js").is_file())
        self.assertFalse((source / "packaging/anolisa/rpm/component.toml").exists())

    def test_v1_sec_core_archive_keeps_sandbox_path_dependencies(self) -> None:
        source = self.work / "src/agent-sec-core"
        original = ROOT / "src/agent-sec-core"
        for name in (
            "skills/fixture",
            "cosh-extension/fixture",
            "tools/sign-skill.sh",
            "openclaw-plugin/fixture",
            "hermes-plugin/src/fixture",
            "hermes-plugin/scripts/fixture",
            "qwen-code-extension/fixture",
            "codex-plugin/hooks-plugin/fixture",
            "codex-plugin/install.sh",
            "codex-plugin/.agents/fixture",
            "qoder-plugin/fixture",
            "scripts/agent-sec-cli-wrapper.sh",
            "scripts/agent-sec-daemon-wrapper.sh",
            "agent-sec-cli/fixture",
            "v2/apps/fixture/main.rs",
        ):
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture")
        for name in (
            "Makefile",
            "agent-sec-core.spec.in",
            "linux-sandbox/Cargo.toml",
            "v2/Cargo.toml",
        ):
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(original / name, path)
        for name in (
            "packaging",
            "v2/crates/asc-sandbox-types",
            "v2/crates/asc-os-permission-types",
            "v2/crates/asc-foundation-types",
        ):
            shutil.copytree(
                original / name,
                source / name,
                ignore=shutil.ignore_patterns("target", "__pycache__"),
            )
        generated = source / "v2/crates/asc-sandbox-types/target/generated"
        generated.parent.mkdir()
        generated.write_text("must not ship")
        script = self.work / "scripts/rpm-build.sh"
        script.parent.mkdir()
        shutil.copyfile(ROOT / "scripts/rpm-build.sh", script)
        rpmbuild = self.work / "rpmbuild"
        rpmbuild.write_text("#!/bin/sh\nexit 0\n")
        rpmbuild.chmod(0o755)
        self.environment.update(VERSION="9.9.9", RPMBUILD=str(rpmbuild))
        result = subprocess.run(
            ["bash", str(script), "agent-sec-core-v1"],
            env=self.environment,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        tarball = self.work / "scripts/rpmbuild/SOURCES/agent-sec-core-9.9.9.tar.gz"
        with tarfile.open(tarball) as archive:
            prefix = "agent-sec-core-9.9.9/"
            names = set(archive.getnames())

            def manifest(name: str) -> dict:
                return tomllib.loads(archive.extractfile(prefix + name).read().decode())

            sandbox = manifest("linux-sandbox/Cargo.toml")
            deps = sandbox["target"]['cfg(target_os = "linux")']["dependencies"]
            self.assertNotIn("asc-policy-types", deps)
            types = os.path.normpath("linux-sandbox/" + deps["asc-sandbox-types"]["path"])
            workspace = manifest("v2/Cargo.toml")["workspace"]
            crates = [types]
            for dependency in ("asc-os-permission-types", "asc-foundation-types"):
                parent = manifest(crates[-1] + "/Cargo.toml")
                self.assertTrue(parent["dependencies"][dependency]["workspace"])
                crates.append("v2/" + workspace["dependencies"][dependency]["path"])
            for crate in crates:
                self.assertIn(prefix + crate + "/src/lib.rs", names)
                self.assertTrue(manifest(crate + "/Cargo.toml")["package"]["version"]["workspace"])
            self.assertIn("version", workspace["package"])
            self.assertFalse(any("/target/" in name or "/v2/apps/" in name for name in names))
            archive.extractall(self.work / "unpacked", filter="data")
        dry_run = subprocess.run(
            ["make", "-n", "build-all-v1"],
            cwd=self.work / "unpacked/agent-sec-core-9.9.9",
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(dry_run.returncode, 0, dry_run.stdout + dry_run.stderr)
        self.assertIn("maturin build", dry_run.stdout)
        self.assertNotIn("cd v2 &&", dry_run.stdout)

    def test_prebuilt_actions_select_contracts_only_in_the_selected_source(self) -> None:
        for component in ("cosh-ng", "agentsight", "skillfs", "ws-ckpt", "agent-sec-core"):
            action = "sec-core" if component == "agent-sec-core" else component
            text = (ROOT / f".github/actions/build-{action}-prebuilt/build.sh").read_text()
            start = text.index('CONTRACT_DIR="$COMPONENT_ROOT/')
            end = (
                text.index("\nCOMPONENT_VERSION=", start)
                if component in ("skillfs", "ws-ckpt", "agent-sec-core")
                else text.index("\nLINUX_VERSION=", start)
            )
            script = text[start:end] + '\nprintf "%s\\n" "$CONTRACT_DIR"\n'
            source = self.work / component
            legacy = source / ".anolisa"
            legacy.mkdir(parents=True)
            modern = source / "packaging/anolisa"
            if component != "agent-sec-core":
                modern /= "raw"
            for use_modern in (False, True):
                with self.subTest(component=component, modern=use_modern):
                    if use_modern:
                        modern.mkdir(parents=True)
                    self.environment["COMPONENT_ROOT"] = str(source)
                    result = self.run_script(script)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(result.stdout.strip(), str(modern if use_modern else legacy))


if __name__ == "__main__":
    unittest.main()
