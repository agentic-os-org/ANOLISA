#!/usr/bin/env python3
"""Integration tests for skill verifier"""

import errno
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# Add agent-sec-cli/src to path so the full package is importable
sys.path.insert(
    0,
    os.path.abspath(
        os.path.join(
            os.path.dirname(__file__), "..", "..", "..", "agent-sec-cli", "src"
        )
    ),
)

from agent_sec_cli.asset_verify.errors import (
    ErrConfigInvalid,
    ErrConfigMissing,
    ErrHashMismatch,
    ErrManifestMissing,
    ErrNoTrustedKeys,
    ErrSigMissing,
    ErrUnexpectedFile,
)
from agent_sec_cli.asset_verify.verifier import (
    compute_file_hash,
    load_config,
    load_trusted_keys,
    verify_manifest_hashes,
    verify_skill,
    verify_skills_dir,
)


class TestComputeFileHash(unittest.TestCase):
    def test_hash_computation(self):
        with tempfile.NamedTemporaryFile(mode="w", delete=False) as f:
            f.write("test content")
            f.flush()
            path = f.name

        try:
            h = compute_file_hash(path)
            self.assertEqual(len(h), 64)  # SHA256 hex length
            # Known hash for "test content"
            self.assertEqual(
                h, "6ae8a75555209fd6c44157c0aed8016e763ff435a19cf186f76863140143ff72"
            )
        finally:
            os.unlink(path)


class TestVerifyManifestHashes(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        # Create test file
        self.test_file = os.path.join(self.tmpdir, "main.py")
        with open(self.test_file, "w") as f:
            f.write("print('hello')")
        self.file_hash = compute_file_hash(self.test_file)

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_valid_hashes(self):
        manifest = {"files": [{"path": "main.py", "hash": self.file_hash}]}
        # Should not raise
        verify_manifest_hashes(self.tmpdir, manifest, "test_skill")

    def test_hash_mismatch(self):
        manifest = {"files": [{"path": "main.py", "hash": "wrong_hash"}]}
        with self.assertRaises(ErrHashMismatch):
            verify_manifest_hashes(self.tmpdir, manifest, "test_skill")

    def test_missing_file(self):
        manifest = {"files": [{"path": "nonexistent.py", "hash": "somehash"}]}
        with self.assertRaises(ErrHashMismatch) as ctx:
            verify_manifest_hashes(self.tmpdir, manifest, "test_skill")
        self.assertIn("FILE_MISSING", str(ctx.exception))

    def test_unexpected_file(self):
        extra_file = os.path.join(self.tmpdir, "references", "a.md")
        os.makedirs(os.path.dirname(extra_file), exist_ok=True)
        with open(extra_file, "w") as f:
            f.write("")

        manifest = {"files": [{"path": "main.py", "hash": self.file_hash}]}
        with self.assertRaises(ErrUnexpectedFile) as ctx:
            verify_manifest_hashes(self.tmpdir, manifest, "test_skill")
        self.assertIn("references/a.md", str(ctx.exception))

    def test_unexpected_root_file(self):
        extra_file = os.path.join(self.tmpdir, "a.md")
        with open(extra_file, "w") as f:
            f.write("")

        manifest = {"files": [{"path": "main.py", "hash": self.file_hash}]}
        with self.assertRaises(ErrUnexpectedFile) as ctx:
            verify_manifest_hashes(self.tmpdir, manifest, "test_skill")
        self.assertIn("a.md", str(ctx.exception))

    def test_hidden_files_are_ignored(self):
        hidden_file = os.path.join(self.tmpdir, ".hidden.md")
        hidden_dir_file = os.path.join(self.tmpdir, ".skill-meta", "Manifest.json")
        os.makedirs(os.path.dirname(hidden_dir_file), exist_ok=True)
        with open(hidden_file, "w") as f:
            f.write("ignored")
        with open(hidden_dir_file, "w") as f:
            f.write("ignored")

        manifest = {"files": [{"path": "main.py", "hash": self.file_hash}]}
        verify_manifest_hashes(self.tmpdir, manifest, "test_skill")


class TestVerifySkill(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.skill_dir = os.path.join(self.tmpdir, "test_skill")
        os.makedirs(self.skill_dir)

        # Create test file
        test_file = os.path.join(self.skill_dir, "main.py")
        with open(test_file, "w") as f:
            f.write("print('hello')")

        self.file_hash = compute_file_hash(test_file)

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_missing_manifest(self):
        # Create sig inside .skill-meta but no manifest
        meta_dir = os.path.join(self.skill_dir, ".skill-meta")
        os.makedirs(meta_dir, exist_ok=True)
        sig_path = os.path.join(meta_dir, ".skill.sig")
        with open(sig_path, "w") as f:
            f.write("fake sig")

        with self.assertRaises(ErrManifestMissing):
            verify_skill(self.skill_dir, [])

    def test_missing_sig(self):
        # Create manifest inside .skill-meta but no sig
        manifest = {
            "version": "0.1",
            "skill_name": "test_skill",
            "algorithm": "SHA256",
            "files": [{"path": "main.py", "hash": self.file_hash}],
        }
        meta_dir = os.path.join(self.skill_dir, ".skill-meta")
        os.makedirs(meta_dir, exist_ok=True)
        manifest_path = os.path.join(meta_dir, "Manifest.json")
        with open(manifest_path, "w") as f:
            json.dump(manifest, f)

        with self.assertRaises(ErrSigMissing):
            verify_skill(self.skill_dir, [])


class TestVerifySkillsDir(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_nonexistent_dir(self):
        missing_dir = os.path.join(self.tmpdir, "missing_skills")
        results = verify_skills_dir(missing_dir, [])
        self.assertEqual(results["checked"], 0)
        self.assertEqual(results["passed"], [])
        self.assertEqual(results["failed"], [])

    def test_empty_dir(self):
        empty_dir = os.path.join(self.tmpdir, "empty_skills")
        os.makedirs(empty_dir)
        results = verify_skills_dir(empty_dir, [])
        self.assertEqual(results["checked"], 0)
        self.assertEqual(results["passed"], [])
        self.assertEqual(results["failed"], [])

    def test_root_files_and_hidden_directories_are_not_candidates(self):
        root = os.path.join(self.tmpdir, "skills")
        os.makedirs(os.path.join(root, ".hidden-skill"))
        with open(os.path.join(root, "README.md"), "w") as f:
            f.write("not a skill directory")

        results = verify_skills_dir(root, [])

        self.assertEqual(results["checked"], 0)
        self.assertEqual(results["passed"], [])
        self.assertEqual(results["failed"], [])

    def test_existing_non_directory_is_an_error(self):
        file_path = os.path.join(self.tmpdir, "not-a-directory")
        with open(file_path, "w") as f:
            f.write("x")

        with self.assertRaises(NotADirectoryError):
            verify_skills_dir(file_path, [])

    def test_unreadable_root_is_an_error(self):
        root = Path(self.tmpdir) / "skills"
        root.mkdir()

        with patch.object(Path, "iterdir", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(PermissionError, "denied"):
                verify_skills_dir(str(root), [])

    def test_visible_entry_stat_io_error_is_an_operation_error(self):
        root = Path(self.tmpdir) / "skills"
        (root / "candidate").mkdir(parents=True)

        with patch.object(
            Path,
            "stat",
            side_effect=OSError(errno.EIO, "Input/output error"),
        ):
            with self.assertRaisesRegex(OSError, "Input/output error"):
                verify_skills_dir(str(root), [])

    @patch(
        "agent_sec_cli.asset_verify.verifier.verify_skill",
        side_effect=ErrManifestMissing("bad-skill"),
    )
    def test_candidate_failure_counts_as_checked(self, _mock_verify_skill):
        root = os.path.join(self.tmpdir, "skills")
        os.makedirs(os.path.join(root, "bad-skill"))

        results = verify_skills_dir(root, [])

        self.assertEqual(results["checked"], 1)
        self.assertEqual(results["passed"], [])
        self.assertEqual(len(results["failed"]), 1)


class TestLoadConfig(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_missing_config(self):
        missing_config = Path(self.tmpdir) / "missing.conf"
        with self.assertRaises(ErrConfigMissing):
            load_config(missing_config)

    def test_single_skills_dir(self):
        config_path = os.path.join(self.tmpdir, "config.conf")
        with open(config_path, "w") as f:
            f.write("skills_dir = /opt/skills\n")

        config = load_config(Path(config_path))
        self.assertEqual(config["skills_dirs"], ["/opt/skills"])

    def test_list_skills_dir(self):
        config_path = os.path.join(self.tmpdir, "config.conf")
        with open(config_path, "w") as f:
            f.write("skills_dir = [\n")
            f.write("    /opt/skills1\n")
            f.write("    /opt/skills2\n")
            f.write("]\n")

        config = load_config(Path(config_path))
        self.assertEqual(config["skills_dirs"], ["/opt/skills1", "/opt/skills2"])

    def test_empty_list_is_valid(self):
        config_path = Path(self.tmpdir) / "config.conf"
        with open(config_path, "w") as f:
            f.write("skills_dir = []\n")

        config = load_config(config_path)

        self.assertEqual(config["skills_dirs"], [])

    def test_unterminated_list_is_invalid(self):
        config_path = Path(self.tmpdir) / "config.conf"
        with open(config_path, "w") as f:
            f.write("skills_dir = [\n")
            f.write("    /opt/skills\n")

        with self.assertRaises(ErrConfigInvalid):
            load_config(config_path)

    def test_malformed_entry_is_invalid(self):
        config_path = Path(self.tmpdir) / "config.conf"
        with open(config_path, "w") as f:
            f.write("skills_dir /opt/skills\n")

        with self.assertRaises(ErrConfigInvalid):
            load_config(config_path)

    def test_unknown_key_is_invalid(self):
        config_path = Path(self.tmpdir) / "config.conf"
        with open(config_path, "w") as f:
            f.write("skill_dirs = /opt/skills\n")

        with self.assertRaisesRegex(
            ErrConfigInvalid, "unknown config key 'skill_dirs' on line 1"
        ):
            load_config(config_path)

    def test_unsupported_inline_list_is_invalid(self):
        config_path = Path(self.tmpdir) / "config.conf"
        with open(config_path, "w") as f:
            f.write("skills_dir = [/opt/skills, /usr/local/skills]\n")

        with self.assertRaises(ErrConfigInvalid):
            load_config(config_path)


class TestLoadTrustedKeys(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_nonexistent_dir(self):
        missing_dir = Path(self.tmpdir) / "missing_keys"
        with self.assertRaises(ErrNoTrustedKeys):
            load_trusted_keys(missing_dir)

    def test_empty_keys_dir(self):
        with self.assertRaises(ErrNoTrustedKeys):
            load_trusted_keys(Path(self.tmpdir))


class TestIntegrationWithGPG(unittest.TestCase):
    """Integration test using system gpg"""

    @classmethod
    def setUpClass(cls):
        cls.gpg_available = shutil.which("gpg") is not None
        if not cls.gpg_available:
            return

        cls.tmpdir = tempfile.mkdtemp()
        cls.keys_dir = os.path.join(cls.tmpdir, "keys")
        cls.skills_dir = os.path.join(cls.tmpdir, "skills")
        cls.skill_dir = os.path.join(cls.skills_dir, "test_skill")
        os.makedirs(cls.keys_dir)
        os.makedirs(cls.skill_dir)

        # Generate test key pair
        cls.gpg_home = os.path.join(cls.tmpdir, "gnupg")
        os.makedirs(cls.gpg_home, mode=0o700)

        key_params = """
Key-Type: RSA
Key-Length: 2048
Name-Real: Test Key
Name-Email: test@test.com
Expire-Date: 0
%no-protection
%commit
"""
        subprocess.run(
            ["gpg", "--homedir", cls.gpg_home, "--batch", "--gen-key"],
            input=key_params.encode(),
            capture_output=True,
            check=False,
        )

        # Export public key
        pub_key_path = os.path.join(cls.keys_dir, "test.asc")
        with open(pub_key_path, "w") as f:
            subprocess.run(
                [
                    "gpg",
                    "--homedir",
                    cls.gpg_home,
                    "--armor",
                    "--export",
                    "test@test.com",
                ],
                stdout=f,
                check=False,
            )

        # Create test skill files
        main_py = os.path.join(cls.skill_dir, "main.py")
        with open(main_py, "w") as f:
            f.write("print('hello')")

        # Create .skill-meta directory and manifest
        cls.meta_dir = os.path.join(cls.skill_dir, ".skill-meta")
        os.makedirs(cls.meta_dir)

        manifest = {
            "version": "0.1",
            "skill_name": "test_skill",
            "algorithm": "SHA256",
            "files": [{"path": "main.py", "hash": compute_file_hash(main_py)}],
        }
        cls.manifest_path = os.path.join(cls.meta_dir, "Manifest.json")
        with open(cls.manifest_path, "w") as f:
            json.dump(manifest, f)

        # Sign manifest
        cls.sig_path = os.path.join(cls.meta_dir, ".skill.sig")
        subprocess.run(
            [
                "gpg",
                "--homedir",
                cls.gpg_home,
                "--armor",
                "--detach-sign",
                "--output",
                cls.sig_path,
                cls.manifest_path,
            ],
            capture_output=True,
            check=False,
        )

    @classmethod
    def tearDownClass(cls):
        if hasattr(cls, "tmpdir") and os.path.exists(cls.tmpdir):
            shutil.rmtree(cls.tmpdir)

    def test_full_verification(self):
        if not self.gpg_available:
            self.skipTest("gpg not available")

        keys = load_trusted_keys(Path(self.keys_dir))
        self.assertTrue(len(keys) > 0)

        success, name = verify_skill(self.skill_dir, keys)
        self.assertTrue(success)
        self.assertEqual(name, "test_skill")

    def test_batch_verification(self):
        if not self.gpg_available:
            self.skipTest("gpg not available")

        keys = load_trusted_keys(Path(self.keys_dir))

        results = verify_skills_dir(self.skills_dir, keys)
        self.assertEqual(results["passed"], ["test_skill"])
        self.assertEqual(results["failed"], [])


if __name__ == "__main__":
    unittest.main()
