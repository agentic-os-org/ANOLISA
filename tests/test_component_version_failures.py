"""One invalid metadata file must not hide independent contract failures."""

import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/check-component-versions.py"
SPEC = importlib.util.spec_from_file_location("component_version_failures", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class FailureIsolationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.write("source.json", '{"version":"1.0.0"}')
        self.write("wrong.json", '{"version":"2.0.0"}')
        self.write("bad.json", "{not JSON")
        self.write("template.json.in", '{"version":"wrong"}')
        self.write("src/agent-memory/Cargo.toml", 'version = "1.0.0"')
        self.write("memory.json", '{"version":"3.0.0"}')
        self.write(
            "src/agent-memory/adapters/agent-memory/openclaw/package-lock.json",
            json.dumps({"version": "4.0.0"}),
        )

    def write(self, name: str, content: str) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def run_gate(self, contracts: tuple) -> tuple[int, str]:
        output = io.StringIO()
        with (
            patch.object(CHECKER, "ROOT", self.root),
            patch.object(CHECKER, "TOML_CONTRACTS", contracts),
            patch.object(CHECKER, "VERSION_TEMPLATES", (("source.json", "template.json.in"),)),
            patch.object(CHECKER, "AGENT_MEMORY_JSON", ("bad.json", "memory.json")),
            patch.object(
                CHECKER,
                "check_generated_contracts_untracked",
                side_effect=lambda errors: errors.append("generated contract was tracked"),
            ),
            redirect_stderr(output),
        ):
            result = CHECKER.main()
        return result, output.getvalue()

    def test_missing_file_does_not_hide_later_contracts(self) -> None:
        result, output = self.run_gate(
            (("missing.json", "wrong.json"), ("source.json", "wrong.json"))
        )
        self.assertEqual(result, 1)
        for expected in (
            "missing.json",
            "wrong.json: expected",
            "template.json.in",
            "bad.json",
            "memory.json",
            "package-lock.json",
            "generated contract was tracked",
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, output)

    def test_invalid_json_does_not_hide_following_mismatches(self) -> None:
        result, output = self.run_gate((("source.json", "bad.json"), ("source.json", "wrong.json")))
        self.assertEqual(result, 1)
        self.assertIn("bad.json", output)
        self.assertIn("wrong.json: expected", output)
        self.assertIn("generated contract was tracked", output)


if __name__ == "__main__":
    unittest.main()
