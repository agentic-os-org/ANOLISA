"""Historical installer acceptance cannot silently use missing or unsafe evidence."""

import io
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import Callable, Optional
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import check_legacy_cli as legacy


def write_archive(path: Path, entries: list[tuple[str, bytes, bytes]]) -> None:
    with tarfile.open(path, "w:") as archive:
        for name, content, kind in entries:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.mode = 0o755 if kind == tarfile.DIRTYPE else 0o644
            member.size = len(content) if kind == tarfile.REGTYPE else 0
            archive.addfile(member, io.BytesIO(content) if member.size else None)


class LegacyInstallerTests(unittest.TestCase):
    def fixture(self) -> tuple[Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        aw = root / "src/aw"
        aw.mkdir(parents=True)
        return root, aw

    def runner(
        self,
        aw: Path,
        *,
        failure: Optional[str] = None,
        binary: str = "valid",
        help_text: Optional[str] = None,
    ) -> tuple[Callable[..., str], list[tuple[list[str], Path, float, bool]]]:
        calls: list[tuple[list[str], Path, float, bool]] = []

        def run(command: list[str], cwd: Path, timeout: float = 600, capture: bool = False) -> str:
            calls.append((command, cwd, timeout, capture))
            if failure == "timeout" and command[0] == "cargo":
                raise subprocess.TimeoutExpired(command, timeout)
            if failure == command[0]:
                raise subprocess.CalledProcessError(1, command)
            if command[:2] == ["git", "rev-parse"]:
                return legacy.LEGACY_SOURCE
            if command[:2] == ["git", "archive"]:
                write_archive(Path(command[command.index("--output") + 1]), [
                    ("src", b"", tarfile.DIRTYPE),
                    ("src/aw/Cargo.toml", b"[workspace]\n", tarfile.REGTYPE),
                ])
            if command[0] == "cargo" and binary != "missing":
                for name in ("aw-package", "aw"):
                    if binary == "core_missing" and name == "aw":
                        continue
                    path = aw / "target/legacy-cli-build/debug" / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("fixture, never executed", encoding="utf-8")
                    path.chmod(0o644 if binary == "nonexe" else 0o755)
            if command[-1] == "--help":
                return help_text if help_text is not None else "\n".join(
                    f"aw-package {action}" for action in ("install", "uninstall", "configure")
                )
            return ""

        return run, calls

    @patch.object(legacy.sys, "platform", "linux")
    def test_pinned_source_is_built_locked_and_processes_are_bounded(self) -> None:
        root, aw = self.fixture()
        run, calls = self.runner(aw)
        result = legacy.prepare_legacy_cli(aw, root, run)
        self.assertEqual(result, aw / "target/legacy-cli-build/debug/aw-package")
        self.assertEqual(calls[0][0][2], legacy.LEGACY_SOURCE + "^{commit}")
        build = next(command for command, *_ in calls if command[0] == "cargo")
        self.assertIn("--locked", build)
        self.assertEqual(
            [build[index + 1] for index, value in enumerate(build) if value == "--bin"],
            ["aw-package", "aw"],
        )
        self.assertEqual(
            build[build.index("--config") + 1], "profile.dev.package.sha2.opt-level=3"
        )
        self.assertTrue(all(0 < timeout <= 600 for _, _, timeout, _ in calls))
        self.assertEqual(list((aw / "target").glob("legacy-cli-source-*")), [])

    @patch.object(legacy.sys, "platform", "linux")
    def test_missing_source_and_failed_build_are_errors(self) -> None:
        for failure in ("git", "cargo"):
            with self.subTest(failure=failure):
                root, aw = self.fixture()
                run, _ = self.runner(aw, failure=failure)
                with self.assertRaises(subprocess.CalledProcessError):
                    legacy.prepare_legacy_cli(aw, root, run)

    @patch.object(legacy.sys, "platform", "linux")
    def test_missing_nonexecutable_and_wrong_binary_fail(self) -> None:
        for binary, help_text in (("missing", None), ("core_missing", None),
                                  ("nonexe", None), ("valid", "other CLI")):
            with self.subTest(binary=binary, help=help_text):
                root, aw = self.fixture()
                run, _ = self.runner(aw, binary=binary, help_text=help_text)
                with self.assertRaises((ValueError, FileNotFoundError)):
                    legacy.prepare_legacy_cli(aw, root, run)

    @patch.object(legacy.sys, "platform", "linux")
    def test_build_timeout_is_not_converted_into_a_skip(self) -> None:
        root, aw = self.fixture()
        run, _ = self.runner(aw, failure="timeout")
        with self.assertRaises(subprocess.TimeoutExpired):
            legacy.prepare_legacy_cli(aw, root, run)
        self.assertEqual(list((aw / "target").glob("legacy-cli-source-*")), [])

    @patch.object(legacy.sys, "platform", "linux")
    def test_wrong_source_commit_never_starts_a_build(self) -> None:
        root, aw = self.fixture()
        with self.assertRaisesRegex(ValueError, "source SHA"):
            legacy.prepare_legacy_cli(aw, root, lambda *_a, **_kw: "a" * 40)
        self.assertFalse((aw / "target").exists())

    def test_unsupported_platform_never_starts_a_build(self) -> None:
        root, aw = self.fixture()
        with patch.object(legacy.sys, "platform", "darwin"):
            with self.assertRaisesRegex(ValueError, "requires Linux"):
                legacy.prepare_legacy_cli(aw, root, lambda *_a, **_kw: self.fail("ran build"))

    def test_archive_traversal_links_and_unexpected_paths_fail(self) -> None:
        root, _ = self.fixture()
        for name, kind in (("../outside", tarfile.REGTYPE), ("/outside", tarfile.REGTYPE),
                           ("src/aw/link", tarfile.SYMTYPE), ("src/aw/hard", tarfile.LNKTYPE),
                           ("other/Cargo.toml", tarfile.REGTYPE)):
            with self.subTest(name=name, kind=kind):
                archive = root / "source.tar"
                write_archive(archive, [(name, b"", kind)])
                with self.assertRaises(ValueError):
                    legacy.extract_source(archive, root / "destination")

    def test_archive_byte_limit_is_enforced(self) -> None:
        root, _ = self.fixture()
        archive = root / "source.tar"
        write_archive(archive, [("src/aw/Cargo.toml", b"too much", tarfile.REGTYPE)])
        with patch.object(legacy, "MAX_ARCHIVE_BYTES", 2):
            with self.assertRaisesRegex(ValueError, "byte limit"):
                legacy.extract_source(archive, root / "destination")


if __name__ == "__main__":
    unittest.main()
