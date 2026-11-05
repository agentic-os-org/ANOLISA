"""Exercise the raw package boundary without executing fixture ELF files."""

import hashlib
import json
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "packaging/raw/package.py"


class RawPackaging(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.source = self.work / "source"
        self.source.mkdir()
        for name in ("packaging/anolisa", "config", "adapters"):
            shutil.copytree(
                ROOT / name,
                self.source / name,
                ignore=shutil.ignore_patterns("node_modules", "dist", "coverage"),
            )
        shutil.copytree(ROOT / "packaging/systemd", self.source / "packaging/systemd")
        shutil.copy2(ROOT / "Cargo.toml", self.source / "Cargo.toml")
        self.version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        self.bin = self.work / "private-bin"
        self.bin.mkdir()
        self.plugin = self.work / "private-plugin"
        self.plugin.mkdir()
        (self.plugin / "index.js").write_text("export default {};\n")
        (self.plugin / "build-meta.json").write_text("{}")
        self.metadata = self.work / "build.toml"
        self.prepare("x86_64")

    def prepare(self, arch: str) -> None:
        blob = bytearray(64)
        blob[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", blob, 18, {"x86_64": 62, "aarch64": 183}[arch])
        (self.bin / "agent-memory").write_bytes(blob)
        (self.bin / "agent-memory").chmod(0o755)
        plugin_hash = hashlib.sha256((self.plugin / "index.js").read_bytes()).hexdigest()
        self.metadata.write_text(
            f'version = "{self.version}"\ntarget_os = "linux"\ntarget_arch = "{arch}"\n'
            f'[binaries]\nagent-memory = "{hashlib.sha256(blob).hexdigest()}"\n'
            f'[plugin]\nsha256 = "{plugin_hash}"\n'
        )

    def run_package(
        self, arch: str = "x86_64", success: bool = True
    ) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [
                sys.executable,
                str(PACKAGER),
                "--source-dir",
                str(self.source),
                "--bin-dir",
                str(self.bin),
                "--plugin-dir",
                str(self.plugin),
                "--metadata",
                str(self.metadata),
                "--target-arch",
                arch,
                "--output-dir",
                str(self.work / "out"),
                "--epoch",
                "1700000000",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stderr)
        return result

    def test_architectures_layout_and_reproducibility(self) -> None:
        for arch in ("x86_64", "aarch64"):
            self.prepare(arch)
            archive = Path(self.run_package(arch).stdout.strip())
            first = archive.read_bytes()
            self.run_package(arch)
            self.assertEqual(first, archive.read_bytes())
            with tarfile.open(archive) as tar:
                files = {
                    m.name.removeprefix("./"): tar.extractfile(m).read() for m in tar if m.isfile()
                }
                self.assertEqual(len(files), 12)
                self.assertEqual(
                    files["bin/agent-memory"], (self.bin / "agent-memory").read_bytes()
                )
                self.assertEqual(
                    files["adapters/agent-memory/openclaw/dist/index.js"],
                    (self.plugin / "index.js").read_bytes(),
                )
                contract = tomllib.loads(files[".anolisa/component.toml"].decode())
                self.assertEqual(contract["component"]["version"], self.version)
                self.assertIn(arch, contract["component"]["platform"]["arch"])
                self.assertFalse(contract["component"]["services"][0]["start"])
                unit_path = "share/anolisa/agent-memory/anolisa-memory@.service"
                unit = files[unit_path].decode()
                self.assertEqual(
                    unit,
                    (self.source / "packaging/systemd/anolisa-memory@.service.in").read_text(),
                )
                self.assertIn("ExecStart={bindir}/agent-memory serve", unit)
                self.assertNotIn("ExecStart=/usr/bin/agent-memory", unit)
                unit_mapping = next(
                    item
                    for item in contract["component"]["layout"]["files"]
                    if item.get("source") == unit_path
                )
                self.assertEqual(unit_mapping["target"], "{userunitdir}/anolisa-memory@.service")
                self.assertEqual(unit_mapping["render"], "anolisa-paths-v1")
                minimum = contract["component"]["contract"]["min_anolisa_version"]
                self.assertGreaterEqual(tuple(map(int, minimum.split("."))), (0, 3, 20))
                mcp_path = "share/anolisa/mcp-servers/agent-memory.json"
                mcp = json.loads(files[mcp_path])
                self.assertEqual(mcp["command"], "{bindir}/agent-memory")
                self.assertEqual(
                    json.loads((self.source / "config/mcp-server.json").read_text())["command"],
                    "/usr/bin/agent-memory",
                )
                mcp_mapping = next(
                    item for item in contract["component"]["layout"]["files"]
                    if item.get("source") == mcp_path
                )
                self.assertEqual(mcp_mapping["render"], "anolisa-json-command-v1")
                for item in tar.getmembers():
                    self.assertEqual((item.uid, item.gid, item.mtime), (0, 0, 1700000000))
                    executable = item.name.endswith(".sh") or item.name == "./bin/agent-memory"
                    self.assertEqual(item.mode, 0o755 if item.isdir() or executable else 0o644)

    def test_missing_and_stale_metadata(self) -> None:
        self.metadata.unlink()
        self.run_package(success=False)
        self.prepare("x86_64")
        self.metadata.write_text(self.metadata.read_text().replace(self.version, "0.0.0"))
        self.assertIn("version", self.run_package(success=False).stderr)

    def test_tampered_binary_and_plugin(self) -> None:
        with (self.bin / "agent-memory").open("ab") as stream:
            stream.write(b"replacement")
        self.assertIn("hash", self.run_package(success=False).stderr)
        self.prepare("x86_64")
        (self.plugin / "index.js").write_text("replacement")
        self.assertIn("plugin hash", self.run_package(success=False).stderr)

    def test_missing_outputs(self) -> None:
        (self.bin / "agent-memory").unlink()
        self.run_package(success=False)
        self.prepare("x86_64")
        (self.plugin / "index.js").unlink()
        self.run_package(success=False)

    def test_wrong_architecture(self) -> None:
        self.assertIn("architecture", self.run_package("aarch64", success=False).stderr)
        self.run_package("riscv64", success=False)

    def test_version_drift_fails_without_repair(self) -> None:
        path = self.source / "config/mcp-server.json"
        data = json.loads(path.read_text())
        data["version"] = "0.0.0"
        path.write_text(json.dumps(data))
        self.assertIn("version", self.run_package(success=False).stderr)
        self.assertEqual(json.loads(path.read_text())["version"], "0.0.0")

    def test_shared_output_is_not_used(self) -> None:
        shared = self.source / "target/release"
        shared.mkdir(parents=True)
        (shared / "agent-memory").write_text("stale host binary")
        self.run_package()
        (self.bin / "agent-memory").unlink()
        self.run_package(success=False)


if __name__ == "__main__":
    unittest.main()
