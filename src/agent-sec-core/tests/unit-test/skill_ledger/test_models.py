"""Unit tests for skill_ledger models — manifest hashing, scan aggregation, finding normalization.

These tests protect the most critical invariants:
1. Manifest hash determinism — if canonical JSON changes, ALL existing signatures break.
2. Severity aggregation ordering — deny > warn > pass > none is a core security contract.
3. Finding serialization — optional fields only appear when set (compact manifest).
"""

import unittest

from agent_sec_cli.skill_ledger.models.finding import NormalizedFinding
from agent_sec_cli.skill_ledger.models.manifest import (
    ManifestSignature,
    SignedManifest,
    UserDecision,
)
from agent_sec_cli.skill_ledger.models.scan import (
    ScanEntry,
    aggregate_scan_status,
)


class TestManifestHashDeterminism(unittest.TestCase):
    """The same logical manifest must ALWAYS produce the same hash.

    If this breaks, every existing signed manifest in production becomes
    unverifiable — ``check`` will return ``tampered`` on valid data.
    """

    def _make_manifest(self, **overrides) -> SignedManifest:
        defaults = dict(
            versionId="v000001",
            skillName="test-skill",
            fileHashes={"run.sh": "sha256:aaa", "SKILL.md": "sha256:bbb"},
            scanStatus="pass",
            createdAt="2026-01-01T00:00:00+00:00",
            updatedAt="2026-01-01T00:00:00+00:00",
        )
        defaults.update(overrides)
        return SignedManifest(**defaults)

    def test_same_manifest_same_hash(self):
        """Identical manifests must produce identical hashes — non-negotiable."""
        m1 = self._make_manifest()
        m2 = self._make_manifest()
        self.assertEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_hash_is_sha256_prefixed(self):
        h = self._make_manifest().compute_manifest_hash()
        self.assertTrue(h.startswith("sha256:"))
        self.assertEqual(len(h), len("sha256:") + 64)  # hex SHA-256

    def test_hash_excludes_manifestHash_and_signature(self):
        """Changing manifestHash or signature must NOT change the computed hash.

        These fields are the output of the hashing process, not inputs.
        """
        m1 = self._make_manifest()
        m2 = self._make_manifest()
        m2.manifestHash = "sha256:fake"
        m2.signature = ManifestSignature(
            algorithm="ed25519", value="sig123", keyFingerprint="sha256:fp"
        )
        self.assertEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_hash_changes_on_fileHashes_change(self):
        """Any file change must produce a different hash — core integrity guarantee."""
        m1 = self._make_manifest()
        m2 = self._make_manifest(fileHashes={"run.sh": "sha256:CHANGED"})
        self.assertNotEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_hash_changes_on_scanStatus_change(self):
        """Tampering scanStatus (e.g. deny→pass) must change the hash."""
        m1 = self._make_manifest(scanStatus="deny")
        m2 = self._make_manifest(scanStatus="pass")
        self.assertNotEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_hash_changes_on_versionId_change(self):
        m1 = self._make_manifest(versionId="v000001")
        m2 = self._make_manifest(versionId="v000002")
        self.assertNotEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_hash_changes_on_scans_change(self):
        """Adding a scan entry must change the hash — scan results are signed."""
        m1 = self._make_manifest()
        m2 = self._make_manifest()
        m2.scans = [ScanEntry(scanner="test", version="1.0", status="pass")]
        self.assertNotEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())


class TestManifestSerializationRoundtrip(unittest.TestCase):
    """Serialization/deserialization must be lossless — manifests are stored as JSON."""

    def test_roundtrip_preserves_all_fields(self):
        original = SignedManifest(
            versionId="v000003",
            previousVersionId="v000002",
            skillName="my-skill",
            fileHashes={"a.sh": "sha256:aaa"},
            scans=[
                ScanEntry(
                    scanner="sv",
                    version="1",
                    status="warn",
                    findings=[{"rule": "r1", "level": "warn", "message": "m"}],
                )
            ],
            scanStatus="warn",
            policy="warning",
            createdAt="2026-01-01T00:00:00+00:00",
            updatedAt="2026-01-01T00:05:00+00:00",
            manifestHash="sha256:hash123",
            previousManifestSignature="prevsig",
            signature=ManifestSignature(
                algorithm="ed25519", value="sig", keyFingerprint="sha256:fp"
            ),
        )
        text = original.to_json()
        restored = SignedManifest.from_json(text)
        # Hash must survive roundtrip — otherwise check breaks on loaded manifests
        self.assertEqual(
            original.compute_manifest_hash(), restored.compute_manifest_hash()
        )
        self.assertEqual(original.versionId, restored.versionId)
        self.assertEqual(original.previousVersionId, restored.previousVersionId)
        self.assertEqual(original.fileHashes, restored.fileHashes)
        self.assertEqual(original.scanStatus, restored.scanStatus)
        self.assertEqual(original.updatedAt, restored.updatedAt)
        self.assertEqual(original.signature.value, restored.signature.value)
        self.assertEqual(len(original.scans), len(restored.scans))
        self.assertEqual(original.scans[0].scanner, restored.scans[0].scanner)

    def test_user_decision_roundtrip_preserves_minimal_decision(self):
        original = SignedManifest(
            versionId="v000004",
            skillName="my-skill",
            fileHashes={"SKILL.md": "sha256:aaa"},
            scanStatus="deny",
            userDecision=UserDecision(
                action="rollback",
                targetVersionId="v000001",
                reason="reviewed and restored previous pass version",
            ),
            createdAt="2026-01-01T00:00:00+00:00",
            updatedAt="2026-01-01T00:05:00+00:00",
        )

        restored = SignedManifest.from_json(original.to_json())

        self.assertEqual(restored.userDecision.action, "rollback")
        self.assertEqual(restored.userDecision.targetVersionId, "v000001")
        self.assertEqual(
            restored.userDecision.reason,
            "reviewed and restored previous pass version",
        )

    def test_user_decision_changes_manifest_hash(self):
        m1 = SignedManifest(
            versionId="v000001",
            skillName="my-skill",
            fileHashes={"SKILL.md": "sha256:aaa"},
            scanStatus="deny",
            createdAt="2026-01-01T00:00:00+00:00",
            updatedAt="2026-01-01T00:00:00+00:00",
        )
        m2 = SignedManifest(
            versionId="v000001",
            skillName="my-skill",
            fileHashes={"SKILL.md": "sha256:aaa"},
            scanStatus="deny",
            userDecision=UserDecision(action="allow"),
            createdAt="2026-01-01T00:00:00+00:00",
            updatedAt="2026-01-01T00:00:00+00:00",
        )

        self.assertNotEqual(m1.compute_manifest_hash(), m2.compute_manifest_hash())

    def test_rollback_decision_requires_target_version(self):
        with self.assertRaises(ValueError):
            UserDecision(action="rollback")


class TestScanStatusAggregation(unittest.TestCase):
    """Severity ordering: deny > warn > pass > none.

    This is the security contract: the most severe scanner result
    determines the overall skill status shown to the user.
    """

    def test_empty_scans_returns_none(self):
        self.assertEqual(aggregate_scan_status([]), "none")

    def test_single_pass(self):
        scans = [ScanEntry(status="pass")]
        self.assertEqual(aggregate_scan_status(scans), "pass")

    def test_deny_dominates_all(self):
        """Even one deny scanner overrides any number of pass/warn results."""
        scans = [
            ScanEntry(scanner="a", status="pass"),
            ScanEntry(scanner="b", status="warn"),
            ScanEntry(scanner="c", status="deny"),
        ]
        self.assertEqual(aggregate_scan_status(scans), "deny")

    def test_warn_dominates_pass(self):
        scans = [
            ScanEntry(scanner="a", status="pass"),
            ScanEntry(scanner="b", status="warn"),
        ]
        self.assertEqual(aggregate_scan_status(scans), "warn")

    def test_unknown_status_treated_as_lowest(self):
        """Unknown status defaults to severity 0 — should not override known statuses."""
        scans = [
            ScanEntry(scanner="a", status="pass"),
            ScanEntry(scanner="b", status="unknown_future_status"),
        ]
        result = aggregate_scan_status(scans)
        self.assertEqual(result, "pass")


class TestNormalizedFindingToDict(unittest.TestCase):
    """to_findings_dict() must only include optional fields when set.

    This keeps manifests compact and avoids false diffs when comparing versions.
    """

    def test_minimal_finding_omits_optional_fields(self):
        f = NormalizedFinding(rule="test-rule", level="warn", message="msg")
        d = f.to_findings_dict()
        self.assertEqual(d, {"rule": "test-rule", "level": "warn", "message": "msg"})
        self.assertNotIn("file", d)
        self.assertNotIn("line", d)
        self.assertNotIn("metadata", d)

    def test_full_finding_includes_all_fields(self):
        f = NormalizedFinding(
            rule="dangerous-exec",
            level="deny",
            message="exec found",
            file="run.sh",
            line=42,
            metadata={"pattern": "child_process"},
        )
        d = f.to_findings_dict()
        self.assertEqual(d["file"], "run.sh")
        self.assertEqual(d["line"], 42)
        self.assertEqual(d["metadata"]["pattern"], "child_process")


if __name__ == "__main__":
    unittest.main()
