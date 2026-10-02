#!/usr/bin/env python3
"""Skill integrity verifier - Manifest + PGP signature verification."""

import argparse
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Literal, NotRequired, TypedDict

try:
    import pgpy
except ImportError:
    pgpy = None

from agent_sec_cli.asset_verify.errors import (
    ErrConfigInvalid,
    ErrConfigMissing,
    ErrHashMismatch,
    ErrManifestMissing,
    ErrNoTrustedKeys,
    ErrSigInvalid,
    ErrSigMissing,
    ErrUnexpectedFile,
)

SCRIPT_DIR = Path(__file__).parent.resolve()
DEFAULT_CONFIG = SCRIPT_DIR / "config.conf"
DEFAULT_TRUSTED_KEYS_DIR = SCRIPT_DIR / "trusted-keys"

# Check if system gpg is available (prefer 'gpg', fall back to 'gpg2' on RHEL/Alinux)
GPG_BIN = shutil.which("gpg") or shutil.which("gpg2")

# Hidden directory inside each skill that holds signing artifacts
SIGNING_DIR = ".skill-meta"


VerificationOutcome = Literal["verified", "failed", "no_candidates"]


class VerificationConfig(TypedDict):
    """Parsed asset-verification configuration."""

    skills_dirs: list[str]
    trusted_keys_dir: NotRequired[str]


class VerificationFailure(TypedDict):
    """One skill that failed verification."""

    name: str
    error: str


class SkillsDirectoryResult(TypedDict):
    """Verification result for one configured skills root."""

    checked: int
    passed: list[str]
    failed: list[VerificationFailure]


class VerificationResult(TypedDict):
    """Structured verification result shared by all entry points."""

    outcome: VerificationOutcome
    checked: int
    passed: list[str]
    failed: list[VerificationFailure]


def load_config(config_path: Path) -> VerificationConfig:
    """Load verification config file."""
    if not config_path.exists():
        raise ErrConfigMissing(str(config_path))

    config: VerificationConfig = {"skills_dirs": []}
    in_list = False

    with open(config_path, "r", encoding="utf-8") as f:
        for line_number, line in enumerate(f, start=1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue

            if in_list:
                if line == "]":
                    in_list = False
                else:
                    value = line.rstrip(",").strip()
                    if not value:
                        raise ErrConfigInvalid(
                            str(config_path), "empty skills_dir entry"
                        )
                    config["skills_dirs"].append(value)
            elif "=" in line:
                key, val = line.split("=", 1)
                key, val = key.strip(), val.strip()
                if key == "skills_dir":
                    if val == "[":
                        in_list = True
                    elif val == "[]":
                        continue
                    elif not val:
                        raise ErrConfigInvalid(
                            str(config_path), "empty skills_dir value"
                        )
                    elif val.startswith("[") or val.endswith("]"):
                        raise ErrConfigInvalid(
                            str(config_path),
                            f"unsupported skills_dir list syntax on line {line_number}",
                        )
                    else:
                        config["skills_dirs"].append(val)
                elif key == "trusted_keys_dir":
                    if not val:
                        raise ErrConfigInvalid(
                            str(config_path), "empty trusted_keys_dir value"
                        )
                    config["trusted_keys_dir"] = val
                else:
                    raise ErrConfigInvalid(
                        str(config_path),
                        f"unknown config key '{key}' on line {line_number}",
                    )
            else:
                raise ErrConfigInvalid(
                    str(config_path), f"malformed entry on line {line_number}"
                )

    if in_list:
        raise ErrConfigInvalid(str(config_path), "unterminated skills_dir list")
    return config


def load_trusted_keys(keys_dir: Path) -> list:
    """Load all trusted public keys from directory"""
    if not keys_dir.exists():
        raise ErrNoTrustedKeys(str(keys_dir))

    key_files = list(keys_dir.glob("*.asc"))
    if not key_files:
        raise ErrNoTrustedKeys(str(keys_dir))

    # If pgpy available, load key objects
    if pgpy is not None:
        keys = []
        for key_file in key_files:
            try:
                key, _ = pgpy.PGPKey.from_file(str(key_file))
                keys.append(key)
            except Exception:
                continue
        if keys:
            return keys

    # Fallback: return key file paths for gpg command (absolute paths)
    return [str(f.resolve()) for f in key_files]


def compute_file_hash(file_path: str) -> str:
    """Compute SHA256 hash of a file"""
    sha256 = hashlib.sha256()
    with open(file_path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            sha256.update(chunk)
    return sha256.hexdigest()


def _is_hidden_manifest_path(rel_path: str) -> bool:
    """Return True if a manifest-relative path contains a hidden component."""
    return any(part.startswith(".") for part in Path(rel_path).parts)


def collect_signed_file_paths(skill_dir: str) -> set[str]:
    """Collect files that should be covered by a skill manifest.

    This mirrors ``sign-skill.sh``: regular files are included recursively, while
    hidden files and files in hidden directories such as ``.skill-meta`` are
    ignored.
    """
    signed_paths: set[str] = set()
    root = Path(skill_dir)

    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [name for name in dirnames if not name.startswith(".")]

        for filename in filenames:
            if filename.startswith("."):
                continue

            file_path = Path(dirpath) / filename
            try:
                if not stat.S_ISREG(os.lstat(file_path).st_mode):
                    continue
            except OSError:
                continue

            rel_path = file_path.relative_to(root).as_posix()
            if _is_hidden_manifest_path(rel_path):
                continue
            signed_paths.add(rel_path)

    return signed_paths


def verify_signature_gpg(
    manifest_path: str, sig_path: str, key_files: list, skill_name: str
) -> bool:
    """Verify PGP signature using system gpg command"""
    if not GPG_BIN:
        raise ErrSigInvalid(
            skill_name, "Neither pgpy nor gpg available for signature verification"
        )

    with tempfile.TemporaryDirectory() as gnupg_home:
        # Set proper permissions for GNUPGHOME (GPG requires 700)
        os.chmod(gnupg_home, 0o700)

        # Use GNUPGHOME env var for proper GPG 2.x isolation
        env = os.environ.copy()
        env["GNUPGHOME"] = gnupg_home

        # Import all trusted keys into temporary keyring
        import_failed = []
        for key_file in key_files:
            result = subprocess.run(
                [GPG_BIN, "--batch", "--yes", "--import", key_file],
                capture_output=True,
                check=False,
                env=env,
            )
            if result.returncode != 0:
                import_failed.append(key_file)

        if import_failed and len(import_failed) == len(key_files):
            raise ErrSigInvalid(skill_name, "Failed to import any trusted keys")

        # Verify signature with trust-model always to bypass trustdb issues
        result = subprocess.run(
            [
                GPG_BIN,
                "--batch",
                "--yes",
                "--trust-model",
                "always",
                "--verify",
                sig_path,
                manifest_path,
            ],
            capture_output=True,
            check=False,
            env=env,
        )

        if result.returncode == 0:
            return True

        raise ErrSigInvalid(skill_name, result.stderr.decode().strip())


def verify_signature(
    manifest_path: str, sig_path: str, trusted_keys: list, skill_name: str
) -> bool:
    """Verify PGP signature of manifest"""
    # Check if trusted_keys contains pgpy key objects or file paths
    if trusted_keys and isinstance(trusted_keys[0], str):
        # File paths - use gpg command
        return verify_signature_gpg(manifest_path, sig_path, trusted_keys, skill_name)

    if pgpy is None:
        return verify_signature_gpg(manifest_path, sig_path, [], skill_name)

    with open(manifest_path, "rb") as f:
        manifest_data = f.read()

    sig = pgpy.PGPSignature.from_file(sig_path)

    for key in trusted_keys:
        try:
            verification = key.verify(manifest_data, sig)
            if verification:
                return True
        except Exception:
            continue

    raise ErrSigInvalid(skill_name, "No trusted key could verify the signature")


def verify_manifest_hashes(skill_dir: str, manifest: dict, skill_name: str) -> None:
    """Verify manifest hashes and reject unsigned files."""
    manifest_paths: set[str] = set()

    for file_entry in manifest.get("files", []):
        rel_path = file_entry["path"]
        expected_hash = file_entry["hash"]
        manifest_paths.add(rel_path)

        full_path = os.path.join(skill_dir, rel_path)
        if not os.path.exists(full_path):
            raise ErrHashMismatch(skill_name, rel_path, expected_hash, "<FILE_MISSING>")

        actual_hash = compute_file_hash(full_path)
        if actual_hash != expected_hash:
            raise ErrHashMismatch(skill_name, rel_path, expected_hash, actual_hash)

    for rel_path in sorted(collect_signed_file_paths(skill_dir) - manifest_paths):
        raise ErrUnexpectedFile(skill_name, rel_path)


def verify_skill(skill_dir: str, trusted_keys: list) -> tuple[bool, str]:
    """Verify a single skill directory"""
    skill_name = os.path.basename(skill_dir)
    signing_dir = os.path.join(skill_dir, SIGNING_DIR)
    manifest_path = os.path.join(signing_dir, "Manifest.json")
    sig_path = os.path.join(signing_dir, ".skill.sig")

    if not os.path.exists(manifest_path):
        raise ErrManifestMissing(skill_name)

    if not os.path.exists(sig_path):
        raise ErrSigMissing(skill_name)

    verify_signature(manifest_path, sig_path, trusted_keys, skill_name)

    with open(manifest_path, "r") as f:
        manifest = json.load(f)

    verify_manifest_hashes(skill_dir, manifest, skill_name)

    return True, skill_name


def verify_skills_dir(skills_dir: str, trusted_keys: list) -> SkillsDirectoryResult:
    """Verify all candidate skills in one best-effort search root.

    Missing roots are valid because packaged and raw installations use different
    locations. Existing roots that cannot be enumerated still raise an error.
    """
    root = Path(skills_dir)
    try:
        entries = sorted(root.iterdir(), key=lambda entry: entry.name)
    except FileNotFoundError:
        return {
            "checked": 0,
            "passed": [],
            "failed": [],
        }

    passed: list[str] = []
    failed: list[VerificationFailure] = []
    checked = 0

    for entry in entries:
        if entry.name.startswith("."):
            continue
        if not stat.S_ISDIR(entry.stat().st_mode):
            continue

        checked += 1
        try:
            _, skill_name = verify_skill(str(entry), trusted_keys)
            passed.append(skill_name)
        except Exception as e:
            failed.append({"name": entry.name, "error": str(e)})

    return {
        "checked": checked,
        "passed": passed,
        "failed": failed,
    }


def _verification_outcome(
    checked: int, failed: list[VerificationFailure]
) -> VerificationOutcome:
    """Return the semantic outcome for a completed verification run."""
    if failed:
        return "failed"
    if checked == 0:
        return "no_candidates"
    return "verified"


def run_verification(skill: str | None = None) -> VerificationResult:
    """Run verification and return structured results.

    Handles the full workflow: load trusted keys, verify single skill or
    all configured directories, and aggregate results.

    Args:
        skill: Optional path to a single skill directory.  When *None*,
               all directories listed in ``config.conf`` are scanned.

    Returns:
        Structured outcome, checked count, and per-skill results.
    """
    trusted_keys = load_trusted_keys(DEFAULT_TRUSTED_KEYS_DIR)

    if skill is not None:
        try:
            verify_skill(skill, trusted_keys)
            return {
                "outcome": "verified",
                "checked": 1,
                "passed": [os.path.basename(os.path.normpath(skill))],
                "failed": [],
            }
        except Exception as e:
            failed: list[VerificationFailure] = [
                {
                    "name": os.path.basename(os.path.normpath(skill)),
                    "error": str(e),
                }
            ]
            return {
                "outcome": "failed",
                "checked": 1,
                "passed": [],
                "failed": failed,
            }

    config = load_config(DEFAULT_CONFIG)
    all_passed: list[str] = []
    all_failed: list[VerificationFailure] = []
    checked = 0
    seen_roots: set[Path] = set()

    for skills_dir in config.get("skills_dirs", []):
        canonical_root = Path(skills_dir).resolve(strict=False)
        if canonical_root in seen_roots:
            continue
        seen_roots.add(canonical_root)

        results = verify_skills_dir(skills_dir, trusted_keys)
        checked += results["checked"]
        all_passed.extend(results["passed"])
        all_failed.extend(results["failed"])

    return {
        "outcome": _verification_outcome(checked, all_failed),
        "checked": checked,
        "passed": all_passed,
        "failed": all_failed,
    }


def format_verification_result(results: VerificationResult) -> str:
    """Render the canonical human-readable verification result."""
    output_lines: list[str] = []
    for name in results["passed"]:
        output_lines.append(f"[OK] {name}")
    for item in results["failed"]:
        output_lines.append(f"[ERROR] {item['name']}")
        output_lines.append(f"  {item['error']}")

    output_lines.append("")
    output_lines.append("=" * 50)
    output_lines.append(f"CHECKED: {results['checked']}")
    output_lines.append(f"PASSED: {len(results['passed'])}")
    output_lines.append(f"FAILED: {len(results['failed'])}")
    output_lines.append("=" * 50)

    status = {
        "verified": "VERIFICATION PASSED",
        "failed": "VERIFICATION FAILED",
        "no_candidates": "VERIFICATION SKIPPED: NO CANDIDATE SKILLS",
    }[results["outcome"]]
    output_lines.append(status)
    return "\n".join(output_lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Verify skill integrity and signatures"
    )
    parser.add_argument("--skill", "-s", help="Verify single skill directory")
    args = parser.parse_args()

    try:
        results = run_verification(args.skill)
        print(format_verification_result(results), end="")
        return 1 if results["outcome"] == "failed" else 0

    except Exception as e:
        print(f"[ERROR] {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
