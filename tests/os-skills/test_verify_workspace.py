"""Compilation checks must never write fixtures into the caller's directory."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/devops/kernel-dev/scripts/verify-env.sh"
)


@unittest.skipUnless(os.name == "posix", "uses private POSIX shell fixtures")
class VerifyWorkspaceTests(unittest.TestCase):
    def check(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caller = root / "caller"
            caller.mkdir()
            workspace = root / "workspace"
            workspace.mkdir()
            for name in ("Makefile", "test_module.c"):
                (caller / name).write_text(f"personal {name}\n", encoding="utf-8")
            source = SCRIPT.read_text(encoding="utf-8")
            fragment = source[source.index("# Step 8:") : source.index("# Summary")]
            harness = r"""
PASS=0; FAIL=0
check_pass() { echo "PASS $1"; PASS=$((PASS+1)); }
check_fail() { echo "FAIL $1"; FAIL=$((FAIL+1)); }
mktemp() {
    case "$MODE" in
        empty) return 1 ;;
        failed-output) printf '%s\n' "$PRIVATE_DIR"; return 1 ;;
        missing) printf '%s\n' "$PRIVATE_DIR/absent" ;;
        *) printf '%s\n' "$PRIVATE_DIR" ;;
    esac
}
cd() {
    if [ "$MODE" = "cd-fails" ] && [ "${1:-}" = "$PRIVATE_DIR" ]; then return 1; fi
    builtin cd "$@"
}
make() {
    printf '%s\n' "$PWD" >> "$CALLS"
    case "$MODE" in
        compile-fails) return 2 ;;
        no-output) return 0 ;;
        *) touch test_module.ko ;;
    esac
}
"""
            result = subprocess.run(
                ["bash", "-c", harness + fragment + '\nprintf "counts %s %s\\n" "$PASS" "$FAIL"'],
                cwd=caller,
                env={
                    **os.environ,
                    "MODE": mode,
                    "PRIVATE_DIR": str(workspace),
                    "CALLS": str(root / "calls"),
                },
                capture_output=True,
                text=True,
                timeout=10,
            )
            for name in ("Makefile", "test_module.c"):
                self.assertEqual(
                    (caller / name).read_text(encoding="utf-8"),
                    f"personal {name}\n",
                    result.stdout + result.stderr,
                )
            calls = (root / "calls").read_text().splitlines() if (root / "calls").exists() else []
            if mode in ("empty", "failed-output", "missing", "cd-fails"):
                self.assertEqual(calls, [], result.stdout + result.stderr)
                self.assertIn("counts 0 1", result.stdout)
            elif mode in ("compile-fails", "no-output"):
                self.assertEqual(calls, [str(workspace)])
                self.assertIn("counts 0 1", result.stdout)
                self.assertFalse(workspace.exists())
            else:
                self.assertEqual(calls, [str(workspace)])
                self.assertIn("counts 1 0", result.stdout)
                self.assertFalse(workspace.exists())

    def test_failed_temp_creation_keeps_caller_files(self):
        self.check("empty")

    def test_nonzero_mktemp_cannot_supply_a_workspace(self):
        self.check("failed-output")

    def test_missing_temp_directory_is_rejected(self):
        self.check("missing")

    def test_failed_directory_change_keeps_caller_files(self):
        self.check("cd-fails")

    def test_successful_check_cleans_only_its_workspace(self):
        self.check("success")

    def test_failed_build_is_reported_and_cleaned(self):
        self.check("compile-fails")

    def test_build_without_artifact_is_reported_and_cleaned(self):
        self.check("no-output")


if __name__ == "__main__":
    unittest.main()
