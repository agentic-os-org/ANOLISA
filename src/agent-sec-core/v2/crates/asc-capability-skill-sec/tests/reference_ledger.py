"""Freeze V1 Ledger business workflows using real Ed25519 signatures and synthetic data.

Requires Python 3.11.6, cryptography, PyYAML and pydantic. Rust imports neither
this script nor any Python business code. Keys and platform paths are excluded
from comparison because V2 deliberately uses a new system trust generation.
"""

import base64
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

from agent_sec_cli.skill_ledger.core.auditor import audit
from agent_sec_cli.skill_ledger.core.certifier import certify, scan_skill
from agent_sec_cli.skill_ledger.core.checker import check
from agent_sec_cli.skill_ledger.core.live_root import ResolvedSkillRoot
from agent_sec_cli.skill_ledger.errors import SignatureInvalidError
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

SKILL = "---\nname: fixture\ndescription: Local fixture\n---\nUse local files.\n"


class Backend(SigningBackend):
    """Real in-memory Ed25519 backend; no user key/config access."""

    def __init__(self) -> None:
        self.key = Ed25519PrivateKey.generate()
        public = self.key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        self.fingerprint = "sha256:" + hashlib.sha256(public).hexdigest()

    @property
    def name(self) -> str:
        return "ed25519"

    def generate_keys(self, passphrase: str | None = None) -> dict[str, Any]:
        return {"fingerprint": self.fingerprint}

    def sign(self, data: bytes) -> tuple[str, str]:
        return base64.b64encode(self.key.sign(data)).decode("ascii"), self.fingerprint

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        if fingerprint != self.fingerprint:
            raise SignatureInvalidError("unknown fixture key")
        try:
            self.key.public_key().verify(base64.b64decode(signature_b64), data)
        except InvalidSignature as error:
            raise SignatureInvalidError("invalid fixture signature") from error
        return True

    def get_public_key_fingerprint(self) -> str:
        return self.fingerprint


def cases() -> list[dict[str, Any]]:
    def cert(scanner: str = "fixture", level: str | None = None) -> dict[str, Any]:
        findings = (
            [{"rule": "review", "level": level, "message": "review required"}]
            if level
            else []
        )
        return {"op": "certify", "scanner": scanner, "findings": findings}

    def write(path: str, content: str) -> dict[str, Any]:
        return {"op": "write", "path": path, "content": content}

    def check_step() -> dict[str, Any]:
        return {"op": "check"}

    return [
        {
            "name": "certify-merge-replace",
            "steps": [
                check_step(),
                cert("one"),
                check_step(),
                cert("two", "deny"),
                check_step(),
                cert("two", "warn"),
                check_step(),
                cert("two"),
                check_step(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "content-versions",
            "steps": [
                cert(),
                write("main.sh", "echo new\n"),
                check_step(),
                cert(),
                {"op": "remove", "path": "main.sh"},
                write("new.txt", "new"),
                check_step(),
                cert(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "scan-fill-force",
            "steps": [
                {"op": "scan", "scanners": ["code-scanner"]},
                {"op": "scan"},
                {"op": "scan"},
                {"op": "scan", "force": True},
                check_step(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "import-only-scan",
            "steps": [
                {"op": "scan", "scanners": ["skill-vetter"]},
                cert(),
                {"op": "scan", "scanners": ["skill-vetter"]},
            ],
        },
        {
            "name": "missing-latest",
            "steps": [
                cert(),
                {"op": "remove", "path": ".skill-meta/latest.json"},
                check_step(),
                cert(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "corrupt-latest",
            "steps": [
                cert(),
                write(".skill-meta/latest.json", "{}"),
                check_step(),
                cert(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "corrupt-snapshot",
            "steps": [
                cert(),
                write(".skill-meta/versions/v000001.snapshot/main.sh", "tampered"),
                check_step(),
                {"op": "audit"},
                {"op": "audit", "snapshots": True},
                cert(),
                check_step(),
            ],
        },
        {
            "name": "missing-snapshot",
            "steps": [
                cert(),
                {"op": "remove", "path": ".skill-meta/versions/v000001.snapshot"},
                check_step(),
                {"op": "audit", "snapshots": True},
                cert(),
            ],
        },
        {
            "name": "orphan-and-high-slot",
            "steps": [
                {"op": "mkdir", "path": ".skill-meta/versions/v000001.snapshot"},
                write(".skill-meta/versions/v999999.json", "{}"),
                check_step(),
                cert(),
                {"op": "audit", "snapshots": True},
            ],
        },
        {
            "name": "excluded-content",
            "steps": [
                cert(),
                write(".git/config", "ignored"),
                check_step(),
                write("build/data", "hashed although scanner excludes this directory"),
                check_step(),
                cert(),
            ],
        },
    ]


def run_step(
    root: ResolvedSkillRoot, backend: Backend, step: dict[str, Any], directory: Path
) -> dict[str, Any] | None:
    operation = step["op"]
    if operation in {"write", "remove", "mkdir"}:
        target = root.io_dir / step["path"]
        if operation == "write":
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(step["content"])
        elif operation == "mkdir":
            target.mkdir(parents=True, exist_ok=True)
        elif target.is_dir():
            shutil.rmtree(target)
        else:
            target.unlink()
        return None
    if operation == "check":
        return check(root, backend)
    if operation == "audit":
        return audit(root, backend, verify_snapshots=step.get("snapshots", False))
    if operation == "scan":
        return scan_skill(
            root,
            backend,
            scanner_names=step.get("scanners"),
            force=step.get("force", False),
        )
    findings = directory / "findings.json"
    findings.write_text(json.dumps(step["findings"]))
    return certify(
        root,
        backend,
        findings_path=str(findings),
        scanner=step["scanner"],
        scanner_version="test-1",
    )


def main() -> None:
    repo = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True
        ).strip()
    )
    source = repo / "src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger"
    hashes = {
        str(path.relative_to(source)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(source.rglob("*.py"))
    }
    result = cases()
    fields = {
        "status",
        "versionId",
        "scanStatus",
        "newVersion",
        "fileCount",
        "scannersRun",
        "skippedScanners",
        "findings",
        "added",
        "removed",
        "modified",
        "valid",
        "versions_checked",
    }
    for case in result:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            skill = directory / "skill"
            skill.mkdir()
            (skill / "SKILL.md").write_text(SKILL)
            (skill / "main.sh").write_text("echo safe\n")
            root = ResolvedSkillRoot(skill, skill, "host")
            backend = Backend()
            # Only redirect configuration and forbid sockets; all domain functions run unchanged.
            with patch(
                "agent_sec_cli.skill_ledger.config.get_config_dir",
                return_value=directory / "config",
            ), patch(
                "socket.socket.connect",
                side_effect=AssertionError("oracle cannot access network"),
            ):
                for step in case["steps"]:
                    try:
                        output = run_step(root, backend, step, directory)
                    except Exception as error:
                        if step["op"] not in {"scan", "certify", "check", "audit"}:
                            raise
                        step["expected"] = {"executionError": type(error).__name__}
                    else:
                        if output is not None:
                            step["expected"] = {
                                key: value
                                for key, value in output.items()
                                if key in fields
                            }
    document = {
        "source_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], text=True
        ).strip(),
        "source_hashes": hashes,
        "cases": result,
    }
    output_path = Path(__file__).parent / "fixtures/ledger.json"
    output_path.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    print(f"Wrote {len(result)} source-pinned Ledger workflows to {output_path}")


if __name__ == "__main__":
    main()
