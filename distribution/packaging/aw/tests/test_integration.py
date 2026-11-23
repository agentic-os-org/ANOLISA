"""Verify backend-only integration builds without invoking Linux compilers."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import call, patch

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
                integration, "prepare_tokenless", return_value=self.repo / "export"
            ) as prepare, patch.object(
                integration, "rtk_provenance", return_value={"source": self.commit}
            ) as provenance:
                integration.build(self.repo, selected, self.cancel)
                packages = {name for call in compile_target.call_args_list for name in call.args[1]}
                expected = set()
                if "sec-core" in integration.backends(selected):
                    expected.update(("asc-cli", "asc-daemon"))
                if "tokenless" in integration.backends(selected):
                    expected.add("tokenless-cli")
                    prepare.assert_called_once()
                    self.assertEqual(prepare.call_args.args[:2], (self.repo, self.commit))
                    self.assertIn(
                        call(self.repo / "export", ("tokenless-cli",), self.cancel,
                             self.repo / "src/tokenless/target"),
                        compile_target.call_args_list,
                    )
                    provenance.assert_called_once_with(
                        self.repo / "export", self.commit,
                        self.repo / "src/tokenless/target/release/tokenless", self.cancel,
                    )
                    receipt = self.repo / "src/aw/target/rtk-provenance.json"
                    self.assertEqual(json.loads(receipt.read_text()), {"source": self.commit})
                else:
                    prepare.assert_not_called()
                    provenance.assert_not_called()
                self.assertEqual(packages, expected)
                recheck.assert_called_once_with(self.repo, self.commit, self.cancel)

    @unittest.skipUnless(
        all(shutil.which(tool) for tool in ("git", "tar", "bash")),
        "export preparation needs git, tar and bash",
    )
    def test_tokenless_preparation_exports_the_commit_and_fetches_rtk_inside_it(self) -> None:
        self.file(
            "src/tokenless/packaging/build/setup-rtk.sh",
            b'root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"\n'
            b'mkdir -p "$root/third_party/rtk"\n'
            b'printf "%s\\n" "$#" > "$root/third_party/rtk/arguments"\n',
        )
        self.file("src/tokenless/Cargo.toml", b"committed")
        self.file(".gitignore", b"/src/tokenless/target/\n/src/tokenless/third_party/rtk/\n")
        environment = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
        with patch.dict(os.environ, environment):
            for argv in (
                ["init", "-q"], ["add", "."],
                ["-c", "user.name=t", "-c", "user.email=t@example.invalid",
                 "commit", "-q", "-m", "fixture"],
            ):
                subprocess.run(["git", *argv], cwd=self.repo, check=True)
            commit = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=self.repo, check=True,
                capture_output=True, text=True,
            ).stdout.strip()
            cached = self.file("src/tokenless/third_party/rtk/local-edit", b"preserve")
            self.file("src/tokenless/target/release/stale", b"ignored")
            self.file("src/tokenless/Cargo.toml", b"uncommitted")
            owned = self.repo / "owned"
            owned.mkdir()
            tree = integration.prepare_tokenless(self.repo, commit, owned, self.cancel)
        self.assertEqual(tree, owned / "tokenless")
        self.assertEqual((tree / "Cargo.toml").read_bytes(), b"committed")
        self.assertEqual((tree / "third_party/rtk/arguments").read_text(), "0\n")
        self.assertFalse((tree / "third_party/rtk/local-edit").exists())
        self.assertFalse((tree / "target").exists())
        self.assertFalse((owned / "tokenless.tar").exists())
        self.assertEqual(cached.read_bytes(), b"preserve")

    def test_rtk_provenance_receipts_every_patched_input_and_the_built_cli(self) -> None:
        tree = self.repo / "export"
        self.file("export/third_party/rtk/LICENSE", b"upstream license")
        self.file("export/third_party/rtk/.anolisa-rtk-commit", b"b" * 40 + b"\n")
        for name in integration.RTK_SHARED_SOURCES:
            self.file(f"export/crates/tokenless-stats/src/{name}", name.encode())
        self.file("export/packaging/build/setup-rtk.sh")
        patches = ("rtk-tokenless-stats.patch", "rtk-pytest-error-report.patch", "rtk-library.patch")
        for name in patches:
            self.file(f"export/third_party/patches/{name}", name.encode())
        binary = self.binary("src/tokenless", "tokenless")
        receipt = integration.rtk_provenance(tree, self.commit, binary, self.cancel)
        self.assertEqual(receipt["upstream"]["commit"], "b" * 40)
        self.assertEqual(receipt["anolisa_source_commit"], self.commit)
        self.assertIn("call-scoped receipt", receipt["shared_sources_reason"])
        inputs = {item["input"]: item for item in receipt["inputs"]}
        self.assertEqual(len(inputs), 9)
        for name in integration.RTK_SHARED_SOURCES:
            item = inputs[f"tokenless-stats/{name}"]
            self.assertEqual(item["source"], f"src/tokenless/crates/tokenless-stats/src/{name}")
            self.assertEqual(
                item["sha256"],
                integration.digest(tree / f"crates/tokenless-stats/src/{name}", self.cancel),
            )
        for name in patches:
            self.assertEqual(inputs[name]["source"], f"src/tokenless/third_party/patches/{name}")
            self.assertEqual(
                inputs[name]["sha256"],
                integration.digest(tree / f"third_party/patches/{name}", self.cancel),
            )
        self.assertEqual(
            receipt["binary"],
            {"name": "tokenless", "sha256": integration.digest(binary, self.cancel)},
        )
        (tree / "crates/tokenless-stats/src/rtk_outcome.rs").unlink()
        with self.assertRaises(FileNotFoundError):
            integration.rtk_provenance(tree, self.commit, binary, self.cancel)

    def test_backend_selection_rejects_package_roles_duplicates_and_empty_input(self) -> None:
        for selected in ([], ["core"], ["all"], ["tokenless tokenless"]):
            with self.subTest(selected=selected), self.assertRaisesRegex(ValueError, "backends"):
                integration.backends(selected)


if __name__ == "__main__":
    unittest.main()
