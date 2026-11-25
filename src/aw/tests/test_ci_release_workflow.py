"""Verify the package-only release gate and exact release assets."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import textwrap
import tempfile
import unittest

WORKFLOWS = Path(__file__).resolve().parents[3] / ".github/workflows"


class ReleaseWorkflowTests(unittest.TestCase):
    def test_release_uses_its_own_tested_archives_before_publication(self) -> None:
        release = (WORKFLOWS / "aw-release.yml").read_text()
        self.assertNotIn("./.github/workflows/aw-ci.yml", release)
        self.assertIn("  integration:\n    needs: candidate\n", release)
        self.assertIn("packages_only: true", release)
        self.assertIn("  assets:\n    needs: [candidate, integration]\n    runs-on:", release)
        self.assertIn("git merge-base --is-ancestor", release)
        self.assertIn('test "$GITHUB_REF_NAME" = "aw/v$version"', release)
        self.assertIn("artifact-ids: ${{ needs.integration.outputs.archive_id || 'missing-archive-id' }}", release)
        assets = release.split("\n  assets:\n", 1)[1]
        self.assertIn("ref: ${{ env.CANDIDATE }}", assets)
        verify = assets.index("verify_release_assets.py")
        contract = assets.index('release_contract.py "$runtime/aw" assets/sdk-candidate.json')
        self.assertLess(verify, contract)
        self.assertLess(contract, assets.index("actions/upload-artifact"))
        self.assertIn('tar -xOf "assets/$bundle.tar.gz" "$bundle/bin/aw"', assets[verify:contract])
        self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", release)

        publish = (WORKFLOWS / "release.yaml").read_text()
        self.assertIn("uses: ./.github/workflows/aw-release.yml", publish)
        self.assertIn("needs.aw_assets.result == 'success'", publish)
        self.assertIn("name: aw-release-${{ github.sha }}", publish)
        self.assertIn("sha256sum --strict -c SHA256SUMS", publish)

    def test_package_only_integration_requires_success_and_an_archive(self) -> None:
        workflow = (WORKFLOWS / "aw-integration.yml").read_text()
        triggers = workflow.split("\n  workflow_call:\n", 1)
        self.assertNotIn("packages_only", triggers[0])
        self.assertIn("packages_only:\n        description: Build and test installable packages for release\n"
                      "        type: boolean\n        default: false", triggers[1])
        result = workflow.split("\n  result:\n", 1)[1]
        step = result.split("      - name: Require tested release archives\n", 1)[1]
        step = step.split("      - uses:", 1)[0]
        self.assertIn("if: inputs.packages_only", step)
        self.assertIn("ARCHIVE_ID: ${{ needs.packages.outputs.archive_id }}", step)
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        for outcome, archive, passes in (
            ("success", "17", True), ("success", "", False),
            ("failure", "17", False), ("cancelled", "17", False), ("skipped", "", False),
        ):
            with self.subTest(outcome=outcome, archive=archive):
                result = subprocess.run(
                    ["bash", "-eu", "-c", script],
                    env={**os.environ, "PACKAGE_RESULT": outcome, "ARCHIVE_ID": archive},
                    timeout=5,
                )
                self.assertEqual(result.returncode == 0, passes)


class ReleaseReceiptsTests(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.sha = "a" * 40
        self.version = "0.2.1"
        for name in ("aw-core", "aw-provider-sec-core", "aw-provider-tokenless"):
            archive = f"{name}-{self.version}-linux-x86_64"
            receipt = "share/doc/aw/source.json" if name == "aw-core" else "package.json"
            data = json.dumps({"package_id": name, "version": self.version, "source_commit": self.sha}).encode()
            info = tarfile.TarInfo(f"{archive}/{receipt}")
            info.size = len(data)
            with tarfile.open(self.root / f"{archive}.tar.gz", "w:gz") as file:
                file.addfile(info, io.BytesIO(data))
        records = []
        for kind, components, suffix in [
            ("npm", ("core", "qoder", "pi"), ".tgz"), ("wheel", ("core", "claude", "qwenpaw"), ".whl"),
        ]:
            for component in components:
                name = f"{component}{suffix}"
                (self.root / name).write_bytes(b"SDK")
                records.append({
                    "kind": kind, "component": component, "file": name,
                    "sha256": hashlib.sha256(b"SDK").hexdigest(),
                })
        (self.root / "sdk-candidate.json").write_text(json.dumps({
            "schema": "aw-sdk-candidate/v2", "commit": self.sha, "artifacts": records,
        }))
        self.write_checksums()

    def write_checksums(self) -> None:
        files = [
            *sorted(self.root.glob("*.tar.gz")), *self.root.glob("*.tgz"),
            *self.root.glob("*.whl"), self.root / "sdk-candidate.json",
        ]
        sums = [f"{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.name}\n" for file in files]
        (self.root / "SHA256SUMS").write_text("".join(sums))

    def check(self, sha: str) -> int:
        return subprocess.run(
            [sys.executable, "-B", str(WORKFLOWS.parents[1] / "distribution/packaging/aw/verify_release_assets.py"),
             str(self.root), sha, self.version], capture_output=True, timeout=5,
        ).returncode

    def test_only_matching_receipts_and_complete_unchanged_bytes_pass(self) -> None:
        self.assertEqual(self.check(self.sha), 0)
        self.assertNotEqual(self.check("b" * 40), 0)
        (self.root / "extra.tar.gz").touch()
        self.assertNotEqual(self.check(self.sha), 0)
        (self.root / "extra.tar.gz").unlink()
        (self.root / "core.whl").write_bytes(b"changed after validation")
        self.write_checksums()
        self.assertNotEqual(self.check(self.sha), 0)

    def test_partial_or_duplicate_sdk_set_and_unlisted_packages_fail(self) -> None:
        receipt = self.root / "sdk-candidate.json"
        contents = json.loads(receipt.read_text())
        for records in ([], contents["artifacts"][:-1], [contents["artifacts"][0]] * 6):
            receipt.write_text(json.dumps(contents | {"artifacts": records}))
            self.write_checksums()
            self.assertNotEqual(self.check(self.sha), 0)
        receipt.write_text(json.dumps(contents))
        (self.root / "unlisted.tgz").touch()
        self.write_checksums()
        self.assertNotEqual(self.check(self.sha), 0)
