"""Verify backend-only integration builds without invoking Linux compilers."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import integration
from test_packages import Fixture


class IntegrationTests(Fixture):
    def test_backend_selection_never_compiles_framework_or_provider(self) -> None:
        for selected in (["sec-core"], ["tokenless"], ["sec-core tokenless"]):
            with self.subTest(selected=selected), patch.object(
                integration.platform, "system", return_value="Linux"
            ), patch.object(integration.platform, "machine", return_value="x86_64"), patch.object(
                integration, "source_commit", return_value=self.commit
            ), patch.object(
                integration, "recheck_source"
            ) as recheck, patch.object(
                integration, "compile_target"
            ) as compile_target, patch.object(
                integration, "prepare_rtk", return_value=self.repo / "fresh-rtk"
            ) as prepare, patch.object(
                integration, "rtk_provenance", return_value={"source": self.commit}
            ) as provenance:
                integration.build(self.repo, selected, self.cancel)
                packages = {name for call in compile_target.call_args_list for name in call.args[1]}
                expected = set()
                if "sec-core" in integration.backends(selected):
                    expected.update(("asc-cli", "asc-daemon"))
                if "tokenless" in integration.backends(selected):
                    expected.update(("tokenless-cli", "rtk"))
                    prepare.assert_called_once()
                    provenance.assert_called_once()
                    receipt = self.repo / "src/aw/target/rtk-provenance.json"
                    self.assertEqual(json.loads(receipt.read_text()), {"source": self.commit})
                else:
                    prepare.assert_not_called()
                    provenance.assert_not_called()
                self.assertEqual(packages, expected)
                recheck.assert_called_once_with(self.repo, self.commit, self.cancel)

    def test_rtk_preparation_preserves_cached_source_and_copies_patched_dependencies(self) -> None:
        cached = self.file("src/tokenless/third_party/rtk/local-edit", b"preserve")
        for name in integration.RTK_SHARED_SOURCES:
            self.file(f"src/tokenless/crates/tokenless-stats/src/{name}")
        owned = self.repo / "owned"

        def setup(argv: list[str], *args: object) -> bytes:
            rtk = Path(argv[-1])
            (rtk / "src/core").mkdir(parents=True)
            for name in integration.RTK_SHARED_SOURCES:
                relative = rtk / "src/core/../../../../crates/tokenless-stats/src" / name
                self.assertEqual(relative.read_bytes(), b"fixture")
            return b""

        with patch.object(integration, "run", side_effect=setup) as command:
            prepared = integration.prepare_rtk(self.repo, owned, self.cancel)
        self.assertEqual(prepared, owned / "third_party/rtk")
        self.assertEqual(cached.read_bytes(), b"preserve")
        self.assertEqual((owned / "crates/tokenless-stats/src/home.rs").read_bytes(), b"fixture")
        self.assertEqual(command.call_args.args[0][-1], str(prepared))

    def test_rtk_provenance_receipts_every_patched_input_and_the_built_binary(self) -> None:
        rtk = self.repo / "owned/third_party/rtk"
        self.file("owned/third_party/rtk/LICENSE", b"upstream license")
        self.file("owned/third_party/rtk/.anolisa-rtk-commit", b"b" * 40 + b"\n")
        for name in integration.RTK_SHARED_SOURCES:
            self.file(f"owned/crates/tokenless-stats/src/{name}", name.encode())
        self.file("src/tokenless/packaging/build/setup-rtk.sh")
        for name in ("rtk-tokenless-stats.patch", "rtk-pytest-error-report.patch"):
            self.file(f"src/tokenless/third_party/patches/{name}")
        binary = self.binary("src/aw", "rtk")
        receipt = integration.rtk_provenance(self.repo, rtk, self.commit, binary, self.cancel)
        self.assertEqual(receipt["upstream"]["commit"], "b" * 40)
        self.assertEqual(receipt["anolisa_source_commit"], self.commit)
        self.assertIn("call-scoped receipt", receipt["shared_sources_reason"])
        inputs = {item["input"]: item for item in receipt["inputs"]}
        self.assertEqual(len(inputs), 8)
        for name in integration.RTK_SHARED_SOURCES:
            item = inputs[f"tokenless-stats/{name}"]
            self.assertEqual(item["source"], f"src/tokenless/crates/tokenless-stats/src/{name}")
            self.assertEqual(
                item["sha256"],
                integration.digest(self.repo / f"owned/crates/tokenless-stats/src/{name}", self.cancel),
            )
        self.assertEqual(receipt["binary"]["sha256"], integration.digest(binary, self.cancel))
        (rtk.parent.parent / "crates/tokenless-stats/src/rtk_outcome.rs").unlink()
        with self.assertRaises(FileNotFoundError):
            integration.rtk_provenance(self.repo, rtk, self.commit, binary, self.cancel)

    def test_backend_selection_rejects_package_roles_duplicates_and_empty_input(self) -> None:
        for selected in ([], ["core"], ["all"], ["tokenless tokenless"]):
            with self.subTest(selected=selected), self.assertRaisesRegex(ValueError, "backends"):
                integration.backends(selected)


if __name__ == "__main__":
    unittest.main()
