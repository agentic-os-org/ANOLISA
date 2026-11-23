"""Verify chat admission remains busy when native completion is uncertain."""

import importlib.util
from pathlib import Path
import unittest

SOURCE = Path(__file__).resolve().parents[4] / "aw-adapter/native/hermes/aw_reload.py"
spec = importlib.util.spec_from_file_location("aw_hermes_reload", SOURCE)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReloadTests(unittest.TestCase):
    def runtime(self, active: int = 0):
        runtime = module.Runtime({}, lambda: active)
        phases = []
        runtime.lifecycle = lambda phase, key="": phases.append(phase)
        return runtime, phases

    def test_completed_chat_releases_and_can_start_another_turn(self) -> None:
        runtime, phases = self.runtime()
        runtime.ready()
        self.assertEqual(runtime.chat(lambda: "reply"), "reply")
        self.assertEqual(runtime.chat(lambda: "next"), "next")
        self.assertEqual(phases, ["ready", "request_start", "request_end", "request_start", "request_end"])

    def test_canceled_or_unfinished_tools_leave_update_gate_busy(self) -> None:
        for result, active in [(None, 0), ("reply", 1)]:
            with self.subTest(result=result, active=active):
                runtime, phases = self.runtime(active)
                runtime.chat(lambda: result)
                self.assertEqual(phases, ["request_start"])
                with self.assertRaisesRegex(RuntimeError, "restart required"):
                    runtime.chat(lambda: "next")

    def test_failed_native_chat_cannot_release_admission(self) -> None:
        runtime, phases = self.runtime()
        def fail():
            raise ValueError("native failure")
        with self.assertRaises(ValueError):
            runtime.chat(fail)
        self.assertEqual(phases, ["request_start"])
        self.assertTrue(runtime.failed)

    def test_submission_and_chat_share_one_admission(self) -> None:
        runtime, phases = self.runtime()
        runtime.admit_input()
        self.assertEqual(runtime.chat(lambda: "reply"), "reply")
        self.assertEqual(phases, ["request_start", "request_end"])

    def test_denied_input_releases_without_dispatch(self) -> None:
        runtime, phases = self.runtime()
        runtime.admit_input()
        runtime.cancel_input()
        self.assertFalse(runtime.busy)
        self.assertEqual(phases, ["request_start", "request_end"])

    def test_nested_chat_does_not_open_another_request(self) -> None:
        runtime, phases = self.runtime()
        with self.assertRaisesRegex(RuntimeError, "restart required"):
            runtime.chat(lambda: runtime.chat(lambda: "nested"))
        self.assertEqual(phases, ["request_start"])


if __name__ == "__main__":
    unittest.main()
