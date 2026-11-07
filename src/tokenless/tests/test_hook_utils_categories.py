#!/usr/bin/env python3
"""Fresh-import regression tests for the hook_utils tool-category loader.

``hook_utils`` reads ``tool_categories.json`` at module import time, so a
configuration the loader cannot consume aborts every lifecycle hook sharing the
module instead of one malformed request. The loader documents a fallback to the
built-in safe classifications, but it validated only the layer and list
containers and caught a subset of the read and decode errors, so all of these
escaped and exited 1 before any hook reached its pass-through handling:

- a JSON ``null``, or any other non-mapping top level -> ``TypeError`` on ``in``;
- a ``tools`` entry that is not a string -> ``TypeError: unhashable type`` from
  ``set()``;
- a ``thresholds`` mapping that is ``null`` -> ``AttributeError`` on ``.get``;
- any read failure other than FileNotFoundError, for example a directory sitting
  at the JSON path -> ``IsADirectoryError``.

Each case copies the real ``hook_utils.py`` into a sandbox directory beside a
generated ``tool_categories.json`` and imports it in a fresh interpreter, which
is the only way to exercise import-time behaviour. Unavailable or malformed
category data must import cleanly, keep the existing stderr diagnostic, and
expose the built-in safe categories and default thresholds; valid custom tool
names and threshold overrides must be unchanged. Invalid UTF-8 stays covered as
the regression control for the decode path that already fell back.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
COMMON_HOOKS = TESTS_DIR.parent / "adapters" / "tokenless" / "common" / "hooks"
HOOK_UTILS = COMMON_HOOKS / "hook_utils.py"
SHIPPED_CATEGORIES = COMMON_HOOKS / "tool_categories.json"
CATEGORIES_NAME = "tool_categories.json"

# Thresholds the module hardcodes per layer. The shipped JSON repeats the same
# numbers, so they are asserted separately from the tool-name sets.
DEFAULT_SHELL_THRESHOLDS = [65_536, 128, 8]
DEFAULT_API_THRESHOLDS = [1_048_576, 65_536, 32]

# Names the safe built-in classification must always keep: content retrieval is
# never compressed, shell output is moderately truncated.
SAFE_SKIP_NAMES = ("Read", "Grep", "Glob")
SAFE_SHELL_NAMES = ("Bash", "Shell")

WARNING_PREFIX = "Warning: Failed to load tool_categories.json:"

# A tool name no configuration in this suite defines, so get_thresholds() takes
# its Layer 3 branch for it.
API_PROBE_TOOL = "TokenlessProbeApiTool"

PROBE = """
import json

import hook_utils

shell_probe = sorted(hook_utils.SHELL_TOOLS)[0] if hook_utils.SHELL_TOOLS else "Bash"
print(json.dumps({
    "skip": sorted(hook_utils.SKIP_TOOLS),
    "shell": sorted(hook_utils.SHELL_TOOLS),
    "shell_thresholds": list(hook_utils.get_thresholds(shell_probe)),
    "api_thresholds": list(hook_utils.get_thresholds(%r)),
}))
""" % API_PROBE_TOOL


def shipped_categories() -> dict:
    """Return a copy of the shipped tool_categories.json payload."""
    return json.loads(SHIPPED_CATEGORIES.read_text(encoding="utf-8"))


def with_layer(payload: dict, layer: str, **fields) -> dict:
    """Return *payload* with *layer* replaced by a copy carrying *fields*."""
    updated = dict(payload)
    merged = dict(updated[layer]) if isinstance(updated.get(layer), dict) else {}
    merged.update(fields)
    updated[layer] = merged
    return updated


def malformed_cases() -> list:
    """Return (name, payload, expected diagnostic fragment) for invalid shapes."""
    shipped = shipped_categories()
    return [
        ("top-level-null", None, "Top level must be a dict, got NoneType"),
        ("top-level-list", [shipped], "Top level must be a dict, got list"),
        ("top-level-string", "layer_1_skip", "Top level must be a dict, got str"),
        (
            "unhashable-tools-entry",
            with_layer(shipped, "layer_1_skip", tools=[{}]),
            "Layer layer_1_skip.tools entries must be strings, got dict",
        ),
        (
            "nested-tools-entry",
            with_layer(shipped, "layer_2_shell", tools=["Bash", ["shell"]]),
            "Layer layer_2_shell.tools entries must be strings, got list",
        ),
        (
            "null-tools-entry",
            with_layer(shipped, "layer_1_skip", tools=["Read", None]),
            "Layer layer_1_skip.tools entries must be strings, got NoneType",
        ),
        (
            "tools-is-a-mapping",
            with_layer(shipped, "layer_2_shell", tools={"Bash": True}),
            "Layer layer_2_shell.tools must be a list",
        ),
        (
            "tools-is-a-string",
            with_layer(shipped, "layer_1_skip", tools="Read"),
            "Layer layer_1_skip.tools must be a list",
        ),
        (
            "null-layer-2-thresholds",
            with_layer(shipped, "layer_2_shell", thresholds=None),
            "Layer layer_2_shell.thresholds must be a dict",
        ),
        (
            "list-layer-3-thresholds",
            with_layer(shipped, "layer_3_api", thresholds=[1, 2]),
            "Layer layer_3_api.thresholds must be a dict",
        ),
        (
            "string-threshold-value",
            with_layer(
                shipped,
                "layer_2_shell",
                thresholds={"truncate_strings_at": "64k"},
            ),
            "Layer layer_2_shell.thresholds.truncate_strings_at "
            "must be a positive integer",
        ),
        (
            "float-threshold-value",
            with_layer(shipped, "layer_3_api", thresholds={"max_depth": 8.0}),
            "Layer layer_3_api.thresholds.max_depth must be a positive integer",
        ),
        (
            "boolean-threshold-value",
            with_layer(shipped, "layer_2_shell", thresholds={"max_depth": True}),
            "Layer layer_2_shell.thresholds.max_depth must be a positive integer",
        ),
        (
            "zero-threshold-value",
            with_layer(shipped, "layer_2_shell", thresholds={"truncate_arrays_at": 0}),
            "Layer layer_2_shell.thresholds.truncate_arrays_at "
            "must be a positive integer",
        ),
        (
            "negative-threshold-value",
            with_layer(shipped, "layer_3_api", thresholds={"max_depth": -3}),
            "Layer layer_3_api.thresholds.max_depth must be a positive integer",
        ),
        ("null-layer", dict(shipped, layer_3_api=None), "Layer layer_3_api must be a dict"),
        (
            "missing-layer",
            {k: v for k, v in shipped.items() if k != "layer_2_shell"},
            "Missing required layer: layer_2_shell",
        ),
        (
            "missing-tools-field",
            dict(shipped, layer_1_skip={"_description": "no tools key"}),
            "Layer layer_1_skip missing 'tools' field",
        ),
    ]


class ImportProbe:
    """Result of importing hook_utils in a fresh interpreter."""

    def __init__(self, completed):
        self.returncode = completed.returncode
        self.stdout = completed.stdout
        self.stderr = completed.stderr
        self.values = json.loads(completed.stdout) if completed.stdout.strip() else None


class CategoryImportTest(unittest.TestCase):
    """Import a copy of hook_utils beside a generated tool_categories.json."""

    @classmethod
    def setUpClass(cls):
        # Source-tree import: the shipped JSON is valid, so this only reads the
        # built-in safe sets the fallback is contractually required to expose.
        sys.path.insert(0, str(COMMON_HOOKS))
        try:
            sys.modules.pop("hook_utils", None)
            import hook_utils

            cls.fallback_skip = sorted(hook_utils._FALLBACK_SKIP_TOOLS)
            cls.fallback_shell = sorted(hook_utils._FALLBACK_SHELL_TOOLS)
        finally:
            sys.path.pop(0)
            sys.modules.pop("hook_utils", None)

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="tokenless-categories-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def make_sandbox(self, name: str = "hooks") -> Path:
        """Create a directory holding a copy of hook_utils.py and return it."""
        sandbox = self.tmp / name
        sandbox.mkdir(parents=True)
        shutil.copy(HOOK_UTILS, sandbox / "hook_utils.py")
        return sandbox

    def import_categories(self, sandbox: Path) -> ImportProbe:
        """Import the copied hook_utils in a fresh interpreter inside sandbox."""
        completed = subprocess.run(
            [sys.executable, "-B", "-c", PROBE],
            cwd=str(sandbox),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        return ImportProbe(completed)

    def write_categories(self, sandbox: Path, payload) -> Path:
        path = sandbox / CATEGORIES_NAME
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    def write_categories_bytes(self, sandbox: Path, data: bytes) -> Path:
        path = sandbox / CATEGORIES_NAME
        path.write_bytes(data)
        return path

    def assert_falls_back(self, probe: ImportProbe, expected_warning: str) -> None:
        """Assert a clean import that used the built-in safe categories."""
        self.assertEqual(probe.returncode, 0, probe.stderr)
        self.assertIsNotNone(probe.values, probe.stderr)
        self.assertIn(WARNING_PREFIX, probe.stderr)
        self.assertIn(expected_warning, probe.stderr)
        self.assertEqual(probe.values["skip"], self.fallback_skip)
        self.assertEqual(probe.values["shell"], self.fallback_shell)
        self.assertEqual(probe.values["shell_thresholds"], DEFAULT_SHELL_THRESHOLDS)
        self.assertEqual(probe.values["api_thresholds"], DEFAULT_API_THRESHOLDS)
        for name in SAFE_SKIP_NAMES:
            self.assertIn(name, probe.values["skip"])
        for name in SAFE_SHELL_NAMES:
            self.assertIn(name, probe.values["shell"])

    def assert_uses_config(self, probe: ImportProbe) -> None:
        """Assert a clean import that kept the configured categories."""
        self.assertEqual(probe.returncode, 0, probe.stderr)
        self.assertIsNotNone(probe.values, probe.stderr)
        self.assertEqual(probe.stderr, "")


class MalformedCategoryTest(CategoryImportTest):
    """Unavailable or malformed category data must never abort the import."""

    def test_malformed_payloads_fall_back(self):
        for name, payload, expected_warning in malformed_cases():
            with self.subTest(case=name):
                sandbox = self.make_sandbox(name)
                self.write_categories(sandbox, payload)
                self.assert_falls_back(self.import_categories(sandbox), expected_warning)

    def test_missing_file_falls_back(self):
        sandbox = self.make_sandbox()
        self.assert_falls_back(self.import_categories(sandbox), "No such file or directory")

    def test_directory_at_json_path_falls_back(self):
        # IsADirectoryError escaped the old FileNotFoundError-only handler.
        sandbox = self.make_sandbox()
        (sandbox / CATEGORIES_NAME).mkdir()
        self.assert_falls_back(self.import_categories(sandbox), "Is a directory")

    @unittest.skipIf(
        hasattr(os, "geteuid") and os.geteuid() == 0,
        "root bypasses file permissions",
    )
    def test_unreadable_file_falls_back(self):
        sandbox = self.make_sandbox()
        path = self.write_categories(sandbox, shipped_categories())
        path.chmod(0o000)
        self.addCleanup(path.chmod, 0o600)
        self.assert_falls_back(self.import_categories(sandbox), "Permission denied")

    def test_invalid_utf8_falls_back(self):
        # Regression control: UnicodeDecodeError is a ValueError, so this path
        # already fell back and must keep doing so.
        sandbox = self.make_sandbox()
        self.write_categories_bytes(sandbox, b"\xff\xfe\x00not utf-8")
        self.assert_falls_back(self.import_categories(sandbox), "codec can't decode")

    def test_truncated_json_falls_back(self):
        sandbox = self.make_sandbox()
        self.write_categories_bytes(sandbox, b'{"layer_1_skip": ')
        self.assert_falls_back(self.import_categories(sandbox), "Expecting value")


class ValidCategoryTest(CategoryImportTest):
    """Valid custom names and threshold overrides must stay unchanged."""

    def test_shipped_categories_are_used(self):
        sandbox = self.make_sandbox()
        self.write_categories(sandbox, shipped_categories())
        probe = self.import_categories(sandbox)
        self.assert_uses_config(probe)

        shipped = shipped_categories()
        self.assertEqual(probe.values["skip"], sorted(shipped["layer_1_skip"]["tools"]))
        self.assertEqual(probe.values["shell"], sorted(shipped["layer_2_shell"]["tools"]))
        for layer, key in (("layer_2_shell", "shell_thresholds"),
                           ("layer_3_api", "api_thresholds")):
            thresholds = shipped[layer]["thresholds"]
            self.assertEqual(
                probe.values[key],
                [
                    thresholds["truncate_strings_at"],
                    thresholds["truncate_arrays_at"],
                    thresholds["max_depth"],
                ],
            )

    def test_custom_names_and_full_threshold_overrides_are_used(self):
        sandbox = self.make_sandbox()
        self.write_categories(
            sandbox,
            {
                "_meta": {"version": "9.9"},
                "layer_1_skip": {"tools": ["CustomRead", "Read"]},
                "layer_2_shell": {
                    "tools": ["CustomBash"],
                    "thresholds": {
                        "truncate_strings_at": 1024,
                        "truncate_arrays_at": 4,
                        "max_depth": 2,
                    },
                },
                "layer_3_api": {
                    "thresholds": {
                        "truncate_strings_at": 2048,
                        "truncate_arrays_at": 8,
                        "max_depth": 3,
                        # Unknown keys must not invalidate a configuration.
                        "future_knob": 7,
                    }
                },
                "_aliases": {"bash": "CustomBash"},
            },
        )
        probe = self.import_categories(sandbox)
        self.assert_uses_config(probe)
        self.assertEqual(probe.values["skip"], ["CustomRead", "Read"])
        self.assertEqual(probe.values["shell"], ["CustomBash"])
        self.assertEqual(probe.values["shell_thresholds"], [1024, 4, 2])
        self.assertEqual(probe.values["api_thresholds"], [2048, 8, 3])

    def test_partial_threshold_override_keeps_other_defaults(self):
        sandbox = self.make_sandbox()
        self.write_categories(
            sandbox,
            with_layer(shipped_categories(), "layer_2_shell", thresholds={"max_depth": 5}),
        )
        probe = self.import_categories(sandbox)
        self.assert_uses_config(probe)
        self.assertEqual(
            probe.values["shell_thresholds"],
            [DEFAULT_SHELL_THRESHOLDS[0], DEFAULT_SHELL_THRESHOLDS[1], 5],
        )
        self.assertEqual(probe.values["api_thresholds"], DEFAULT_API_THRESHOLDS)

    def test_empty_tools_lists_are_valid(self):
        sandbox = self.make_sandbox()
        payload = with_layer(shipped_categories(), "layer_1_skip", tools=[])
        payload = with_layer(payload, "layer_2_shell", tools=[])
        self.write_categories(sandbox, payload)
        probe = self.import_categories(sandbox)
        self.assert_uses_config(probe)
        self.assertEqual(probe.values["skip"], [])
        self.assertEqual(probe.values["shell"], [])
        # No shell tool is configured, so both dispatch branches are Layer 3.
        self.assertEqual(probe.values["shell_thresholds"], probe.values["api_thresholds"])


class ValidatorTest(unittest.TestCase):
    """Direct checks on the shape validator behind the loader."""

    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(COMMON_HOOKS))
        sys.modules.pop("hook_utils", None)
        import hook_utils

        cls.hook_utils = hook_utils

    @classmethod
    def tearDownClass(cls):
        sys.path.pop(0)
        sys.modules.pop("hook_utils", None)

    def test_accepts_shipped_payload_unchanged(self):
        shipped = shipped_categories()
        self.assertIs(self.hook_utils._validate_tool_categories(shipped), shipped)

    def test_rejects_malformed_shapes_with_value_error(self):
        # ValueError, not the TypeError/AttributeError the unvalidated shapes
        # used to raise out of the module import.
        for name, payload, _ in malformed_cases():
            with self.subTest(case=name):
                with self.assertRaises(ValueError):
                    self.hook_utils._validate_tool_categories(payload)

    def test_safe_categories_cover_retrieval_and_shell(self):
        safe = self.hook_utils._safe_tool_categories()
        self.assertEqual(
            sorted(safe["layer_1_skip"]["tools"]),
            sorted(self.hook_utils._FALLBACK_SKIP_TOOLS),
        )
        self.assertEqual(
            sorted(safe["layer_2_shell"]["tools"]),
            sorted(self.hook_utils._FALLBACK_SHELL_TOOLS),
        )
        for name in SAFE_SKIP_NAMES:
            self.assertIn(name, safe["layer_1_skip"]["tools"])
        for name in SAFE_SHELL_NAMES:
            self.assertIn(name, safe["layer_2_shell"]["tools"])
        # No thresholds: the module-level readers apply their own defaults.
        self.assertNotIn("thresholds", safe["layer_3_api"])


if __name__ == "__main__":
    unittest.main()
