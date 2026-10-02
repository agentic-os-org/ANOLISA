"""End-to-end workflow tests for check, certify, and audit.

These tests use real Ed25519 cryptography with temp directories — no mocks
for the signing layer.  They protect the actual security-critical paths:

1. **Check state machine** — the security gate called on every skill invocation.
   Every state (none/drifted/tampered/deny/warn/pass) must be correct.
2. **Certify scan merge** — scanner results are accumulated correctly, old
   entries for the same scanner replaced (not duplicated).
3. **Audit chain integrity** — broken previousManifestSignature chain and
   tampered manifestHash must both be detected.
"""

import base64
import hashlib
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from agent_sec_cli.skill_ledger.core.auditor import audit
from agent_sec_cli.skill_ledger.core.certifier import certify, scan_skill
from agent_sec_cli.skill_ledger.core.checker import (
    check,
    check_batch,
    manifest_only_status,
)
from agent_sec_cli.skill_ledger.core.decision import (
    clear_decision,
    decide_skill,
    export_skill,
    rollback_skill,
    show_skill,
)
from agent_sec_cli.skill_ledger.core.exposure import build_exposure_summary
from agent_sec_cli.skill_ledger.core.file_hasher import (
    compute_file_hashes,
    diff_file_hashes,
)
from agent_sec_cli.skill_ledger.errors import (
    KeyNotFoundError,
    SignatureInvalidError,
    SkillLedgerError,
)
from agent_sec_cli.skill_ledger.models.manifest import (
    ManifestSignature,
    SignedManifest,
)
from agent_sec_cli.skill_ledger.models.scan import ScanEntry
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

# ---------------------------------------------------------------------------
# In-memory Ed25519 backend for testing (no filesystem key storage)
# ---------------------------------------------------------------------------


class InMemoryEd25519Backend(SigningBackend):
    """A test-only signing backend that holds keys in memory."""

    def __init__(self):
        self._private_key = Ed25519PrivateKey.generate()
        raw_pub = self._private_key.public_key().public_bytes(
            Encoding.Raw, PublicFormat.Raw
        )
        self._fingerprint = f"sha256:{hashlib.sha256(raw_pub).hexdigest()}"

    @property
    def name(self) -> str:
        return "ed25519"

    def generate_keys(self, passphrase=None):
        """No-op for in-memory backend — keys are generated in __init__."""
        return {"fingerprint": self._fingerprint}

    def sign(self, data: bytes) -> tuple[str, str]:
        raw_sig = self._private_key.sign(data)
        return base64.b64encode(raw_sig).decode("ascii"), self._fingerprint

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        if fingerprint != self._fingerprint:
            raise SignatureInvalidError(f"Unknown fingerprint {fingerprint}")
        raw_sig = base64.b64decode(signature_b64)
        try:
            self._private_key.public_key().verify(raw_sig, data)
            return True
        except InvalidSignature:
            raise SignatureInvalidError("Signature verification failed")

    def get_public_key_fingerprint(self) -> str:
        return self._fingerprint


class VerifyFalseBackend(SigningBackend):
    """Signing backend wrapper whose verify method returns ``False``."""

    def __init__(self, delegate: SigningBackend):
        self._delegate = delegate

    @property
    def name(self) -> str:
        return self._delegate.name

    def generate_keys(self, passphrase=None):
        return self._delegate.generate_keys(passphrase)

    def sign(self, data: bytes) -> tuple[str, str]:
        return self._delegate.sign(data)

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        return False

    def get_public_key_fingerprint(self) -> str:
        return self._delegate.get_public_key_fingerprint()


class KeyMissingVerifyBackend(VerifyFalseBackend):
    """Signing backend wrapper that raises missing-key errors on verify."""

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        raise KeyNotFoundError("/tmp/missing-test-key.pub")


# ---------------------------------------------------------------------------
# Test helper: manage a temp skill directory
# ---------------------------------------------------------------------------


class SkillDirTestCase(unittest.TestCase):
    """Base class that creates a temp skill directory with sample files."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.skill_dir = os.path.join(self.tmpdir, "test-skill")
        os.makedirs(self.skill_dir)
        # Create sample skill files
        self._write_file("run.sh", "#!/bin/bash\necho hello\n")
        self._write_file(
            "SKILL.md",
            "---\nname: test-skill\ndescription: Test skill\n---\n# Test Skill\n",
        )
        self.backend = InMemoryEd25519Backend()
        # Patch config to avoid touching user's real config
        self._patch_config()

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _write_file(self, name: str, content: str):
        path = os.path.join(self.skill_dir, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)

    def _write_findings(self, findings: list[dict]) -> str:
        path = os.path.join(self.tmpdir, "findings.json")
        with open(path, "w") as f:
            json.dump(findings, f)
        return path

    def _manifest_path(self, version_id: str | None = None) -> str:
        metadata_dir = os.path.join(self.skill_dir, ".skill-meta")
        if version_id is None:
            return os.path.join(metadata_dir, "latest.json")
        return os.path.join(metadata_dir, "versions", f"{version_id}.json")

    def _read_manifest(self, version_id: str | None = None) -> dict:
        with open(self._manifest_path(version_id), "r") as f:
            return json.load(f)

    def _write_manifest(
        self,
        data: dict,
        version_id: str | None = None,
    ) -> None:
        with open(self._manifest_path(version_id), "w") as f:
            json.dump(data, f)

    def _resign_manifest_data(
        self,
        data: dict,
        backend: SigningBackend | None = None,
    ) -> dict:
        manifest = SignedManifest.model_validate(data)
        manifest.manifestHash = manifest.compute_manifest_hash()
        signer = backend or self.backend
        signature, fingerprint = signer.sign(manifest.manifestHash.encode("utf-8"))
        manifest.signature = ManifestSignature(
            algorithm=signer.name,
            value=signature,
            keyFingerprint=fingerprint,
        )
        return manifest.model_dump()

    def _patch_config(self):
        """Point config to a temp config dir so tests don't touch real user config."""
        config_dir = os.path.join(self.tmpdir, "config")
        os.makedirs(config_dir, exist_ok=True)
        os.environ["XDG_CONFIG_HOME"] = self.tmpdir
        self.addCleanup(lambda: os.environ.pop("XDG_CONFIG_HOME", None))


# ---------------------------------------------------------------------------
# Check state machine
# ---------------------------------------------------------------------------


class TestCheckStateMachine(SkillDirTestCase):
    """Tests for the check command — the security gate.

    The check state machine has 6 possible outputs:
    none, drifted, tampered, deny, warn, pass.
    Each represents a distinct security posture.
    """

    def test_no_manifest_returns_none_read_only(self):
        """First check on a fresh skill is read-only and returns status=none."""
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "none")
        # check is read-only; scan/certify are responsible for creating versions.
        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        self.assertFalse(os.path.exists(latest))
        self.assertEqual(result["skillName"], "test-skill")
        self.assertIsNone(result["versionId"])
        self.assertIsNone(result["createdAt"])
        self.assertIsNone(result["updatedAt"])
        self.assertIsNone(result["manifestHash"])
        self.assertIsNone(result["fileCount"])

    def test_no_manifest_does_not_hash_files(self):
        """A fresh skill returns none without walking and hashing the tree."""
        with patch(
            "agent_sec_cli.skill_ledger.core.checker.compute_file_hashes",
            side_effect=AssertionError("fresh skill should not be hashed"),
        ):
            result = check(self.skill_dir, self.backend)

        self.assertEqual(result["status"], "none")
        self.assertIsNone(result["fileCount"])

    def test_missing_latest_with_version_artifacts_is_tampered(self):
        """Removing latest cannot downgrade a populated ledger to none."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        os.remove(self._manifest_path())

        with patch(
            "agent_sec_cli.skill_ledger.core.checker.compute_file_hashes",
            side_effect=AssertionError("missing latest must fail before live hashing"),
        ):
            checked = check(self.skill_dir, self.backend)
            manifest_only = manifest_only_status(self.skill_dir, self.backend)

        for result in (checked, manifest_only):
            self.assertEqual(result["status"], "tampered")
            self.assertEqual(
                result["reason"],
                "latest.json is missing while version artifacts exist",
            )
            for field in (
                "versionId",
                "createdAt",
                "updatedAt",
                "fileCount",
                "manifestHash",
                "userDecision",
            ):
                self.assertIsNone(result[field])
            self.assertNotIn("added", result)
            self.assertNotIn("removed", result)
            self.assertNotIn("modified", result)

    def test_unchanged_after_certify_pass(self):
        """certify with all-pass findings → check returns pass with enriched metadata."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "pass")
        # Enriched metadata present for pass status
        self.assertEqual(result["skillName"], "test-skill")
        self.assertIn("versionId", result)
        self.assertIn("manifestHash", result)
        self.assertTrue(result["manifestHash"].startswith("sha256:"))

    def test_snapshot_validation_is_reserved_for_manifest_only_status(self):
        """Full check hashes live files; activation status also verifies snapshot."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        snapshot_file = Path(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000001.snapshot",
            "run.sh",
        )
        snapshot_file.write_text("tampered snapshot\n")

        checked = check(self.skill_dir, self.backend)
        manifest_only = manifest_only_status(self.skill_dir, self.backend)

        self.assertEqual(checked["status"], "pass")
        self.assertEqual(manifest_only["status"], "tampered")
        self.assertIsNone(manifest_only["versionId"])

    def test_drifted_after_file_change(self):
        """Modifying a skill file → check returns drifted."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        # Modify a file
        self._write_file("run.sh", "#!/bin/bash\necho MODIFIED\n")
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "drifted")
        self.assertIn("modified", result)

    def test_drifted_on_file_added(self):
        """Adding a new file → check returns drifted with added list."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("new_file.py", "print('hello')\n")
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "drifted")
        self.assertIn("new_file.py", result["added"])

    def test_drifted_on_file_removed(self):
        """Removing a file → check returns drifted with removed list."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        os.remove(os.path.join(self.skill_dir, "run.sh"))
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "drifted")
        self.assertIn("run.sh", result["removed"])

    def test_tampered_manifest_hash(self):
        """Directly editing the manifest JSON → tampered (hash mismatch)."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        with open(latest, "r") as f:
            data = json.load(f)
        # Tamper: change scanStatus without re-hashing
        data["scanStatus"] = "deny"
        with open(latest, "w") as f:
            json.dump(data, f)
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "tampered")

    def test_manifest_hash_failure_precedes_drift_without_hashing_root(self):
        """Untrusted fileHashes never drive a drift decision."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        data = self._read_manifest()
        data["versionId"] = "v999999"
        data["createdAt"] = "attacker-created"
        data["updatedAt"] = "attacker-updated"
        data["userDecision"] = {
            "action": "always_allow",
            "reason": "attacker-controlled",
        }
        self._write_manifest(data)
        self._write_file("run.sh", "#!/bin/bash\necho drifted\n")

        with patch(
            "agent_sec_cli.skill_ledger.core.checker.compute_file_hashes",
            side_effect=AssertionError("untrusted fileHashes must not be consulted"),
        ):
            result = check(self.skill_dir, self.backend)

        self.assertEqual(result["status"], "tampered")
        for field in (
            "versionId",
            "createdAt",
            "updatedAt",
            "fileCount",
            "manifestHash",
            "userDecision",
        ):
            self.assertIsNone(result[field])
        self.assertNotIn("added", result)
        self.assertNotIn("removed", result)
        self.assertNotIn("modified", result)

    def test_tampered_wrong_key_signature(self):
        """Signing with a different key → tampered (signature mismatch)."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        # Re-sign the manifest with a different key
        other_backend = InMemoryEd25519Backend()
        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        with open(latest, "r") as f:
            data = json.load(f)
        # Recompute hash and sign with wrong key
        m = SignedManifest.from_json(json.dumps(data))
        m.manifestHash = m.compute_manifest_hash()
        sig_val, fp = other_backend.sign(m.manifestHash.encode("utf-8"))
        data["manifestHash"] = m.manifestHash
        data["signature"]["value"] = sig_val
        data["signature"]["keyFingerprint"] = fp
        with open(latest, "w") as f:
            json.dump(data, f)
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "tampered")

    def test_signature_failure_precedes_drift_without_hashing_root(self):
        """A cryptographic failure wins even when the live tree changed."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        data = self._resign_manifest_data(
            self._read_manifest(),
            backend=InMemoryEd25519Backend(),
        )
        self._write_manifest(data)
        self._write_file("run.sh", "#!/bin/bash\necho drifted\n")

        with patch(
            "agent_sec_cli.skill_ledger.core.checker.compute_file_hashes",
            side_effect=AssertionError("invalid signatures must fail before hashing"),
        ):
            result = check(self.skill_dir, self.backend)

        self.assertEqual(result["status"], "tampered")
        self.assertIsNone(result["versionId"])
        self.assertNotIn("modified", result)

    def test_missing_signature_is_tampered_on_both_read_paths(self):
        """Existing unsigned metadata is never treated as a legacy baseline."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        data = self._read_manifest()
        data["signature"] = None
        self._write_manifest(data)

        checked = check(self.skill_dir, self.backend)
        manifest_only = manifest_only_status(self.skill_dir, self.backend)

        for result in (checked, manifest_only):
            self.assertEqual(result["status"], "tampered")
            self.assertEqual(result["reason"], "Missing signature")
            self.assertIsNone(result["versionId"])
            self.assertIsNone(result["manifestHash"])
            self.assertIsNone(result["userDecision"])

    def test_signed_skill_identity_mismatch_is_tampered(self):
        """A valid signature from another skill name cannot be transplanted."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        data = self._read_manifest()
        data["skillName"] = "other-skill"
        self._write_manifest(self._resign_manifest_data(data))

        result = check(self.skill_dir, self.backend)

        self.assertEqual(result["status"], "tampered")
        self.assertIn("skillName", result["reason"])
        self.assertEqual(result["skillName"], "test-skill")
        self.assertIsNone(result["versionId"])

    def test_signature_algorithm_must_match_backend(self):
        """Unsigned algorithm metadata cannot select a different verifier."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        for version_id in (None, "v000001"):
            data = self._read_manifest(version_id)
            data["signature"]["algorithm"] = "none"
            self._write_manifest(data, version_id)

        result = check(self.skill_dir, self.backend)

        self.assertEqual(result["status"], "tampered")
        self.assertEqual(
            result["reason"],
            "signature algorithm does not match backend",
        )

    def test_authenticated_latest_replay_is_tampered_before_live_hashing(self):
        """A signed older version cannot become the authoritative latest pointer."""
        pass_findings = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=pass_findings)
        trusted_v1 = self._read_manifest("v000001")
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        deny_findings = self._write_findings(
            [{"rule": "r2", "level": "deny", "message": "deny"}]
        )
        certify(self.skill_dir, self.backend, findings_path=deny_findings)
        self._write_manifest(trusted_v1)

        with patch(
            "agent_sec_cli.skill_ledger.core.checker.compute_file_hashes",
            side_effect=AssertionError("replayed latest must fail before live hashing"),
        ):
            result = check(self.skill_dir, self.backend)

        summary = build_exposure_summary(
            self.skill_dir,
            self.backend,
            status_result=result,
        )
        self.assertEqual(result["status"], "tampered")
        self.assertIsNone(result["versionId"])
        self.assertEqual(summary["latestStatus"], "tampered")
        self.assertEqual(summary["reasonCode"], "tampered")

    def test_invalid_artifact_suppresses_older_always_allow_decision(self):
        """An invalid artifact blocks reuse of an older allowing decision."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "deny", "message": "risk"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        decided = decide_skill(
            self.skill_dir,
            self.backend,
            action="always_allow",
        )
        self.assertEqual(decided["activation"]["activeVersionId"], "v000001")

        Path(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000002.snapshot",
        ).mkdir()
        self._write_file("run.sh", "#!/bin/bash\necho changed\n")
        recovered = certify(
            self.skill_dir,
            self.backend,
            findings_path=findings_path,
        )
        self.assertEqual(recovered["versionId"], "v000003")
        clear_decision(self.skill_dir, self.backend)

        summary = build_exposure_summary(self.skill_dir, self.backend)

        self.assertIsNone(summary["activeVersionId"])
        self.assertIsNone(summary["userDecision"])
        self.assertEqual(summary["reasonCode"], "latest_risk_pending_decision")

    def test_tampered_diagnostics_do_not_echo_manifest_values(self):
        """Signature and schema failures return stable public diagnostics."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted = self._read_manifest()
        for version_id in (None, "v000001"):
            data = self._read_manifest(version_id)
            data["signature"]["keyFingerprint"] = "attacker-sentinel"
            self._write_manifest(data, version_id)

        signature_result = check(self.skill_dir, self.backend)

        self.assertEqual(signature_result["status"], "tampered")
        self.assertNotIn("attacker-sentinel", signature_result["reason"])
        self.assertEqual(signature_result["reason"], "signature verification failed")

        self._write_manifest(trusted, "v000001")
        malformed = dict(trusted)
        malformed["fileHashes"] = ["attacker-sentinel"]
        self._write_manifest(malformed)

        schema_result = check(self.skill_dir, self.backend)

        self.assertEqual(schema_result["status"], "tampered")
        self.assertNotIn("attacker-sentinel", schema_result["reason"])
        self.assertEqual(
            schema_result["reason"],
            "manifest file is corrupted or schema-invalid",
        )

    def test_tampered_when_verify_returns_false(self):
        """A backend returning False from verify is treated as tampered."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        result = check(self.skill_dir, VerifyFalseBackend(self.backend))

        self.assertEqual(result["status"], "tampered")
        self.assertEqual(result["reason"], "signature verification returned false")

    def test_tampered_when_verification_key_is_missing(self):
        """Missing public keys fail closed without exposing backend diagnostics."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        result = check(self.skill_dir, KeyMissingVerifyBackend(self.backend))

        self.assertEqual(result["status"], "tampered")
        self.assertEqual(result["reason"], "manifest signature could not be verified")

    def test_deny_status_passthrough(self):
        """certify with deny findings → check returns deny."""
        findings_path = self._write_findings(
            [
                {"rule": "dangerous-exec", "level": "deny", "message": "exec found"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "deny")

    def test_warn_status_passthrough(self):
        """certify with warn findings → check returns warn."""
        findings_path = self._write_findings(
            [
                {"rule": "obfuscated", "level": "warn", "message": "hex encoded"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        result = check(self.skill_dir, self.backend)
        self.assertEqual(result["status"], "warn")


# ---------------------------------------------------------------------------
# Check batch
# ---------------------------------------------------------------------------


class TestCheckBatch(SkillDirTestCase):
    """Tests for check_batch() — batch checking multiple skill directories."""

    def test_batch_returns_one_result_per_skill(self):
        """check_batch returns one result per input directory."""
        # Create two skill directories
        skill_dir2 = os.path.join(self.tmpdir, "skill-two")
        os.makedirs(skill_dir2)
        with open(os.path.join(skill_dir2, "SKILL.md"), "w") as f:
            f.write("# Skill Two\n")
        with open(os.path.join(skill_dir2, "main.py"), "w") as f:
            f.write("print('hello')\n")

        dirs = [Path(self.skill_dir), Path(skill_dir2)]
        results = check_batch(dirs, self.backend)
        self.assertEqual(len(results), 2)
        for r in results:
            self.assertIn("status", r)
            self.assertIn("skillName", r)

    def test_batch_handles_per_skill_error(self):
        """If one skill dir is invalid, its result has status=error."""
        bad_dir = Path(self.tmpdir) / "nonexistent-skill"
        dirs = [Path(self.skill_dir), bad_dir]
        results = check_batch(dirs, self.backend)
        self.assertEqual(len(results), 2)
        # First should succeed
        self.assertNotEqual(results[0].get("status"), "error")
        # Second should be error
        self.assertEqual(results[1]["status"], "error")
        self.assertIn("error", results[1])


# ---------------------------------------------------------------------------
# Certify workflow
# ---------------------------------------------------------------------------


class TestCertifyWorkflow(SkillDirTestCase):
    """Tests for the certify command — manifest creation and scan merging."""

    def test_certify_creates_version_and_snapshot(self):
        """First certify → creates v000001 manifest + snapshot with enriched output."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "clean"},
            ]
        )
        result = certify(self.skill_dir, self.backend, findings_path=findings_path)
        self.assertEqual(result["versionId"], "v000001")
        self.assertTrue(result["newVersion"])
        self.assertEqual(result["scanStatus"], "pass")
        # Enriched fields present in certify output
        self.assertEqual(result["skillName"], "test-skill")
        self.assertIn("createdAt", result)
        self.assertIn("updatedAt", result)
        self.assertIsInstance(result["fileCount"], int)
        self.assertIn("manifestHash", result)
        self.assertTrue(result["manifestHash"].startswith("sha256:"))
        # Version file and snapshot should exist
        v_file = os.path.join(self.skill_dir, ".skill-meta", "versions", "v000001.json")
        v_snap = os.path.join(
            self.skill_dir, ".skill-meta", "versions", "v000001.snapshot"
        )
        self.assertTrue(os.path.isfile(v_file))
        self.assertTrue(os.path.isdir(v_snap))

    def test_recertify_same_files_no_new_version(self):
        """Certifying again without file changes → same versionId, no new version."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "clean"},
            ]
        )
        r1 = certify(self.skill_dir, self.backend, findings_path=findings_path)
        r2 = certify(self.skill_dir, self.backend, findings_path=findings_path)
        self.assertEqual(r1["versionId"], r2["versionId"])
        self.assertFalse(r2["newVersion"])

    def test_missing_signature_creates_new_version_instead_of_signing_in_place(self):
        """An unsigned latest manifest is recovered as tampered."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        first = certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_v1 = self._read_manifest("v000001")
        unsigned = self._read_manifest()
        unsigned["signature"] = None
        self._write_manifest(unsigned)

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(first["versionId"], "v000001")
        self.assertEqual(recovered["versionId"], "v000002")
        self.assertTrue(recovered["newVersion"])
        self.assertEqual(recovered["auditEvents"][0]["fromStatus"], "tampered")
        self.assertEqual(latest["previousVersionId"], "v000001")
        self.assertEqual(
            latest["previousManifestSignature"],
            trusted_v1["signature"]["value"],
        )

    def test_replayed_older_latest_creates_version_after_newer_history(self):
        """A valid but stale latest cannot make an older version current again."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_v1 = self._read_manifest("v000001")
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_v2 = self._read_manifest("v000002")

        self._write_manifest(trusted_v1)
        self._write_file("run.sh", "#!/bin/bash\necho hello\n")

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000003")
        self.assertTrue(recovered["newVersion"])
        self.assertEqual(latest["previousVersionId"], "v000002")
        self.assertEqual(
            latest["previousManifestSignature"],
            trusted_v2["signature"]["value"],
        )
        self.assertEqual(self._read_manifest("v000001"), trusted_v1)

    def test_missing_highest_version_manifest_preserves_snapshot_and_sequence(self):
        """Recovery reserves version ids whose snapshot evidence still exists."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_v1 = self._read_manifest("v000001")
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        v2_snapshot_file = os.path.join(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000002.snapshot",
            "run.sh",
        )
        with open(v2_snapshot_file, "r") as f:
            v2_snapshot_content = f.read()

        os.remove(self._manifest_path("v000002"))
        self._write_file("run.sh", "#!/bin/bash\necho recovery\n")

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000003")
        self.assertTrue(recovered["newVersion"])
        self.assertEqual(latest["previousVersionId"], "v000001")
        self.assertEqual(
            latest["previousManifestSignature"],
            trusted_v1["signature"]["value"],
        )
        with open(v2_snapshot_file, "r") as f:
            self.assertEqual(f.read(), v2_snapshot_content)
        self.assertTrue(os.path.isfile(self._manifest_path("v000003")))
        audit_result = audit(self.skill_dir, self.backend)
        self.assertFalse(audit_result["valid"])
        self.assertTrue(
            any(
                error["versionId"] == "v000002" and "missing" in error["error"]
                for error in audit_result["errors"]
            )
        )

    def test_unverified_high_version_artifact_cannot_block_recovery(self):
        """An attacker-controlled filename cannot force version-ID overflow."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_v1 = self._read_manifest("v000001")
        bogus_snapshot = Path(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v999999.snapshot",
        )
        bogus_snapshot.mkdir()
        self._write_file("run.sh", "#!/bin/bash\necho recovery\n")

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000002")
        self.assertEqual(latest["previousVersionId"], "v000001")
        self.assertEqual(
            latest["previousManifestSignature"],
            trusted_v1["signature"]["value"],
        )
        audit_result = audit(self.skill_dir, self.backend)
        self.assertFalse(audit_result["valid"])
        self.assertFalse(
            any(error["versionId"] == "latest.json" for error in audit_result["errors"])
        )

    def test_tampered_drift_does_not_launder_manifest_state(self):
        """Forged decisions and scans never enter the recovered signed state."""
        first_findings = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=first_findings)
        trusted_v1 = self._read_manifest("v000001")
        forged = self._read_manifest()
        forged["userDecision"] = {
            "action": "always_allow",
            "reason": "forged allow",
        }
        forged["scans"] = [
            {
                "scanner": "forged-scanner",
                "version": "attacker",
                "status": "pass",
                "findings": [],
                "scannedAt": "attacker-time",
            }
        ]
        forged["scanStatus"] = "pass"
        self._write_manifest(forged)
        self._write_file("run.sh", "#!/bin/bash\necho changed\n")
        second_findings = self._write_findings(
            [{"rule": "r2", "level": "pass", "message": "rescanned"}]
        )

        recovered = certify(self.skill_dir, self.backend, findings_path=second_findings)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000002")
        self.assertEqual(recovered["auditEvents"][0]["fromStatus"], "tampered")
        self.assertEqual(latest["previousVersionId"], "v000001")
        self.assertEqual(
            latest["previousManifestSignature"],
            trusted_v1["signature"]["value"],
        )
        self.assertIsNone(latest["userDecision"])
        self.assertNotIn(
            "forged-scanner",
            {scan["scanner"] for scan in latest["scans"]},
        )

    def test_no_verified_history_starts_clean_chain_root(self):
        """Recovery never links to the physically latest untrusted version."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        unsigned = self._read_manifest()
        unsigned["signature"] = None
        unsigned["userDecision"] = {
            "action": "always_allow",
            "reason": "forged allow",
        }
        self._write_manifest(unsigned)
        self._write_manifest(unsigned, "v000001")

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000002")
        self.assertIsNone(latest["previousVersionId"])
        self.assertIsNone(latest["previousManifestSignature"])
        self.assertIsNone(latest["userDecision"])
        audit_result = audit(self.skill_dir, self.backend)
        self.assertFalse(audit_result["valid"])
        self.assertFalse(
            any(error["versionId"] == "v000002" for error in audit_result["errors"])
        )

    def test_recovery_skips_untrusted_historical_candidates(self):
        """Only a complete, authentic, identity-bound artifact can become parent."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        for case in (
            "snapshot",
            "schema",
            "manifest_hash",
            "missing_signature",
            "signature",
            "skill_name",
            "version_id",
        ):
            with self.subTest(case=case):
                skill = Path(self.tmpdir, f"candidate-{case}")
                skill.mkdir()
                (skill / "run.sh").write_text("#!/bin/bash\necho v1\n")
                (skill / "SKILL.md").write_text(f"# {case}\n")
                certify(str(skill), self.backend, findings_path=findings_path)
                versions = skill / ".skill-meta" / "versions"
                trusted_v1 = json.loads((versions / "v000001.json").read_text())
                (skill / "run.sh").write_text("#!/bin/bash\necho v2\n")
                certify(str(skill), self.backend, findings_path=findings_path)
                v2_path = versions / "v000002.json"
                v2 = json.loads(v2_path.read_text())

                if case == "snapshot":
                    (versions / "v000002.snapshot" / "run.sh").write_text("tampered\n")
                elif case == "schema":
                    v2["fileHashes"] = ["invalid"]
                    v2_path.write_text(json.dumps(v2))
                elif case == "manifest_hash":
                    v2["scanStatus"] = "deny"
                    v2_path.write_text(json.dumps(v2))
                elif case == "missing_signature":
                    v2["signature"] = None
                    v2_path.write_text(json.dumps(v2))
                else:
                    if case == "skill_name":
                        v2["skillName"] = "other-skill"
                    elif case == "version_id":
                        v2["versionId"] = "v000999"
                    signer = (
                        InMemoryEd25519Backend()
                        if case == "signature"
                        else self.backend
                    )
                    v2_path.write_text(
                        json.dumps(self._resign_manifest_data(v2, backend=signer))
                    )

                recovered = certify(
                    str(skill), self.backend, findings_path=findings_path
                )
                latest = json.loads((skill / ".skill-meta" / "latest.json").read_text())

                self.assertEqual(recovered["versionId"], "v000003")
                self.assertEqual(latest["previousVersionId"], "v000001")
                self.assertEqual(
                    latest["previousManifestSignature"],
                    trusted_v1["signature"]["value"],
                )
                audit_result = audit(
                    str(skill),
                    self.backend,
                    verify_snapshots=case == "snapshot",
                )
                self.assertFalse(audit_result["valid"])
                self.assertFalse(
                    any(
                        error["versionId"] == "v000003"
                        for error in audit_result["errors"]
                    )
                )

    def test_latest_and_version_artifact_divergence_forces_new_version(self):
        """Two valid but different copies of one version are never overwritten in place."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        divergent_version = self._read_manifest("v000001")
        divergent_version["updatedAt"] = "signed-divergent-copy"
        divergent_version = self._resign_manifest_data(divergent_version)
        self._write_manifest(divergent_version, "v000001")

        recovered = certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()

        self.assertEqual(recovered["versionId"], "v000002")
        self.assertTrue(recovered["newVersion"])
        self.assertEqual(recovered["auditEvents"][0]["fromStatus"], "tampered")
        self.assertEqual(latest["previousVersionId"], "v000001")
        self.assertEqual(
            latest["previousManifestSignature"],
            divergent_version["signature"]["value"],
        )

    def test_history_io_failure_aborts_recovery(self):
        """Storage failures cannot be downgraded into a clean-chain recovery."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        tampered = self._read_manifest()
        tampered["scanStatus"] = "deny"
        self._write_manifest(tampered)

        with patch(
            "agent_sec_cli.skill_ledger.core.manifest_helpers.load_verified_version_manifest",
            side_effect=PermissionError("metadata temporarily unreadable"),
        ):
            with self.assertRaisesRegex(PermissionError, "temporarily unreadable"):
                certify(self.skill_dir, self.backend, findings_path=findings_path)

        self.assertFalse(os.path.exists(self._manifest_path("v000002")))

    def test_verification_key_failure_aborts_recovery(self):
        """An unavailable verifier cannot be replaced with a new trust root."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        with self.assertRaises(KeyNotFoundError):
            certify(
                self.skill_dir,
                KeyMissingVerifyBackend(self.backend),
                findings_path=findings_path,
            )

        self.assertFalse(os.path.exists(self._manifest_path("v000002")))

    def test_rollback_rejects_scan_root_mismatch_before_decision(self):
        """Rollback restores the original root when files change after persistence."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho risky\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        fresh_scan = ScanEntry(scanner="code-scanner", status="pass")

        def scan_then_change_root(skill_dir, backend, *, force=False):
            result = scan_skill(skill_dir, backend, force=force)
            Path(self.skill_dir, "run.sh").write_text("changed after scan\n")
            return result

        with patch(
            "agent_sec_cli.skill_ledger.core.certifier._auto_invoke_scanners",
            return_value=[fresh_scan],
        ), patch(
            "agent_sec_cli.skill_ledger.core.decision.scan_skill",
            side_effect=scan_then_change_root,
        ):
            with self.assertRaisesRegex(
                SkillLedgerError,
                "did not certify the restored skill root",
            ):
                rollback_skill(
                    self.skill_dir,
                    self.backend,
                    target_version_id="v000001",
                )

        self.assertIn("risky", Path(self.skill_dir, "run.sh").read_text())
        latest = self._read_manifest()
        self.assertEqual(latest["versionId"], "v000003")
        self.assertIsNone(latest["userDecision"])

    def test_scan_recovery_without_results_fails_instead_of_returning_candidate(self):
        """A noop scanner cannot expose an unpersisted recovery version."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        tampered = self._read_manifest()
        tampered["scanStatus"] = "deny"
        self._write_manifest(tampered)

        with patch(
            "agent_sec_cli.skill_ledger.core.certifier._auto_invoke_scanners",
            return_value=[],
        ):
            with self.assertRaisesRegex(
                SkillLedgerError,
                "scan cannot recover tampered",
            ):
                scan_skill(self.skill_dir, self.backend)

        self.assertEqual(self._read_manifest(), tampered)
        self.assertFalse(os.path.exists(self._manifest_path("v000002")))

    def test_decide_rejects_invalid_latest_snapshot(self):
        """A decision cannot re-sign a manifest with an invalid snapshot."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_latest = self._read_manifest()
        snapshot_file = Path(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000001.snapshot",
            "run.sh",
        )
        snapshot_file.write_text("tampered snapshot\n")

        with self.assertRaisesRegex(SkillLedgerError, "untrusted latest"):
            decide_skill(self.skill_dir, self.backend, action="allow")

        self.assertEqual(self._read_manifest(), trusted_latest)

    def test_clear_decision_rejects_replayed_latest(self):
        """Clear cannot overwrite history through an authenticated stale latest."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        v1 = self._read_manifest("v000001")
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        v2 = self._read_manifest("v000002")
        Path(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000002.snapshot",
            "run.sh",
        ).write_text("tampered snapshot\n")
        self._write_manifest(v1)

        with self.assertRaisesRegex(SkillLedgerError, "untrusted latest"):
            clear_decision(self.skill_dir, self.backend)

        self.assertEqual(self._read_manifest("v000001"), v1)
        self.assertEqual(self._read_manifest("v000002"), v2)

    def test_rollback_rejects_signed_target_identity_mismatch(self):
        """Rollback cannot restore a signed snapshot owned by another skill."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        trusted_latest = self._read_manifest()
        transplanted = self._read_manifest("v000001")
        transplanted["skillName"] = "other-skill"
        self._write_manifest(
            self._resign_manifest_data(transplanted),
            "v000001",
        )

        with self.assertRaisesRegex(SkillLedgerError, "untrusted version"):
            rollback_skill(
                self.skill_dir,
                self.backend,
                target_version_id="v000001",
            )

        self.assertEqual(self._read_manifest(), trusted_latest)
        self.assertIn("v2", Path(self.skill_dir, "run.sh").read_text())

    def test_export_revalidates_the_copied_snapshot(self):
        """A source change during copy cannot produce a successful mixed export."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "clean"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        output = Path(self.tmpdir, "exported")
        real_copytree = shutil.copytree

        def copy_then_tamper(src, dst, *args, **kwargs):
            copied = real_copytree(src, dst, *args, **kwargs)
            Path(copied, "run.sh").write_text("changed during export\n")
            return copied

        with patch(
            "agent_sec_cli.skill_ledger.core.decision.shutil.copytree",
            side_effect=copy_then_tamper,
        ):
            with self.assertRaisesRegex(SkillLedgerError, "exported snapshot"):
                export_skill(
                    self.skill_dir,
                    self.backend,
                    version="latest",
                    output=str(output),
                )

        self.assertFalse(output.exists())

    def test_certify_after_file_change_creates_new_version(self):
        """File change between certifies → new version created."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "clean"},
            ]
        )
        r1 = certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho modified\n")
        r2 = certify(self.skill_dir, self.backend, findings_path=findings_path)
        self.assertEqual(r1["versionId"], "v000001")
        self.assertEqual(r2["versionId"], "v000002")
        self.assertTrue(r2["newVersion"])

    def test_scan_entry_merge_replaces_same_scanner(self):
        """Re-certifying with the same scanner replaces the old entry, not appends."""
        findings_warn = self._write_findings(
            [
                {"rule": "r1", "level": "warn", "message": "first scan"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_warn)

        findings_pass = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "fixed"},
            ]
        )
        result = certify(self.skill_dir, self.backend, findings_path=findings_pass)
        self.assertEqual(result["scanStatus"], "pass")  # was warn, now pass

        # Verify only one scan entry in manifest (not two)
        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        with open(latest, "r") as f:
            data = json.load(f)
        self.assertEqual(len(data["scans"]), 1)

    def test_signed_ledger_reads_and_new_scan_preserve_version_files(self):
        scan_skill(self.skill_dir, self.backend)
        metadata = Path(self.skill_dir) / ".skill-meta"
        before = {
            path: hashlib.sha256(path.read_bytes()).hexdigest()
            for path in metadata.rglob("*")
            if path.is_file()
        }

        self.assertEqual(check(self.skill_dir, self.backend)["status"], "pass")
        show_skill(self.skill_dir, self.backend)
        self.assertTrue(
            audit(self.skill_dir, self.backend, verify_snapshots=True)["valid"]
        )
        self.assertEqual(
            {
                path: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in metadata.rglob("*")
                if path.is_file()
            },
            before,
        )

        self._write_file("run.sh", "echo new version\n")
        result = scan_skill(self.skill_dir, self.backend)
        self.assertEqual(result["versionId"], "v000002")
        self.assertEqual(
            {entry["scanner"] for entry in self._read_manifest()["scans"]},
            {"code-scanner", "static-scanner"},
        )
        first_version = Path(self._manifest_path("v000001"))
        self.assertEqual(
            hashlib.sha256(first_version.read_bytes()).hexdigest(),
            before[first_version],
        )
        self.assertTrue(
            audit(self.skill_dir, self.backend, verify_snapshots=True)["valid"]
        )

    def test_deny_finding_produces_deny_status(self):
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
                {"rule": "r2", "level": "deny", "message": "bad"},
            ]
        )
        result = certify(self.skill_dir, self.backend, findings_path=findings_path)
        self.assertEqual(result["scanStatus"], "deny")

    def test_scan_mode_no_crash(self):
        """Scan runs default built-in scanners."""
        result = scan_skill(self.skill_dir, self.backend)
        self.assertIn("versionId", result)
        self.assertEqual(result["scanStatus"], "pass")

        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        with open(latest, "r") as f:
            data = json.load(f)
        scans = {scan["scanner"]: scan for scan in data["scans"]}
        self.assertIn("code-scanner", scans)
        self.assertIn("static-scanner", scans)
        self.assertEqual(scans["code-scanner"]["status"], "pass")
        self.assertEqual(scans["static-scanner"]["status"], "pass")

    def test_builtin_scanner_failure_is_reported_without_manifest_update(self):
        with patch(
            "agent_sec_cli.skill_ledger.scanner.builtins.dispatcher.scan_skill",
            side_effect=ValueError("invalid bundled rules"),
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "static-scanner.*invalid bundled rules",
            ):
                scan_skill(self.skill_dir, self.backend)

        latest = os.path.join(self.skill_dir, ".skill-meta", "latest.json")
        self.assertFalse(os.path.exists(latest))


# ---------------------------------------------------------------------------
# Audit chain verification
# ---------------------------------------------------------------------------


class TestAuditChainIntegrity(SkillDirTestCase):
    """Tests for the audit command — version chain integrity verification."""

    def _create_three_version_chain(self) -> None:
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v3\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)

    def test_valid_single_version_passes(self):
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        result = audit(self.skill_dir, self.backend)
        self.assertTrue(result["valid"])
        self.assertEqual(result["versions_checked"], 1)

    def test_valid_multi_version_chain(self):
        """Two certifies with file change → two versions, chain should be valid."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        result = audit(self.skill_dir, self.backend)
        self.assertTrue(result["valid"])
        self.assertEqual(result["versions_checked"], 2)

    def test_tampered_hash_detected(self):
        """Modifying a version manifest's content → audit detects hash mismatch."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        # Tamper with the version file
        v_file = os.path.join(self.skill_dir, ".skill-meta", "versions", "v000001.json")
        with open(v_file, "r") as f:
            data = json.load(f)
        data["scanStatus"] = (
            "deny"  # tamper: was "pass", now "deny" — without re-hashing
        )
        with open(v_file, "w") as f:
            json.dump(data, f)
        result = audit(self.skill_dir, self.backend)
        self.assertFalse(result["valid"])
        error_msgs = [e["error"] for e in result["errors"]]
        self.assertTrue(any("manifestHash" in msg for msg in error_msgs))

    def test_tampered_latest_manifest_invalidates_audit(self):
        """Audit authenticates latest.json instead of checking only its version id."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        latest = self._read_manifest()
        latest["scanStatus"] = "deny"
        self._write_manifest(latest)

        result = audit(self.skill_dir, self.backend)

        self.assertFalse(result["valid"])
        self.assertTrue(
            any(
                error["versionId"] == "latest.json"
                and "authenticity" in error["error"].lower()
                for error in result["errors"]
            )
        )

    def test_corrupted_version_manifest_does_not_abort_audit(self):
        """Malformed version JSON is reported while later versions are still audited."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        v1_file = os.path.join(
            self.skill_dir,
            ".skill-meta",
            "versions",
            "v000001.json",
        )
        with open(v1_file, "w") as f:
            f.write("{not-json")

        result = audit(self.skill_dir, self.backend)

        self.assertFalse(result["valid"])
        self.assertEqual(result["versions_checked"], 2)
        errors = result["errors"]
        self.assertTrue(
            any(
                error["versionId"] == "v000001" and "corrupted" in error["error"]
                for error in errors
            )
        )
        self.assertTrue(
            any(
                error["versionId"] == "v000002"
                and "prior version manifest" in error["error"]
                for error in errors
            )
        )

    def test_broken_chain_detected(self):
        """Corrupting previousManifestSignature → audit detects chain break."""
        findings_path = self._write_findings(
            [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        self._write_file("run.sh", "#!/bin/bash\necho v2\n")
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        # Tamper with v000002's previousManifestSignature
        v2_file = os.path.join(
            self.skill_dir, ".skill-meta", "versions", "v000002.json"
        )
        with open(v2_file, "r") as f:
            data = json.load(f)
        data["previousManifestSignature"] = "BROKEN"
        # Re-hash and re-sign to avoid hash mismatch detection (test chain specifically)
        m = SignedManifest.from_json(json.dumps(data))
        m.manifestHash = m.compute_manifest_hash()
        sig_val, fp = self.backend.sign(m.manifestHash.encode("utf-8"))
        data["manifestHash"] = m.manifestHash
        data["signature"]["value"] = sig_val
        data["signature"]["keyFingerprint"] = fp
        with open(v2_file, "w") as f:
            json.dump(data, f)

        result = audit(self.skill_dir, self.backend)
        self.assertFalse(result["valid"])
        error_msgs = [e["error"] for e in result["errors"]]
        self.assertTrue(any("chain broken" in msg for msg in error_msgs))

    def test_chain_rejects_invalid_parent_references(self):
        """Parent fields reject half, unknown, self, and forward references."""
        self._create_three_version_chain()
        original_v2 = self._read_manifest("v000002")
        original_v3 = self._read_manifest("v000003")
        cases = (
            ("half", "v000002", "v000001", None, "both"),
            ("unknown", "v000002", "v999999", "unknown", "parent"),
            (
                "self",
                "v000003",
                "v000003",
                original_v3["signature"]["value"],
                "earlier",
            ),
            (
                "forward",
                "v000002",
                "v000003",
                original_v3["signature"]["value"],
                "earlier",
            ),
        )
        for name, child_id, parent_id, parent_signature, expected in cases:
            with self.subTest(case=name):
                self._write_manifest(original_v2, "v000002")
                self._write_manifest(original_v3, "v000003")
                self._write_manifest(original_v3)
                child = json.loads(
                    json.dumps(original_v2 if child_id == "v000002" else original_v3)
                )
                child["previousVersionId"] = parent_id
                child["previousManifestSignature"] = parent_signature
                child = self._resign_manifest_data(child)
                self._write_manifest(child, child_id)
                if child_id == "v000003":
                    self._write_manifest(child)

                result = audit(self.skill_dir, self.backend)

                self.assertFalse(result["valid"])
                self.assertTrue(
                    any(
                        error["versionId"] == child_id
                        and expected in error["error"].lower()
                        for error in result["errors"]
                    ),
                    result["errors"],
                )

    def test_no_versions_returns_valid(self):
        """Empty .skill-meta (no versions) → audit succeeds with 0 checked."""
        os.makedirs(
            os.path.join(self.skill_dir, ".skill-meta", "versions"), exist_ok=True
        )
        result = audit(self.skill_dir, self.backend)
        self.assertTrue(result["valid"])
        self.assertEqual(result["versions_checked"], 0)

    def test_latest_without_version_artifact_is_invalid(self):
        """A signed latest.json cannot stand in for its missing history artifact."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        os.remove(self._manifest_path("v000001"))

        result = audit(self.skill_dir, self.backend)

        self.assertFalse(result["valid"])
        self.assertEqual(result["versions_checked"], 1)
        self.assertTrue(
            any(
                error["versionId"] == "v000001" and "missing" in error["error"]
                for error in result["errors"]
            )
        )

    def test_versions_without_latest_manifest_are_invalid(self):
        """A populated history must have a current latest.json pointer."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)
        os.remove(self._manifest_path())

        result = audit(self.skill_dir, self.backend)

        self.assertFalse(result["valid"])
        self.assertTrue(
            any(
                error["versionId"] == "latest.json" and "missing" in error["error"]
                for error in result["errors"]
            )
        )

    def test_signature_verify_false_is_invalid(self):
        """A backend returning False from verify is reported as invalid."""
        findings_path = self._write_findings(
            [{"rule": "r1", "level": "pass", "message": "ok"}]
        )
        certify(self.skill_dir, self.backend, findings_path=findings_path)

        result = audit(self.skill_dir, VerifyFalseBackend(self.backend))

        self.assertFalse(result["valid"])
        error_msgs = [e["error"] for e in result["errors"]]
        self.assertTrue(
            any("signature verification returned false" in msg for msg in error_msgs)
        )


# ---------------------------------------------------------------------------
# File hash diff
# ---------------------------------------------------------------------------


class TestFileHashDiff(SkillDirTestCase):
    """Tests for file integrity diff — drives drifted/unchanged decisions."""

    def test_identical_hashes_match(self):
        hashes = compute_file_hashes(self.skill_dir)
        diff = diff_file_hashes(hashes, hashes)
        self.assertTrue(diff["match"])
        self.assertEqual(diff["added"], [])
        self.assertEqual(diff["removed"], [])
        self.assertEqual(diff["modified"], [])

    def test_added_file_detected(self):
        old = compute_file_hashes(self.skill_dir)
        self._write_file("new.py", "print('hi')\n")
        new = compute_file_hashes(self.skill_dir)
        diff = diff_file_hashes(old, new)
        self.assertFalse(diff["match"])
        self.assertIn("new.py", diff["added"])

    def test_removed_file_detected(self):
        old = compute_file_hashes(self.skill_dir)
        os.remove(os.path.join(self.skill_dir, "run.sh"))
        new = compute_file_hashes(self.skill_dir)
        diff = diff_file_hashes(old, new)
        self.assertFalse(diff["match"])
        self.assertIn("run.sh", diff["removed"])

    def test_modified_file_detected(self):
        old = compute_file_hashes(self.skill_dir)
        self._write_file("run.sh", "#!/bin/bash\necho CHANGED\n")
        new = compute_file_hashes(self.skill_dir)
        diff = diff_file_hashes(old, new)
        self.assertFalse(diff["match"])
        self.assertIn("run.sh", diff["modified"])

    def test_skill_meta_excluded(self):
        """The .skill-meta directory must be excluded from hashing."""
        os.makedirs(os.path.join(self.skill_dir, ".skill-meta"), exist_ok=True)
        with open(os.path.join(self.skill_dir, ".skill-meta", "latest.json"), "w") as f:
            f.write("{}")
        hashes = compute_file_hashes(self.skill_dir)
        self.assertNotIn(".skill-meta/latest.json", hashes)

    def test_git_dir_excluded(self):
        """The .git directory must be excluded from hashing."""
        os.makedirs(os.path.join(self.skill_dir, ".git"), exist_ok=True)
        with open(os.path.join(self.skill_dir, ".git", "config"), "w") as f:
            f.write("[core]")
        hashes = compute_file_hashes(self.skill_dir)
        self.assertNotIn(".git/config", hashes)


if __name__ == "__main__":
    unittest.main()
