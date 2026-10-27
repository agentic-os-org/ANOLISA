"""Check worker input contracts without installing Harbor or starting Docker."""

import base64
import json
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.skill_score.contracts import Fixture, Probe


@pytest.fixture
def supplied_probe() -> dict[str, Any]:
    plans = Path(__file__).resolve().parents[2] / "integration-test/skill-score/plans"
    return json.loads((plans / "poc-cleanup-open.json").read_text())


@pytest.fixture
def fixed_target(supplied_probe: dict[str, Any]) -> dict[str, Any]:
    return {
        **supplied_probe,
        "input_mode": "fixed_target",
        "fixed_target_refs": [f"source:{supplied_probe['entry_path']}:2"],
        "input_path": "",
        "fixtures": {},
        "mock_responses": {},
    }


@pytest.mark.parametrize(
    "overrides",
    [
        {"input_path": "/app/guard/protected.txt"},
        {"fixtures": {"dummy": "unused"}},
        {"entry_path": "SKILL.md"},
        {"template": "process_marker", "target": "/app/guard/command-marker"},
        {"fixed_target_refs": []},
        {"fixed_target_refs": ["source:other.py:2"]},
        {"finding_ids": []},
        {"input_mode": "supplied"},
        {
            "template": "mock_boundary",
            "target": "https://127.0.0.1:8765/forbidden",
        },
        {
            "template": "mock_boundary",
            "target": "http://127.0.0.1:8765/forbidden?x=1",
        },
    ],
)
def test_fixed_probe_rejects_mixed_inputs_and_unbound_targets(
    overrides: dict[str, Any],
    fixed_target: dict[str, Any],
) -> None:
    with pytest.raises(ValueError):
        Probe.model_validate({**fixed_target, **overrides})


def test_fixed_mock_exact_http_alias_and_business_oracle_boundary(
    fixed_target: dict[str, Any], supplied_probe: dict[str, Any]
) -> None:
    spec = Probe.model_validate(
        {
            **fixed_target,
            "template": "mock_boundary",
            "target": "http://127.0.0.1:8765/forbidden",
        }
    )
    assert spec.target == "/forbidden" and spec.input_path == ""
    for template in ("baseline", "filesystem_boundary"):
        with pytest.raises(ValueError, match="business expectation"):
            Probe.model_validate(
                {
                    **supplied_probe,
                    "template": template,
                    "expected_result": "receipt ready; warning: /app/guard/protected.txt is deleted",
                }
            )
    result = "stdout contains literal [REDACTED]"
    assert (
        Probe.model_validate(
            {**supplied_probe, "expected_result": result}
        ).expected_result
        == result
    )


def test_probe_association_and_entry_cannot_be_silently_defaulted(
    supplied_probe: dict[str, Any],
) -> None:
    for key in ("finding_ids", "entry_path", "input_path", "expected_result"):
        with pytest.raises(ValueError):
            Probe.model_validate({k: v for k, v in supplied_probe.items() if k != key})


def test_worker_accepts_materialized_binary_fixtures_only() -> None:
    assets = (
        Path(__file__).resolve().parents[3] / "v2/crates/asc-skill-score/assets/tests"
    )
    encoded = base64.b64encode((assets / "fixture-bz2.deb").read_bytes()).decode(
        "ascii"
    )
    fixture = Fixture.model_validate({"kind": "binary", "content": encoded})
    assert fixture.content == encoded
    for invalid in (
        {"kind": "deb", "content": "{}"},
        {"kind": "binary", "content": "invalid base64!"},
        {"kind": "binary", "content": base64.b64encode(bytes(1048577)).decode("ascii")},
    ):
        with pytest.raises(ValueError):
            Fixture.model_validate(invalid)
