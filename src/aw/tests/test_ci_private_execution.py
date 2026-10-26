"""Validate the public Python lease contract without native SDK dependencies."""

import importlib.util
from pathlib import Path
import sys
import unittest

SOURCE = Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/qwenpaw/aw_private.py"
SPEC = importlib.util.spec_from_file_location("aw_private_contract_test", SOURCE)
PRIVATE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PRIVATE
SPEC.loader.exec_module(PRIVATE)


class PrivateExecutionContractTests(unittest.TestCase):
    def test_versioned_views_are_copied_and_secret_repr_is_opaque(self):
        environment = {"SAFE": "public"}
        lease = PRIVATE.PrivateExecution(environment, environment, lambda value: value,
                                         {"TASK_KEY": "fixture-secret"})
        PRIVATE.validate_execution(lease)
        environment["SAFE"] = "changed"
        token = PRIVATE.current_execution.set(lease)
        try:
            self.assertEqual(PRIVATE.execution_environment(), {
                "SAFE": "public", "TASK_KEY": "fixture-secret"})
            self.assertEqual(PRIVATE.hook_environment(), {"SAFE": "public"})
            PRIVATE.execution_environment()["TASK_KEY"] = "changed"
            self.assertEqual(lease.secrets["TASK_KEY"], "fixture-secret")
            self.assertNotIn("fixture-secret", repr(lease))
        finally:
            PRIVATE.current_execution.reset(token)
            PRIVATE.clear_execution(lease)
        self.assertEqual(lease.secrets, {})

    def test_unsupported_version_or_invalid_environment_is_rejected(self):
        for change in ({"version": "unsupported"}, {"environment": {"INVALID=KEY": "value"}},
                       {"secrets": {"TASK_KEY": 3}}, {"redact": None}):
            lease = PRIVATE.PrivateExecution({}, {}, lambda value: value)
            for name, value in change.items():
                setattr(lease, name, value)
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                PRIVATE.validate_execution(lease)

    def test_hook_view_rejects_declared_secret_keys_and_values(self):
        for hook in ({"TASK_KEY": "different"}, {"SAFE": "prefix-fixture-secret"}):
            lease = PRIVATE.PrivateExecution({}, hook, lambda value: value,
                                             {"TASK_KEY": "fixture-secret"})
            with self.assertRaisesRegex(RuntimeError, "declared secret"):
                PRIVATE.validate_execution(lease)

    def test_helpers_require_an_owned_execution(self):
        for helper in (PRIVATE.execution_environment, PRIVATE.hook_environment):
            with self.assertRaisesRegex(RuntimeError, "no task-private"):
                helper()


if __name__ == "__main__":
    unittest.main()
