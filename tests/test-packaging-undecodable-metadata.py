#!/usr/bin/env python3
"""Every packaging verifier reports an undecodable metadata file as a plain error.

Both `tomllib.load` on a binary stream and `Path.read_text(encoding="utf-8")`
decode before parsing, so bytes that are not UTF-8 raise `UnicodeDecodeError`.
That is a `ValueError`: not an `OSError`, and not the parser's own
`TOMLDecodeError` or `JSONDecodeError`. A guard written as
`except (OSError, tomllib.TOMLDecodeError)` therefore lets the decode failure
escape, and the packaging job dies on a traceback instead of the verifier's own
"cannot read <path>" message.

The prebuilt binary verifier was fixed for this on its own; the sibling
verifiers are copies of the same read and carried the same gap. This drives
each verifier through the function it actually calls, so a new copy that
reintroduces the short guard fails here.

Runs under any `python3` that has `tomllib` (3.11+); the verifiers themselves
need the same floor.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import ModuleType, SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
_SWE_RUNNER_SRC = ROOT / "benchmark/swe-runner/src"

# Each entry is (label, script, callable that reads TOML from a path).
_READERS = (
    (
        "cosh-ng build metadata",
        ROOT / "src/cosh-ng/packaging/raw/verify-binaries.py",
        ("verify_build_metadata", ("1.0.0", "linux", "x86_64", [])),
    ),
    (
        "cosh-ng release",
        ROOT / "src/cosh-ng/packaging/raw/verify-release.py",
        ("read_toml", ()),
    ),
    (
        "agentsight build metadata",
        ROOT / "src/agentsight/packaging/raw/verify-binaries.py",
        ("verify_build_metadata", ("1.0.0", "linux", "x86_64", [])),
    ),
    (
        "agentsight release",
        ROOT / "src/agentsight/packaging/raw/verify-release.py",
        ("read_toml", ()),
    ),
    (
        "prebuilt release",
        ROOT / "distribution/anolisa/packaging/prebuilt/verify-release.py",
        ("read_toml", ()),
    ),
    (
        "prebuilt binary",
        ROOT / "distribution/anolisa/packaging/prebuilt/verify-binary.py",
        ("main", None),
    ),
    (
        "tokenless generated adapter",
        ROOT / "src/tokenless/packaging/raw/verify-release.py",
        ("read_json_version", ()),
    ),
    (
        "tokenless generated manifest",
        ROOT / "src/tokenless/packaging/raw/verify-release.py",
        ("read_manifest_targets", ()),
    ),
    (
        "tokenless hermes manifest",
        ROOT / "src/tokenless/packaging/raw/verify-release.py",
        ("read_hermes_version", ()),
    ),
)


def _load(script: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, script)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TomlUnicodeGuardTest(unittest.TestCase):
    """A non-UTF-8 metadata file must not raise a bare UnicodeDecodeError."""

    def test_every_verifier_reports_undecodable_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "metadata.toml"
            # Lone 0xFF is not valid UTF-8, so `tomllib.load` fails while
            # decoding, before any TOML grammar is considered.
            bad.write_bytes(b'version = "\xff"\n')

            for index, (label, script, (function, extra)) in enumerate(_READERS):
                with self.subTest(label):
                    self.assertTrue(script.is_file(), f"{label}: {script} is missing")
                    module = _load(script, f"packaging_verifier_{index}")
                    if function == "main":
                        argv = [
                            str(script),
                            "--metadata",
                            str(bad),
                            "--version",
                            "1.0.0",
                            "--os",
                            "linux",
                            "--arch",
                            "x86_64",
                            "bin/cosh",
                        ]
                        with mock.patch.object(sys, "argv", argv):
                            with self.assertRaises(SystemExit) as raised:
                                module.main()
                        self.assertIn("cannot read", str(raised.exception))
                        continue
                    target = getattr(module, function)
                    with self.assertRaises((SystemExit, ValueError)) as raised:
                        target(bad, *extra)
                    self.assertIn("cannot read", str(raised.exception))
                    # ValueError also matches a decode error that escaped the
                    # guard, so say which one this has to be.
                    self.assertNotIsInstance(raised.exception, UnicodeDecodeError)


class SilentFallbackUnicodeGuardTest(unittest.TestCase):
    """Reads that fall back quietly must not crash on an undecodable file."""

    def test_benchmark_redaction_falls_back_on_an_undecodable_manifest(self) -> None:
        sys.path.insert(0, str(_SWE_RUNNER_SRC))
        self.addCleanup(sys.path.remove, str(_SWE_RUNNER_SRC))
        module = _load(
            _SWE_RUNNER_SRC / "swe_runner/run/io/manifest_records.py",
            "manifest_records_for_test",
        )
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "input.json"
            bad.write_bytes(b'{"prompt": "\xff"}')
            self.assertIsNone(module.redacted_json_file(bad))


class ArtifactVerifierUnicodeGuardTest(unittest.TestCase):
    """The release-artifact verifier reports an undecodable SBOM, not a traceback."""

    def test_undecodable_sbom_is_reported(self) -> None:
        script = ROOT / ".github/actions/prebuilt-rust-common/verify-artifacts.py"
        # The verifier imports its sibling `targets` module by plain name.
        sys.path.insert(0, str(script.parent))
        self.addCleanup(sys.path.remove, str(script.parent))
        module = _load(script, "verify_artifacts_for_test")
        component, version, os_name, arch = "cosh", "1.0.0", "linux", "x86_64"
        archive_name = f"{component}-{version}-{os_name}-{arch}.tar.gz"
        sbom_name = f"{archive_name}.cdx.json"

        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = directory / archive_name
            # A valid one-member archive, so the checks before the SBOM read
            # pass and the SBOM read is the step that fails.
            with tarfile.open(archive, "w:gz") as package:
                payload = b"binary payload\n"
                info = tarfile.TarInfo("bin/cosh")
                info.size = len(payload)
                package.addfile(info, io.BytesIO(payload))
            _write_sidecar(archive, archive_name)

            sbom = directory / sbom_name
            # The byte 0xFF is not valid UTF-8, so the read fails while decoding.
            sbom.write_bytes(b'{"bomFormat": "\xff"}')
            _write_sidecar(sbom, sbom_name)

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit):
                    module.validate_directory(directory, component, version, os_name, arch)
            self.assertIn("cannot read SBOM", stderr.getvalue())

    def test_undecodable_sidecar_is_reported(self) -> None:
        # The sidecar is read before the SBOM, so a corrupt one used to take
        # the job down before the SBOM guard could say anything.
        script = ROOT / ".github/actions/prebuilt-rust-common/verify-artifacts.py"
        sys.path.insert(0, str(script.parent))
        self.addCleanup(sys.path.remove, str(script.parent))
        module = _load(script, "verify_artifacts_sidecar_for_test")
        component, version, os_name, arch = "cosh", "1.0.0", "linux", "x86_64"
        archive_name = f"{component}-{version}-{os_name}-{arch}.tar.gz"

        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = directory / archive_name
            with tarfile.open(archive, "w:gz") as package:
                payload = b"binary payload\n"
                info = tarfile.TarInfo("bin/cosh")
                info.size = len(payload)
                package.addfile(info, io.BytesIO(payload))
            sbom = directory / f"{archive_name}.cdx.json"
            sbom.write_text('{"bomFormat": "CycloneDX"}', encoding="utf-8")
            _write_sidecar(sbom, sbom.name)
            # Only the archive's sidecar is corrupt; it is read first.
            Path(f"{archive}.sha256").write_bytes(b"\xff not utf-8\n")

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit) as raised:
                    module.validate_directory(directory, component, version, os_name, arch)
            self.assertIn("cannot read checksum sidecar", stderr.getvalue())
            self.assertNotIsInstance(raised.exception, UnicodeDecodeError)


def _write_sidecar(path: Path, filename: str) -> None:
    """Write the strict `sha256sum` sidecar the verifier expects."""
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    Path(f"{path}.sha256").write_text(f"{digest}  {filename}\n", encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
