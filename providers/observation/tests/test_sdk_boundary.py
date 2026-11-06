"""Prevent absent-effect assertions from accepting empty or partial artifacts."""

from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from sdk_boundary_cases import verify_artifact


class ArtifactTests(unittest.TestCase):
    def test_syscall_denial_requires_absent_marker(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            for mode in ("exec", "file", "partial"):
                self.assertFalse(verify_artifact(marker, mode, True, True))
                for content in (b"", b"syn", b"synthetic"):
                    with self.subTest(mode=mode, content=content):
                        marker.write_bytes(content)
                        with self.assertRaises(AssertionError):
                            verify_artifact(marker, mode, True, True)
                        marker.unlink()

    def test_runtime_denial_requires_absent_marker(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            self.assertFalse(verify_artifact(marker, "file", False, False))
            for content in (b"", b"syn", b"synthetic"):
                marker.write_bytes(content)
                with self.assertRaises(AssertionError):
                    verify_artifact(marker, "file", False, False)
            marker.unlink()
            marker.symlink_to("missing-target")
            with self.assertRaises(AssertionError):
                verify_artifact(marker, "file", False, False)

    def test_blocked_inherited_descriptor_requires_existing_empty_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            with self.assertRaises(AssertionError):
                verify_artifact(marker, "inherited_fd", False, False)
            marker.write_bytes(b"")
            self.assertFalse(verify_artifact(marker, "inherited_fd", False, False))
            for content in (b"syn", b"synthetic"):
                marker.write_bytes(content)
                with self.assertRaises(AssertionError):
                    verify_artifact(marker, "inherited_fd", False, False)

    def test_allowed_effect_requires_exact_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            for mode in ("exec", "file", "partial", "inherited_fd"):
                with self.assertRaises(AssertionError):
                    verify_artifact(marker, mode, True, False)
                for content in (b"", b"syn", b"synthetic-extra"):
                    marker.write_bytes(content)
                    with self.assertRaises(AssertionError):
                        verify_artifact(marker, mode, True, False)
                marker.write_bytes(b"synthetic")
                self.assertTrue(verify_artifact(marker, mode, True, False))
                marker.unlink()

    def test_memory_operation_creates_no_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "effect"
            self.assertFalse(verify_artifact(marker, "memory", True, False))
            marker.write_bytes(b"")
            with self.assertRaises(AssertionError):
                verify_artifact(marker, "memory", True, False)


if __name__ == "__main__":
    unittest.main()
