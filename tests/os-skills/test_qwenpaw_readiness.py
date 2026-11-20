"""Service startup success requires bounded readiness and a live process."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-qwenpaw/scripts/setup.sh"


@unittest.skipUnless(os.name == "posix", "uses private POSIX shell stubs")
class QwenPawReadinessTests(unittest.TestCase):
    def run_start(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = SCRIPT.read_text(encoding="utf-8")
            fragment = source[source.index("# ── 步骤 6") :]
            stubs = r"""
pgrep() { return 1; }
nohup() { return 0; }
kill() { [ "$MODE" != dead ]; }
sleep() { :; }
curl() {
    printf '%s\n' "$*" >> "$CALLS"
    case "$MODE" in
        ready) printf 200 ;;
        not-found) printf 404 ;;
        delayed) if [ "$(wc -l < "$CALLS")" -ge 3 ]; then printf 200; else printf 503; fi ;;
        timeout) return 28 ;;
        *) printf 503 ;;
    esac
}
"""
            result = subprocess.run(
                ["bash", "-eu", "-c", stubs + fragment],
                env={
                    **os.environ,
                    "QWENPAW_DIR": str(root),
                    "CALLS": str(root / "calls"),
                    "MODE": mode,
                },
                capture_output=True,
                text=True,
                timeout=10,
            )
            calls = (root / "calls").read_text().splitlines() if (root / "calls").exists() else []
            return result, calls

    def test_nonready_server_is_not_reported_as_success(self):
        result, calls = self.run_start("unready")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertNotIn("部署完成", result.stdout)
        self.assertGreater(len(calls), 1)
        self.assertLessEqual(len(calls), 30)

    def test_connection_timeouts_have_bounded_probes_and_failure(self):
        result, calls = self.run_start("timeout")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertNotIn("部署完成", result.stdout)
        self.assertLessEqual(len(calls), 30)
        self.assertTrue(calls)
        for call in calls:
            self.assertIn("--connect-timeout", call)
            self.assertIn("--max-time", call)

    def test_dead_process_fails_before_network_probe(self):
        result, calls = self.run_start("dead")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertNotIn("部署完成", result.stdout)
        self.assertEqual(calls, [])

    def test_delayed_server_can_become_ready(self):
        result, calls = self.run_start("delayed")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("服务已就绪", result.stdout)
        self.assertEqual(len(calls), 3)

    def test_successful_http_response_still_completes(self):
        result, calls = self.run_start("ready")
        self.assertEqual(result.returncode, 0)
        self.assertIn("部署完成", result.stdout)
        self.assertEqual(len(calls), 1)

    def test_existing_not_found_root_route_is_accepted(self):
        result, calls = self.run_start("not-found")
        self.assertEqual(result.returncode, 0)
        self.assertIn("服务已就绪", result.stdout)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
