"""Copied transcripts keep their recorded identity, not the filename."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


def rt_cfg(session_id: str, version: str, entrypoint: str) -> dict:
    return {
        "type": "runtime-config",
        "sessionId": session_id,
        "version": version,
        "entrypoint": entrypoint,
        "model": "qwen-max",
    }


class BatchExportIdentityTests(unittest.TestCase):
    def convert(self, events: list, filename: str = "renamed.jsonl") -> dict:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / filename
            path.write_text(
                "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
            )
            return batch_analyze.jsonl_to_atif(path)

    def user(self, content: str = "inspect the project") -> dict:
        return {"type": "user", "message": {"role": "user", "content": content}}

    def test_runtime_config_identity_beats_the_filename(self) -> None:
        document = self.convert(
            [
                self.user(),
                rt_cfg_body("real-session", "1.2.3", "qoderwork"),
            ]
        )
        self.assertEqual(document["session_id"], "real-session")
        self.assertEqual(document["agent"]["name"], "qoderwork")
        self.assertEqual(document["agent"]["version"], "1.2.3")

    def test_late_runtime_config_still_wins(self) -> None:
        document = self.convert(
            [
                self.user(),
                rt_cfg_body("real-session", "1.2.3", "claude-code"),
            ]
        )
        self.assertEqual(document["session_id"], "real-session")
        self.assertEqual(document["agent"]["name"], "claude-code")

    def test_message_level_identity_is_the_fallback(self) -> None:
        record = self.user()
        record["sessionId"] = "marked-session"
        record["version"] = "0.9.9"
        document = self.convert([record])
        self.assertEqual(document["session_id"], "marked-session")
        self.assertEqual(document["agent"]["version"], "0.9.9")

    def test_filename_stem_is_the_last_fallback(self) -> None:
        document = self.convert([self.user()])
        self.assertEqual(document["session_id"], "renamed")
        self.assertEqual(document["agent"]["version"], "")

    def test_empty_metadata_fields_fall_through(self) -> None:
        document = self.convert(
            [
                rt_cfg_body("", "", ""),
                self.user(),
            ]
        )
        self.assertEqual(document["session_id"], "renamed")
        self.assertEqual(document["agent"]["name"], "unknown")

    def test_invalid_session_id_type_is_ignored(self) -> None:
        events = [self.user(), {"type": "runtime-config", "sessionId": 42}]
        document = self.convert(events)
        self.assertEqual(document["session_id"], "renamed")

    def test_path_fallback_names_the_agent_without_runtime_config(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".claude" / "projects" / "p.jsonl"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(self.user()) + "\n", encoding="utf-8")
            document = batch_analyze.jsonl_to_atif(path)
        self.assertEqual(document["agent"]["name"], "claude-code")


def rt_cfg_body(session_id: str, version: str, entrypoint: str) -> dict:
    return {
        "type": "runtime-config",
        "sessionId": session_id,
        "version": version,
        "entrypoint": entrypoint,
        "model": "qwen-max",
    }


def rt_cfg(session_id: str, version: str, entrypoint: str) -> dict:
    return rt_cfg_body(session_id, version, entrypoint)


if __name__ == "__main__":
    unittest.main()
