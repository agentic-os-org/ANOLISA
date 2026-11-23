"""Keep source identity and token accounting consistent in batch exports."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


def assistant_records() -> list[dict]:
    return [
        {
            "type": "assistant",
            "message": {
                "id": "same-id",
                "content": [{"type": "text", "text": text}],
                "usage": {"input_tokens": prompt},
            },
        }
        for text, prompt in [("part one", 100), ("part two", 40)]
    ]


def runtime_config(entrypoint: object) -> dict:
    return {"type": "runtime-config", "entrypoint": entrypoint}


class BatchSourceAccountingTests(unittest.TestCase):
    def check_export(
        self, events: list[dict], directory: str, expected: int, agent_name: str
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / directory / "session.jsonl"
            path.parent.mkdir()
            path.write_text(
                "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
            )
            trajectory = batch_analyze.jsonl_to_atif(path)
            self.assertIsNotNone(trajectory)
            self.assertEqual(trajectory["agent"]["name"], agent_name)
            self.assertEqual(
                [step["message"] for step in trajectory["steps"]],
                ["part one", "part two"],
            )
            self.assertEqual(
                sum(
                    step.get("metrics", {}).get("prompt_tokens", 0)
                    for step in trajectory["steps"]
                ),
                expected,
            )
            exported = batch_analyze.export_atif(trajectory, path, root / "export")
            self.assertEqual(
                json.loads(exported.read_text(encoding="utf-8")), trajectory
            )

    def test_accounting_follows_declared_source(self) -> None:
        cases = [
            (".qoder", None, 140, "qoder"),
            (".qoder", "qoder", 140, "qoder"),
            (".claude", None, 40, "claude-code"),
            (".claude", "qoder", 140, "qoder"),
            (".qoder", "claude-code", 40, "claude-code"),
            ("backup", "qoder", 140, "qoder"),
            ("backup", None, 40, "unknown"),
        ]
        for directory, entrypoint, expected, name in cases:
            for position in [0, 1, 2]:
                with self.subTest(
                    directory=directory, entrypoint=entrypoint, position=position
                ):
                    events = assistant_records()
                    if entrypoint is not None:
                        events.insert(position, runtime_config(entrypoint))
                    self.check_export(events, directory, expected, name)

    def test_anonymous_qoder_records_keep_independent_measurements(self) -> None:
        events = assistant_records()
        for event in events:
            del event["message"]["id"]
        self.check_export(events, ".qoder", 140, "qoder")

    def test_conflicting_declarations_keep_first_identity_and_accounting(self) -> None:
        for first, last, expected in [
            ("qoder", "claude-code", 140),
            ("claude-code", "qoder", 40),
            ("qoderwork", "claude-code", 140),
        ]:
            with self.subTest(first=first, last=last):
                events = assistant_records()
                events.insert(1, runtime_config(first))
                events.append(runtime_config(last))
                self.check_export(events, "backup", expected, first)

    def test_unknown_identity_retains_path_accounting(self) -> None:
        for directory, expected in [(".qoder", 140), (".claude", 40)]:
            with self.subTest(directory=directory):
                events = assistant_records()
                events.insert(1, runtime_config("custom-agent"))
                events.append(
                    runtime_config("claude-code" if expected == 140 else "qoder")
                )
                self.check_export(events, directory, expected, "custom-agent")

    def test_empty_or_invalid_identity_does_not_block_a_known_source(self) -> None:
        for ignored in [None, "", 42]:
            with self.subTest(entrypoint=ignored):
                events = [
                    runtime_config(ignored),
                    *assistant_records(),
                    runtime_config("qoder"),
                ]
                self.check_export(events, "backup", 140, "qoder")


if __name__ == "__main__":
    unittest.main()
