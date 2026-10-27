#!/usr/bin/env python3
"""Exercise source-export and historical-source compatibility without builds."""

import io
import os
import subprocess
import tarfile
import tempfile
import textwrap
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

    def test_memory_source_export_requests_and_exports_rpm_metadata(self) -> None:
        source = self.work / "build" / "agent-memory"
        (source / "packaging/anolisa/rpm").mkdir(parents=True)
        executable = self.work / "bin"
        executable.mkdir()
        (executable / "jq").write_text("#!/bin/sh\nexit 0\n")
        (executable / "make").write_text(
            '#!/bin/sh\n[ "$4" = generate-component-contract ] || exit 91\n'
            'mkdir -p "$2/target/packaging/anolisa/rpm"\n'
            'printf \'version = "0.3.0"\\n\' > "$2/target/packaging/anolisa/rpm/component.toml"\n'
        )
        for command in executable.iterdir():
            command.chmod(0o755)
        self.environment.update(
            PATH=str(executable) + os.pathsep + self.environment["PATH"],
            ARCHIVE_NAME="agent-memory",
        )
        result = self.run_script(action_script("Build OpenClaw plugin (agent-memory)"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (source / "packaging/anolisa/rpm/component.toml").read_text(),
            'version = "0.3.0"\n',
        )

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
