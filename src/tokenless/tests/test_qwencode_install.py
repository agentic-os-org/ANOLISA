#!/usr/bin/env python3
"""Exercise Qwen Code installation against an isolated CLI and profile."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

INSTALLER = Path(__file__).resolve().parents[1] / "adapters/tokenless/qwencode/scripts/install.sh"


class QwenCodeInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.sandbox = tempfile.TemporaryDirectory(prefix="tokenless-qwencode-")
        self.addCleanup(self.sandbox.cleanup)
        self.root = Path(self.sandbox.name)
        self.home = self.root / "home"
        self.plugin = self.root / "adapter root" / "qwencode"
        self.plugin.mkdir(parents=True)
        (self.home / ".qwen").mkdir(parents=True)
        self.manifest = self.plugin / "qwen-extension.json"
        self.template = self.plugin / "qwen-extension.json.in"
        self.installed = self.root / "installed"
        self.log = self.root / "qwen.log"
        self.settings = self.home / ".qwen/settings.json"
        self.settings.write_text(
            json.dumps(
                {
                    "theme": "dark",
                    "hooks": {
                        "PostToolUse": [
                            {"hooks": [{"name": "tokenless-response"}]},
                            {"hooks": [{"name": "user-hook"}]},
                        ]
                    },
                }
            ),
            encoding="utf-8",
        )
        self.cli = self.root / "qwen"
        self.cli.write_text(
            """#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$*" >> "$QWEN_TEST_ROOT/qwen.log"
case "${1:-} ${2:-}" in
    'extensions list')
        if [ -f "$QWEN_TEST_ROOT/installed" ]; then echo tokenless; fi
        ;;
    'extensions uninstall')
        [ "${3:-}" = tokenless ]
        rm -f "$QWEN_TEST_ROOT/installed"
        ;;
    'extensions link')
        [ -f "$3/qwen-extension.json" ]
        printf '%s\\n' "$3" > "$QWEN_TEST_ROOT/installed"
        ;;
    *) exit 2 ;;
esac
""",
            encoding="utf-8",
        )
        self.cli.chmod(0o755)

    def run_install(self, dry_run: bool) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(INSTALLER)],
            env={
                **os.environ,
                "HOME": str(self.home),
                "QWEN_BIN": str(self.cli),
                "QWEN_TEST_ROOT": str(self.root),
                "ANOLISA_ADAPTER_DIR": str(self.plugin.parent),
                "ANOLISA_DRY_RUN": "1" if dry_run else "0",
                "TOKENLESS_VERSION": "1.2.3-test",
                "ANOLISA_COMPONENT": "tokenless",
                "ANOLISA_TARGET": "qwencode",
            },
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

    def test_dry_run_preserves_existing_extension_and_settings(self) -> None:
        self.manifest.write_text('{"name":"tokenless"}', encoding="utf-8")
        self.installed.write_text("original registration", encoding="utf-8")
        before = {
            path: path.read_bytes() for path in (self.installed, self.manifest, self.settings)
        }

        result = self.run_install(dry_run=True)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DRY-RUN:", result.stdout)
        for path, contents in before.items():
            self.assertTrue(path.exists(), f"dry-run removed {path.name}")
            self.assertEqual(path.read_bytes(), contents)
        self.assertFalse(self.log.exists(), "dry-run must not invoke the Qwen CLI")

    def test_dry_run_does_not_stamp_a_template(self) -> None:
        self.template.write_text('{"name":"tokenless","version":"@VERSION@"}', encoding="utf-8")
        before = self.template.read_bytes(), self.settings.read_bytes()

        result = self.run_install(dry_run=True)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DRY-RUN:", result.stdout)
        self.assertFalse(self.manifest.exists(), "dry-run created a manifest")
        self.assertFalse(self.installed.exists())
        self.assertFalse(self.log.exists())
        self.assertEqual((self.template.read_bytes(), self.settings.read_bytes()), before)

    def test_dry_run_rejects_missing_manifest_and_template(self) -> None:
        result = self.run_install(dry_run=True)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("qwen-extension.json missing", result.stderr)
        self.assertFalse(self.log.exists())
        self.assertFalse(self.manifest.exists())

    def test_real_install_stamps_links_and_cleans_legacy_hooks(self) -> None:
        self.template.write_text('{"name":"tokenless","version":"@VERSION@"}', encoding="utf-8")

        result = self.run_install(dry_run=False)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.manifest.read_text())["version"], "1.2.3-test")
        self.assertEqual(self.installed.read_text().strip(), str(self.plugin))
        self.assertEqual(
            self.log.read_text().splitlines(),
            ["extensions list", f"extensions link {self.plugin}", "extensions list"],
        )
        settings = json.loads(self.settings.read_text())
        self.assertEqual(settings["theme"], "dark")
        self.assertEqual(settings["hooks"]["PostToolUse"], [{"hooks": [{"name": "user-hook"}]}])

    def test_real_reinstall_still_unlinks_before_linking(self) -> None:
        self.manifest.write_text('{"name":"tokenless"}', encoding="utf-8")
        self.installed.write_text("original registration", encoding="utf-8")

        result = self.run_install(dry_run=False)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed.read_text().strip(), str(self.plugin))
        self.assertEqual(
            self.log.read_text().splitlines(),
            [
                "extensions list",
                "extensions uninstall tokenless",
                f"extensions link {self.plugin}",
                "extensions list",
            ],
        )


if __name__ == "__main__":
    unittest.main()
