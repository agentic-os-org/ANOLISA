"""The shipped tool-category map must load whole under any process locale.

``tool_categories.json`` is UTF-8 and carries a non-ASCII em dash, so reading it
with the ambient locale raised ``UnicodeDecodeError`` on an ASCII host. The
loader's ``except`` swallowed that as a malformed file, so nothing crashed: the
map silently degraded to the reduced hardcoded sets and the diagnostic went to
stderr, where hooks never look. This pins the read to UTF-8 and asserts the
shipped map is what the module actually consumes.
"""

import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parents[1] / "adapters" / "tokenless" / "common" / "hooks"
CATEGORIES = HOOKS_DIR / "tool_categories.json"

# PYTHONCOERCECLOCALE/PYTHONUTF8 disable PEP 538 and PEP 540; otherwise the
# interpreter reconfigures an ASCII locale to UTF-8 and the fixture is vacuous.
ASCII_LOCALE = {
    "LC_ALL": "C",
    "LANG": "C",
    "PYTHONCOERCECLOCALE": "0",
    "PYTHONUTF8": "0",
}

PROBE = """
import json, sys
sys.path.insert(0, sys.argv[1])
import hook_utils
print(json.dumps({
    "layers": sorted(hook_utils._tool_categories),
    "skip": sorted(hook_utils.SKIP_TOOLS),
    "shell": sorted(hook_utils.SHELL_TOOLS),
    "bash": list(hook_utils.get_thresholds("Bash")),
    "api": list(hook_utils.get_thresholds("some_api_tool")),
}))
"""


class ToolCategoryEncodingTest(unittest.TestCase):
    def probe(self, env_overrides):
        env = {
            key: value
            for key, value in os.environ.items()
            # PYTHONIOENCODING forces UTF-8 stdio regardless of the locale and
            # would hide exactly the failure this fixture is about.
            if key not in ("PYTHONIOENCODING", "PYTHONPATH")
        }
        env.update(env_overrides)
        return subprocess.run(
            [sys.executable, "-c", PROBE, str(HOOKS_DIR)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=env,
        )

    def test_categories_load_whole_under_a_non_utf8_locale(self):
        proc = self.probe(ASCII_LOCALE)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        shipped = json.loads(CATEGORIES.read_text(encoding="utf-8"))
        payload = json.loads(proc.stdout)

        # `_meta` and `_aliases` exist only in the file; the hardcoded fallback
        # is built from the two tool lists alone, so their presence proves the
        # read did not fall back.
        self.assertEqual(payload["layers"], sorted(shipped))
        self.assertEqual(payload["skip"], sorted(shipped["layer_1_skip"]["tools"]))
        self.assertEqual(payload["shell"], sorted(shipped["layer_2_shell"]["tools"]))
        self.assertEqual(
            payload["bash"],
            [
                shipped["layer_2_shell"]["thresholds"]["truncate_strings_at"],
                shipped["layer_2_shell"]["thresholds"]["truncate_arrays_at"],
                shipped["layer_2_shell"]["thresholds"]["max_depth"],
            ],
        )
        self.assertEqual(
            payload["api"],
            [
                shipped["layer_3_api"]["thresholds"]["truncate_strings_at"],
                shipped["layer_3_api"]["thresholds"]["truncate_arrays_at"],
                shipped["layer_3_api"]["thresholds"]["max_depth"],
            ],
        )
        self.assertNotIn("Failed to load tool_categories.json", proc.stderr)


if __name__ == "__main__":
    unittest.main()
