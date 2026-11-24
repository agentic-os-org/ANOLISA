"""Verify release promotion provenance and the asset checks shared by both paths."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AW / "scripts"))
import release_candidate


class AcceptedArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.sha = "a" * 40
        self.repo = release_candidate.REPOSITORY
        self.run = {
            "id": 42, "run_attempt": 2, "head_sha": self.sha, "head_branch": "main",
            "event": "workflow_dispatch", "head_repository": {"full_name": self.repo},
            "path": ".github/workflows/aw-release.yml", "status": "completed",
            "conclusion": "success",
        }
        self.artifact = {
            "id": 17, "name": f"aw-release-{self.sha}-2", "expired": False,
            "digest": "sha256:" + "b" * 64, "workflow_run": {"id": 42, "head_sha": self.sha},
        }

    def test_accepts_only_the_successful_attempt(self) -> None:
        with patch.object(release_candidate, "api", side_effect=[
            {"workflow_runs": [self.run]}, {"artifacts": [self.artifact]},
        ]) as api:
            self.assertEqual(release_candidate.accepted_artifact(self.sha, self.repo), self.artifact)
        self.assertIn("workflows/aw-release.yml/runs?", api.call_args_list[0].args[0])
        self.assertIn(f"head_sha={self.sha}", api.call_args_list[0].args[0])
        self.assertEqual(api.call_args.args[0], f"runs/42/artifacts?name=aw-release-{self.sha}-2")

    def test_rejects_other_or_unfinished_runs(self) -> None:
        for field, value in [
            ("event", "pull_request"), ("head_branch", "other"), ("head_sha", "b" * 40),
            ("path", ".github/workflows/aw-integration.yml"), ("status", "in_progress"),
            ("conclusion", "cancelled"), ("conclusion", "failure"),
            ("id", False), ("run_attempt", 0),
            ("head_repository", {"full_name": "other/fork"}),
        ]:
            with self.subTest(field=field, value=value), patch.object(
                release_candidate, "api", return_value={"workflow_runs": [self.run | {field: value}]}
            ) as api:
                self.assertIsNone(release_candidate.accepted_artifact(self.sha, self.repo))
                api.assert_called_once()
        with patch.object(release_candidate, "api") as api:
            self.assertIsNone(release_candidate.accepted_artifact(self.sha, "other/fork"))
            api.assert_not_called()

    def test_expired_missing_ambiguous_or_previous_attempt_requires_validation(self) -> None:
        for assets in [
            [], [self.artifact, self.artifact], [self.artifact | {"expired": True}],
            [self.artifact | {"expired": None}], [self.artifact | {"id": "17"}],
            [self.artifact | {"workflow_run": {"id": 8, "head_sha": self.sha}}],
            [self.artifact | {"name": f"aw-release-{self.sha}-1"}], [self.artifact | {"digest": None}],
        ]:
            with self.subTest(assets=assets), patch.object(release_candidate, "api", side_effect=[
                {"workflow_runs": [self.run]}, {"artifacts": assets},
            ]):
                self.assertIsNone(release_candidate.accepted_artifact(self.sha, self.repo))

    def test_lookup_failures_leave_promotion_outputs_empty(self) -> None:
        runs = json.dumps({"workflow_runs": [self.run]})
        missing_id = self.artifact.copy()
        del missing_id["id"]
        missing_attempt = self.run.copy()
        del missing_attempt["run_attempt"]
        cases = {
            "missing gh": [FileNotFoundError("gh")],
            "timeout": [subprocess.TimeoutExpired("gh api", 20)],
            "invalid JSON": ["<html>"],
            "missing run attempt": [json.dumps({"workflow_runs": [missing_attempt]})],
            "null repository": [json.dumps({"workflow_runs": [self.run | {"head_repository": None}]})],
            "missing artifact ID": [runs, json.dumps({"artifacts": [missing_id]})],
            "malformed artifact source": [runs, json.dumps({
                "artifacts": [self.artifact | {"workflow_run": ["wrong type"]}]
            })],
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            env = {"GITHUB_SHA": self.sha, "GITHUB_REPOSITORY": self.repo, "GITHUB_OUTPUT": str(output)}
            for name, responses in cases.items():
                with self.subTest(name=name), patch.dict(os.environ, env), patch(
                    "release_candidate.subprocess.check_output", side_effect=responses
                ):
                    output.write_text("")
                    release_candidate.main()
                    self.assertEqual(output.read_text(), "")


class ReleaseWorkflowTests(unittest.TestCase):
    def test_only_accepted_assets_can_bypass_contracts_and_integration(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-release.yml").read_text()
        contracts = workflow.split("\n  contracts:\n", 1)[1].split("\n  integration:\n", 1)[0]
        self.assertIn("if: needs.candidate.outputs.artifact_id == ''", contracts)
        self.assertIn("  integration:\n    needs: [candidate, contracts]\n", workflow)
        self.assertIn("!cancelled() && needs.candidate.result == 'success'", workflow)
        self.assertIn("needs.contracts.result == 'success' && needs.integration.result == 'success'", workflow)
        self.assertLess(
            workflow.index('git merge-base --is-ancestor "$GITHUB_SHA" refs/remotes/origin/main'),
            workflow.index("python3 -B src/aw/scripts/release_candidate.py"),
        )


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
            [sys.executable, "-B", str(AW.parents[1] / "distribution/packaging/aw/verify_release_assets.py"),
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
