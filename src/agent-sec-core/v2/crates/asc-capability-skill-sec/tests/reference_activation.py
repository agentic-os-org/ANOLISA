"""Freeze V1 activation workflows; publication/recovery additions are tested separately on Linux."""

import hashlib
import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

from reference_ledger import SKILL, Backend, run_step

# isort: split

from agent_sec_cli.skill_ledger.core.decision import (
    clear_decision,
    decide_skill,
    export_skill,
    show_skill,
)
from agent_sec_cli.skill_ledger.core.live_root import ResolvedSkillRoot
from agent_sec_cli.skill_ledger.core.resolver import resolve_activation

FIELDS = {
    "status",
    "versionId",
    "scanStatus",
    "currentStatus",
    "newVersion",
    "latestStatus",
    "latestVersionId",
    "activeVersionId",
    "target",
    "reasonCode",
    "userDecision",
    "activation",
    "latest",
    "active",
    "rootMatchesActive",
    "consistencyReason",
    "findings",
    "warnings",
    "message",
    "activationPolicy",
    "policy",
    "schemaVersion",
    "action",
    "reason",
    "targetVersionId",
    "scannersRun",
    "skippedScanners",
}


def project(value: Any) -> Any:
    if isinstance(value, dict):
        # Export's findings is an output filename; actual finding arrays remain fully compared.
        return {
            key: project(item)
            for key, item in value.items()
            if key in FIELDS and not (key == "findings" and isinstance(item, str))
        }
    if isinstance(value, list):
        return value  # Findings are already normalized by the scanner contract.
    return value


def cases() -> list[dict[str, Any]]:
    def cert(level: str | None = None) -> dict[str, Any]:
        return {
            "op": "certify",
            "scanner": "fixture",
            "findings": (
                []
                if level is None
                else [{"rule": "review", "level": level, "message": "review required"}]
            ),
        }

    def decide(action: str, version: str | None = None) -> dict[str, Any]:
        return {
            "op": "decide",
            "action": action,
            **({"version": version} if version else {}),
        }

    show = {"op": "show"}
    activate = {"op": "activate"}
    write = {"op": "write", "path": "main.sh", "content": "echo changed\n"}
    clear = {"op": "clear"}
    return [
        {"name": "fresh-pending", "steps": [show, activate]},
        {"name": "pass-warn", "steps": [cert(), show, cert("warn"), show, activate]},
        {
            "name": "deny-decisions",
            "steps": [
                cert("deny"),
                show,
                decide("allow"),
                show,
                decide("block"),
                show,
                clear,
                show,
            ],
        },
        {
            "name": "allow-pins-older",
            "steps": [
                cert("deny"),
                decide("allow"),
                write,
                cert("deny"),
                show,
                {"op": "export", "version": "active"},
                clear,
                show,
            ],
        },
        {
            "name": "always-allow-inherits",
            "steps": [
                cert("deny"),
                decide("always_allow"),
                write,
                cert("deny"),
                show,
                clear,
                show,
            ],
        },
        {
            "name": "policy-fallback",
            "steps": [cert(), write, cert("deny"), show, activate],
        },
        {
            "name": "block-new-version",
            "steps": [
                cert(),
                decide("block"),
                show,
                write,
                cert("deny"),
                show,
                cert(),
                show,
            ],
        },
        {
            "name": "invalid-suppresses-allow",
            "steps": [
                cert("deny"),
                decide("allow"),
                {
                    "op": "write",
                    "path": ".skill-meta/versions/v000002.json",
                    "content": "{}",
                },
                show,
                activate,
            ],
        },
        {
            "name": "rollback-previous",
            "steps": [
                cert(),
                write,
                cert("deny"),
                decide("rollback", "v000001"),
                show,
                {"op": "check"},
                {"op": "export", "version": "active"},
            ],
        },
        {
            "name": "rollback-drift-default",
            "steps": [cert(), write, show, decide("rollback"), show],
        },
        {
            "name": "damaged-latest",
            "steps": [
                cert(),
                {"op": "write", "path": ".skill-meta/latest.json", "content": "{}"},
                show,
                decide("allow"),
                clear,
            ],
        },
        {
            "name": "damaged-snapshot",
            "steps": [
                cert(),
                {
                    "op": "write",
                    "path": ".skill-meta/versions/v000001.snapshot/main.sh",
                    "content": "tampered",
                },
                show,
                activate,
                decide("allow"),
                decide("rollback", "v000001"),
            ],
        },
    ]


def main() -> None:
    repo = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True
        ).strip()
    )
    source = repo / "src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger"
    result = cases()
    for case in result:
        # Repeated dicts represent repeated observations, never shared expected results.
        case["steps"] = [dict(step) for step in case["steps"]]
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            skill = directory / "skill"
            skill.mkdir()
            (skill / "SKILL.md").write_text(SKILL)
            (skill / "main.sh").write_text("echo safe\n")
            root = ResolvedSkillRoot(skill, skill, "host")
            backend = Backend()
            with patch(
                "agent_sec_cli.skill_ledger.config.get_config_dir",
                return_value=directory / "config",
            ), patch(
                "socket.socket.connect",
                side_effect=AssertionError("oracle cannot access network"),
            ):
                for index, step in enumerate(case["steps"]):
                    try:
                        op = step["op"]
                        if op == "show":
                            output = show_skill(root, backend)
                        elif op == "activate":
                            output = resolve_activation(root, backend)
                        elif op == "decide":
                            output = decide_skill(
                                root,
                                backend,
                                action=step["action"],
                                target_version_id=step.get("version"),
                                reason="fixture review",
                            )
                        elif op == "clear":
                            output = clear_decision(root, backend)
                        elif op == "export":
                            output = export_skill(
                                root,
                                backend,
                                version=step["version"],
                                output=str(directory / f"export-{index}"),
                            )
                        else:
                            output = run_step(root, backend, step, directory)
                    except Exception as error:
                        if op in {"write", "remove", "mkdir"}:
                            raise
                        step["expected"] = {"executionError": type(error).__name__}
                    else:
                        if output is not None:
                            step["expected"] = project(output)
    document = {
        "source_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], text=True
        ).strip(),
        "source_hashes": {
            str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(source.rglob("*.py"))
        },
        "cases": result,
    }
    output_path = Path(__file__).parent / "fixtures/activation.json"
    output_path.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    print(f"Wrote {len(result)} activation workflows to {output_path}")


if __name__ == "__main__":
    main()
