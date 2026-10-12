"""Exercise ws-ckpt Raw payload discovery and safe uninstall behavior."""

from __future__ import annotations

import hashlib
import os
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[1]
CONTRACT = COMPONENT / ".anolisa/component.toml"
PACKER = COMPONENT / "packaging/raw/package.sh"


class WsCkptPackagingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="ws-ckpt-packaging-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.contract = CONTRACT
        self.manifest = tomllib.loads(CONTRACT.read_text())
        self.version = self.manifest["component"]["version"]
        for relative in ("scripts", "src/skills", "src/systemd", "src/plugins/hermes"):
            shutil.copytree(COMPONENT / relative, self.source / relative)
        binary = self.source / "src/target/release/ws-ckpt"
        binary.parent.mkdir(parents=True)
        blob = bytearray(64)
        blob[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", blob, 18, 62)
        binary.write_bytes(blob)
        binary.chmod(0o755)
        plugin = self.source / "src/plugins/openclaw"
        (plugin / "dist/src").mkdir(parents=True)
        for name in ("package.json", "package-lock.json", "openclaw.plugin.json"):
            shutil.copyfile(COMPONENT / "src/plugins/openclaw" / name, plugin / name)
        (plugin / "dist/src/index.js").write_text("export default {};\n")
        shutil.copyfile(
            COMPONENT / "src/config.toml.sample", self.source / "src/config.toml.sample"
        )
        cache = self.source / "src/plugins/hermes/__pycache__"
        cache.mkdir(exist_ok=True)
        (cache / "fixture.pyc").write_bytes(b"cache")
        shutil.copyfile(COMPONENT / "src/Cargo.toml", self.source / "src/Cargo.toml")
        shutil.copyfile(COMPONENT / "component.toml", self.source / "component.toml")
        shutil.copyfile(COMPONENT / "LICENSE", self.source / "LICENSE")
        shutil.copyfile(COMPONENT / "README.md", self.source / "README.md")
        self.metadata = binary.parent / "ws-ckpt-build.toml"
        self.metadata.write_text(
            f'version = "{self.version}"\ntarget_os = "linux"\ntarget_arch = "x86_64"\n'
            f'[binaries]\nws-ckpt = "{hashlib.sha256(binary.read_bytes()).hexdigest()}"\n'
        )
        self.arch = "x86_64"

    def pack(
        self, registry: str = "registry", check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(PACKER), "package"],
            env={
                **os.environ,
                "WS_CKPT_SOURCE_DIR": str(self.source),
                "RAW_CONTRACT": str(self.contract),
                "TARGET_OS": "linux",
                "TARGET_ARCH": self.arch,
                "OUTPUT_DIR": str(self.root / registry),
                "SOURCE_DATE_EPOCH": "1700000000",
                "WS_CKPT_BUILD_METADATA": str(self.metadata),
            },
            capture_output=True,
            text=True,
            check=check,
        )

    def artifact(self, registry: str = "registry") -> Path:
        return self.root / registry / f"ws-ckpt-{self.version}-linux-{self.arch}.tar.gz"

    def test_payload_satisfies_contract_and_runtime_discovery(self) -> None:
        self.pack()
        payload = self.root / "payload"
        with tarfile.open(self.artifact(), "r:gz") as archive:
            archive.extractall(payload, filter="data")
        self.assertEqual((payload / ".anolisa/component.toml").read_bytes(), CONTRACT.read_bytes())
        distribution_contract = (
            COMPONENT.parents[1]
            / "distribution/anolisa/manifests/components/ws-ckpt/component.toml"
        )
        self.assertEqual(distribution_contract.read_bytes(), CONTRACT.read_bytes())
        for entry in self.manifest["component"]["layout"]["files"]:
            self.assertTrue((payload / entry["source"]).exists(), entry["source"])
        for adapter in self.manifest["adapters"]:
            self.assertTrue((payload / adapter["source"] / adapter["bundle"]["entry"]).is_file())
        for framework in ("openclaw", "hermes"):
            helper = payload / f"share/anolisa/adapters/ws-ckpt/{framework}/lib-discover.sh"
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; find_plugin_src "$2"; find_skill_src',
                    "bash",
                    str(helper),
                    framework,
                ],
                env={**os.environ, "ANOLISA_TARGET_DIR": str(payload), "ANOLISA_PROJECT_ROOT": ""},
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(
                result.stdout.splitlines(),
                [
                    str(payload / f"share/anolisa/runtime/ws-ckpt/plugins/{framework}"),
                    str(payload / "share/anolisa/runtime/skills/ws-ckpt"),
                ],
            )
        adapter = payload / "share/anolisa/adapters/ws-ckpt/openclaw"
        subprocess.run(
            ["bash", "-c", 'source "$1/lib-openclaw.sh"', "bash", str(adapter)], check=True
        )
        hermes = payload / "share/anolisa/runtime/ws-ckpt/plugins/hermes"
        self.assertFalse((hermes / "tests").exists())
        self.assertFalse(list(hermes.rglob("*.pyc")))
        self.assertFalse(list(hermes.rglob("__pycache__")))
        self.assertFalse((payload / "scripts").exists())

    def test_archive_is_reproducible_and_metadata_matches(self) -> None:
        self.pack()
        self.pack("second-registry")
        artifact = self.artifact()
        self.assertEqual(artifact.read_bytes(), self.artifact("second-registry").read_bytes())
        with tarfile.open(artifact, "r:gz") as archive:
            self.assertEqual(
                archive.extractfile("./.anolisa/component.toml").read(), CONTRACT.read_bytes()
            )

    def test_missing_openclaw_helper_fails_packaging(self) -> None:
        (self.source / "scripts/openclaw/lib-openclaw.sh").unlink()
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("lib-openclaw.sh", result.stderr)
        self.assertFalse(self.artifact().exists())

    def test_arm64_package_does_not_execute_target_binary(self) -> None:
        self.arch = "aarch64"
        binary = self.source / "src/target/release/ws-ckpt"
        blob = bytearray(binary.read_bytes())
        struct.pack_into("<H", blob, 18, 183)
        binary.write_bytes(blob)
        self.metadata.write_text(
            f'version = "{self.version}"\ntarget_os = "linux"\ntarget_arch = "aarch64"\n'
            f'[binaries]\nws-ckpt = "{hashlib.sha256(blob).hexdigest()}"\n'
        )
        self.pack()
        self.assertTrue(self.artifact().is_file())

    def test_wrong_elf_architecture_fails(self) -> None:
        self.arch = "aarch64"
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ELF architecture does not match", result.stderr)

    def test_build_hash_mismatch_fails(self) -> None:
        binary = self.source / "src/target/release/ws-ckpt"
        binary.write_bytes(binary.read_bytes() + b"changed")
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA-256 does not match", result.stderr)

    def test_build_version_mismatch_fails(self) -> None:
        self.metadata.write_text(self.metadata.read_text().replace(self.version, "99.0.0"))
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("metadata version does not match", result.stderr)

    def test_missing_plugin_entry_fails(self) -> None:
        (self.source / "src/plugins/openclaw/dist/src/index.js").unlink()
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("OpenClaw entry", result.stderr)

    def test_missing_cross_build_metadata_fails(self) -> None:
        self.arch = "aarch64"
        binary = self.source / "src/target/release/ws-ckpt"
        blob = bytearray(binary.read_bytes())
        struct.pack_into("<H", blob, 18, 183)
        binary.write_bytes(blob)
        self.metadata.unlink()
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ws-ckpt-build.toml", result.stderr)

    def test_contract_version_mismatch_fails(self) -> None:
        contract = self.root / "wrong-contract.toml"
        contract.write_text(CONTRACT.read_text().replace(self.version, "99.0.0"))
        self.contract = contract
        result = self.pack(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("contract identity", result.stderr)

    def test_uninstall_preserves_data_when_recovery_fails(self) -> None:
        self.run_uninstall(recover_status=1)

    def test_uninstall_preserves_data_after_successful_recovery(self) -> None:
        self.run_uninstall(recover_status=0)

    def test_uninstall_preserves_data_without_recovery_binary(self) -> None:
        self.run_uninstall(recover_status=None)

    def test_uninstall_preserves_mounts_and_data_when_stop_fails(self) -> None:
        self.run_uninstall(recover_status=0, stop_status=1)

    def test_uninstall_preserves_mounts_and_data_when_service_remains_active(self) -> None:
        self.run_uninstall(recover_status=0, active_state="active")

    def test_uninstall_preserves_mounts_and_data_when_service_state_is_unknown(self) -> None:
        self.run_uninstall(recover_status=0, active_state="")

    def test_uninstall_preserves_workspace_created_between_recovery_and_stop(self) -> None:
        self.run_uninstall(recover_status=0, late_request=True)

    def run_uninstall(
        self,
        recover_status: int | None,
        stop_status: int = 0,
        active_state: str = "inactive",
        late_request: bool = False,
    ) -> None:
        self.pack()
        sandbox = self.root / "uninstall"
        sandbox.mkdir()
        with tarfile.open(self.artifact(), "r:gz") as archive:
            body = (
                archive.extractfile("./share/anolisa/hooks/ws-ckpt/pre-uninstall.sh")
                .read()
                .decode()
            )
        # Redirect every absolute runtime path before executing the hook.
        for path in (
            "/var/lib/ws-ckpt",
            "/data/ws-ckpt",
            "/mnt/btrfs-workspace",
            "/usr/local/bin/ws-ckpt",
            "/usr/bin/ws-ckpt",
        ):
            body = body.replace(path, str(sandbox) + path)
        state = sandbox / "var/lib/ws-ckpt"
        data = sandbox / "data/ws-ckpt"
        state.mkdir(parents=True)
        data.mkdir(parents=True)
        runtime = [
            state / "btrfs-data.img",
            state / "state.json",
            state / "daemon.lock",
            data / "btrfs-data.img",
        ]
        unrelated = state / "manual-backup.img"
        for path in [*runtime, unrelated]:
            path.write_text("preserve me")
        commands = sandbox / "bin"
        commands.mkdir()
        calls = sandbox / "calls"
        late_workspace = state / "late-workspace"
        for command in ("systemctl", "findmnt", "losetup", "mountpoint", "umount"):
            executable = commands / command
            command_body = f'#!/bin/sh\nprintf "%s\\n" "{command} $*" >> "$TEST_CALLS"\n'
            if command == "systemctl":
                command_body += (
                    f'if [ "$1" = stop ]; then\n'
                    + (f'echo "new workspace" > "{late_workspace}"\n' if late_request else "")
                    + f"exit {stop_status}\nfi\n"
                    + f'if [ "$1" = show ]; then echo "{active_state}"; fi\n'
                )
            elif command == "findmnt":
                command_body += f'echo "{sandbox}/mnt/btrfs-workspace"\n'
            elif command == "losetup":
                command_body += f'if [ "$1" = -a ]; then echo "/dev/loop99: ({runtime[0]})"; fi\n'
            executable.write_text(command_body + "exit 0\n")
            executable.chmod(0o755)
        if recover_status is not None:
            executable = commands / "ws-ckpt"
            executable.write_text(
                '#!/bin/sh\nprintf "%s\\n" "ws-ckpt $*" >> "$TEST_CALLS"\n'
                f"exit {recover_status}\n"
            )
            executable.chmod(0o755)
        hook = sandbox / "hook.sh"
        hook.write_text(body)
        result = subprocess.run(
            ["bash", str(hook)],
            env={**os.environ, "PATH": f"{commands}:/usr/bin:/bin", "TEST_CALLS": str(calls)},
            capture_output=True,
            text=True,
            check=True,
        )
        for path in runtime:
            self.assertTrue(path.exists(), str(path))
        self.assertTrue(unrelated.exists())
        teardown_allowed = recover_status == 0 and stop_status == 0 and active_state == "inactive"
        commands_run = calls.read_text().splitlines()
        self.assertEqual("findmnt -rn -t btrfs -o TARGET" in commands_run, teardown_allowed)
        self.assertEqual("losetup -d /dev/loop99" in commands_run, teardown_allowed)
        self.assertEqual(f"umount {sandbox}/mnt/btrfs-workspace" in commands_run, teardown_allowed)
        if recover_status is not None:
            self.assertEqual(
                commands_run[:2],
                ["ws-ckpt recover --all --force", "systemctl stop ws-ckpt.service"],
            )
        if late_request:
            self.assertEqual(late_workspace.read_text(), "new workspace\n")
        if recover_status != 0:
            self.assertIn("preserving ws-ckpt data", result.stderr)


if __name__ == "__main__":
    unittest.main()
