"""Tests for DingTalk notification rendering and response validation."""

import importlib.util
import json
import unittest
import urllib.error
import urllib.parse
from pathlib import Path
from unittest.mock import MagicMock, patch

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
            MODULE.validate_response('{"errcode": 310000, "errmsg": "sign not match"}')

    def test_rejects_invalid_json(self) -> None:
        with self.assertRaisesRegex(ValueError, "valid JSON"):
            MODULE.validate_response("service unavailable")

    def test_missing_or_nonnumeric_status_has_an_unknown_delivery_outcome(self) -> None:
        for raw in ["{}", '{"errcode":false}', '{"errcode":"0"}']:
            with self.assertRaisesRegex(ValueError, "numeric errcode") as error:
                MODULE.validate_response(raw)
            self.assertNotIsInstance(error.exception, MODULE.DeliveryRejected)

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
            r"**Title**: valid\]\(https://evil.example\)\[text " r"\<https://second.example\>",
            text,
        )
        self.assertNotIn("](https://evil.example)", text)
        self.assertNotIn("<https://second.example>", text)

    def test_analysis_card_escapes_model_text_and_keeps_controller_links(self) -> None:
        text = MODULE.build_analysis_markdown(
            issue_number=12,
            issue_title="@all [title](https://evil.example)",
            issue_url="https://github.com/agentic-os-org/ANOLISA/issues/12",
            component_label="component:cosh-ng",
            priority="priority:p1",
            conclusion="<script>\n" + "x" * 1000,
            impact="bounded impact",
            next_step="Verify the reported precondition.",
            owners="triager",
            report_url="https://github.com/agentic-os-org/ANOLISA/actions/runs/1234",
            needs_human=True,
        )
        self.assertIn("needs review", text)
        self.assertIn("[Report](https://github.com/agentic-os-org/ANOLISA/actions/runs/1234)", text)
        self.assertNotIn("@all", text)
        self.assertNotIn("](https://evil.example)", text)
        self.assertNotIn("x" * 241, text)
        self.assertIn("Source analysis", text)

    def test_signed_transport_checks_the_response_and_has_a_network_deadline(self) -> None:
        response = MagicMock()
        response.__enter__.return_value.read.return_value = b'{"errcode":0}'
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response) as send:
            MODULE.send_markdown(
                webhook="https://example.invalid/robot?access_token=private",
                secret="signing-key",
                title="Issue",
                text="Result",
            )
        request = send.call_args.args[0]
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(request.full_url).query)
        self.assertEqual(query["access_token"], ["private"])
        self.assertIn("timestamp", query)
        self.assertIn("sign", query)
        self.assertEqual(send.call_args.kwargs["timeout"], 30)
        self.assertEqual(json.loads(request.data)["markdown"]["text"], "Result")

    def test_transport_distinguishes_rejection_from_an_unknown_outcome(self) -> None:
        response = MagicMock()
        response.__enter__.return_value.read.return_value = b'{"errcode":310000}'
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response):
            with self.assertRaises(MODULE.DeliveryRejected):
                MODULE.send_markdown(
                    webhook="https://example.invalid", secret="", title="Issue", text="Result"
                )
        with patch.object(MODULE.urllib.request, "urlopen", side_effect=TimeoutError):
            with self.assertRaisesRegex(RuntimeError, "unknown"):
                MODULE.send_markdown(
                    webhook="https://example.invalid", secret="", title="Issue", text="Result"
                )
        failure = urllib.error.HTTPError("https://example.invalid", 503, "unavailable", {}, None)
        with patch.object(MODULE.urllib.request, "urlopen", side_effect=failure):
            with self.assertRaisesRegex(RuntimeError, "unknown"):
                MODULE.send_markdown(
                    webhook="https://example.invalid", secret="", title="Issue", text="Result"
                )


if __name__ == "__main__":
    unittest.main()
