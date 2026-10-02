"""Tests for DingTalk notification rendering and response validation."""

import importlib.util
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("dingtalk_notification.py")
SPEC = importlib.util.spec_from_file_location("dingtalk_notification", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("Unable to load DingTalk notification helpers")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DingTalkNotificationTest(unittest.TestCase):
    """Verify message safety and API-level delivery handling."""

    def test_accepts_zero_errcode(self) -> None:
        MODULE.validate_response('{"errcode": 0, "errmsg": "ok"}')

    def test_rejects_nonzero_errcode(self) -> None:
        with self.assertRaisesRegex(ValueError, "errcode=310000"):
            MODULE.validate_response(
                '{"errcode": 310000, "errmsg": "sign not match"}'
            )

    def test_rejects_invalid_json(self) -> None:
        with self.assertRaisesRegex(ValueError, "valid JSON"):
            MODULE.validate_response("service unavailable")

    def test_escapes_issue_title_outside_the_fixed_link(self) -> None:
        text = MODULE.build_issue_markdown(
            issue_number="2262",
            issue_title="valid](https://evil.example)[text <https://second.example>",
            issue_url="https://github.com/alibaba/anolisa/issues/2262",
            component_label="component:sight",
            author="external-user",
            owners="owner",
            assigned="none",
        )
        self.assertIn(
            "**Issue**: [#2262](https://github.com/alibaba/anolisa/issues/2262)",
            text,
        )
        self.assertIn(
            r"**Title**: valid\]\(https://evil.example\)\[text "
            r"\<https://second.example\>",
            text,
        )
        self.assertNotIn("](https://evil.example)", text)
        self.assertNotIn("<https://second.example>", text)


if __name__ == "__main__":
    unittest.main()
