"""Qoder's partial records must not be deduplicated as Claude snapshots."""

import json
from pathlib import Path

import pytest
from test_batch_analyze import batch_analyze


@pytest.mark.parametrize(
    ("directory", "entrypoint", "expected"),
    [
        (".qoder", None, 140),
        (".qoder", "qoder", 140),
        (".claude", None, 40),
        (".claude", "qoder", 140),
        (".qoder", "claude-code", 40),
        ("backup", "qoder", 140),
        ("backup", None, 40),
    ],
)
def test_accounting_follows_declared_source(
    tmp_path: Path, directory: str, entrypoint: str | None, expected: int
) -> None:
    path = tmp_path / directory / "session.jsonl"
    path.parent.mkdir()
    events = []
    if entrypoint is not None:
        events.append({"type": "runtime-config", "entrypoint": entrypoint})
    for text, prompt in [("part one", 100), ("part two", 40)]:
        events.append(
            {
                "type": "assistant",
                "message": {
                    "id": "same-id",
                    "content": [{"type": "text", "text": text}],
                    "usage": {"input_tokens": prompt},
                },
            }
        )
    path.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    trajectory = batch_analyze.jsonl_to_atif(path)
    assert [step["message"] for step in trajectory["steps"]] == ["part one", "part two"]
    assert (
        sum(step.get("metrics", {}).get("prompt_tokens", 0) for step in trajectory["steps"])
        == expected
    )
    exported = batch_analyze.export_atif(trajectory, path, tmp_path / "export")
    assert json.loads(exported.read_text(encoding="utf-8")) == trajectory


def test_anonymous_qoder_records_keep_independent_measurements(tmp_path: Path) -> None:
    path = tmp_path / ".qoder" / "session.jsonl"
    path.parent.mkdir()
    events = [
        {"type": "assistant", "message": {"content": "answer", "usage": {"input_tokens": value}}}
        for value in [100, 40]
    ]
    path.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    trajectory = batch_analyze.jsonl_to_atif(path)
    assert sum(step["metrics"]["prompt_tokens"] for step in trajectory["steps"]) == 140
