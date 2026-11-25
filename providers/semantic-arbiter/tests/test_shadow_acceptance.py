"""Offline parser/config regressions only; synthetic rows are NOT acceptance evidence."""

import argparse
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import shadow_acceptance as harness
from shadow_capture import redact, safe_path, write_json, write_text


class OfflineAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="shadow-parser-unit-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "records").mkdir()
        (self.root / "transport").mkdir()

    def args(self) -> argparse.Namespace:
        return argparse.Namespace(
            mode="prepare", output_parent=self.root, case=["nogit-overwrite"],
            aw_bin=Path("/usr/bin/false"), qoder_bin=Path("/usr/bin/false"),
            provider_bin=Path("/usr/bin/false"), qoder_model="NONNETWORK",
            arbiter_model="NONNETWORK", endpoint="https://nonnetwork.invalid/infer",
            disposable_runtime=False,
        )

    def test_prepare_never_launches_or_reads_environment(self) -> None:
        # mkdtemp has explicit directories, so it needs no credential/environment lookup.
        class NoEnvironment(dict):
            def get(self, key: str, default: object = None) -> object:
                raise AssertionError("prepare accessed environment")

        runtime = self.root / "short-runtime"
        artifact = self.root / "prepared"
        runtime.mkdir()
        artifact.mkdir()
        with patch.object(harness.os, "environ", NoEnvironment()), patch.object(
            harness.subprocess, "Popen", side_effect=AssertionError("process launch")
        ), patch.object(harness.tempfile, "mkdtemp", side_effect=[str(artifact), str(runtime)]):
            root = harness.prepare(self.args())
        config = json.loads((root / "nogit-overwrite/aw.yaml").read_text())
        provider = config["spec"]["providers"]["semantic"]["config"]
        self.assertEqual(provider["resources"], {})
        self.assertEqual(provider["task"], {"allowed_resource_ids": [], "preserve_resource_ids": []})
        self.assertEqual(set(config["spec"]["events"]), {"input.submit", "tool.before", "tool.after"})
        self.assertFalse((root / "nogit-overwrite/workspace/.git").exists())
        summary = harness.summarize(root)
        self.assertEqual(summary["cases"][0]["status"], "not_exercised")
        self.assertFalse(summary["all_cases_native_model_exercised"])

    def request(self) -> dict:
        return {"request_id": "parser-only-request", "input_digest": "parser-only-digest",
                "config_revision": "parser-only-revision", "event": {
                    "name": "tool.before", "agent": {"adapter": "qoder", "binding_id": "b", "instance_id": "i"},
                    "session_id": "s", "tool": {"call_id": "c", "native_name": "Write", "input": {
                        "file_path": "greeting.txt", "content": "hello"}}, "native": {}}}

    def transport(self, request: dict, role: str) -> dict:
        return {"synthetic_parser_fixture": True, "role": role, "request": request,
                "returncode": 0, "response": {"api_version": "aw-provider/v1alpha1",
                    "request_id": request["request_id"], "status": "ok",
                    "input_digest": request["input_digest"], "effects": []}}

    def fixture(self, *, parsed: bool = True, receipt: bool = True,
                effects: bool = False, failure: bool = False,
                used: bool = True, duplicate: bool = False) -> dict:
        request = self.request()
        evaluation = {"model_called": True, "judgments": [
            {"unit": "task_scope", "route": "model_fallback", "policy_id": None,
             "baseline_reason": "missing_evidence", "model_used": used,
             "probability": {"parser_fixture": True} if used else None}], "answers": []}
        data = {"input": {"aw_request": request}, "task": {"origin": "parser_fixture"},
                "evidence": {}, "evaluation": {"local_context": {
                    "signals": [{"kind": "test_change"}], "gaps": ["parser_gap"]}},
                "model_request": {"parser_fixture": True},
                "model_result": {"status": "parsed" if parsed else "error"},
                "result": {"code": "shadow_evaluated", "evaluation": evaluation}}
        lines = [{"call": "parser-only-call", "stage": stage, "data": value} for stage, value in data.items()]
        if duplicate:
            lines.append(lines[-1])
        write_text(self.root / "records/unbound.jsonl", "\n".join(json.dumps(row) for row in lines) + "\n")
        transport = self.transport(request, "semantic_transport")
        if effects:
            transport["response"]["effects"] = [{"type": "block"}]
        write_json(self.root / "transport" / ("a" * 32 + ".json"), transport)
        if receipt:
            after = copy.deepcopy(request)
            after["event"]["name"] = "tool.after"
            after["event"]["native"]["hook_event_name"] = "PostToolUseFailure" if failure else "PostToolUse"
            after["event"]["tool"]["result"] = {"is_error": failure, "content": "parser fixture"}
            after["event"]["native"]["tool_response"] = after["event"]["tool"]["result"]
            write_json(self.root / "transport" / ("b" * 32 + ".json"), self.transport(after, "native_receipt"))
        return harness.summarize_case(self.root)["calls"][0]

    def test_complete_observed_chain_retains_actual_stages(self) -> None:
        call = self.fixture()
        self.assertTrue(call["model_shadow_verified"])
        self.assertEqual(len(call["stages"]), 7)
        self.assertEqual(call["judgments"][0]["baseline_reason"], "missing_evidence")
        self.assertEqual(call["local_gaps"], ["parser_gap"])
        # No valid run manifest is supplied: these synthetic parser fixtures cannot
        # be summarized through the acceptance CLI as if they were a native run.
        self.assertFalse((self.root / "manifest.json").exists())

    def test_model_called_but_error_is_not_success(self) -> None:
        self.assertFalse(self.fixture(parsed=False)["model_shadow_verified"])

    def test_parsed_but_unused_model_is_not_success(self) -> None:
        self.assertFalse(self.fixture(used=False)["model_shadow_verified"])

    def test_before_without_native_completion_is_not_exercised(self) -> None:
        call = self.fixture(receipt=False)
        self.assertFalse(call["native_tool_called"])
        self.assertFalse(call["shadow_exercised"])

    def test_failure_callback_is_not_execution_proof(self) -> None:
        self.assertFalse(self.fixture(failure=True)["native_tool_called"])

    def test_posttooluse_error_and_interrupt_are_not_success(self) -> None:
        for result in ({"exitCode": 1}, {"isError": True}, {"is_error": True},
                       {"interrupted": True}, {"signal": "SIGTERM"}, {"signal": 15}):
            event = {"native": {"hook_event_name": "PostToolUse", "tool_response": result},
                     "tool": {"result": result}}
            with self.subTest(result=result):
                self.assertFalse(harness.native_result_succeeded(event))

    def test_completion_requires_actual_native_result(self) -> None:
        event = {"native": {"hook_event_name": "PostToolUse"}, "tool": {"result": {"exitCode": 0}}}
        self.assertFalse(harness.native_result_succeeded(event))
        event["native"]["tool_response"] = event["tool"]["result"]
        self.assertTrue(harness.native_result_succeeded(event))

    def test_nonempty_effects_fail_shadow_acceptance(self) -> None:
        self.assertFalse(self.fixture(effects=True)["model_shadow_verified"])

    def test_duplicate_stages_fail_completeness(self) -> None:
        self.assertFalse(self.fixture(duplicate=True)["model_shadow_verified"])

    def test_protocol_identity_must_match(self) -> None:
        transport = self.transport(self.request(), "semantic_transport")
        transport["response"]["input_digest"] = "wrong"
        self.assertFalse(harness.protocol_observed(transport))

    def test_endpoint_rejects_credentials_and_queries(self) -> None:
        for endpoint in ("http://example.invalid/infer", "https://key@example.invalid/infer",
                         "https://example.invalid/infer?api_key=placeholder",
                         "https://example.invalid/token/placeholder", "https://example.invalid/%61"):
            args = self.args()
            args.endpoint = endpoint
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                harness.metadata(args)

    def test_sensitive_paths_and_links_are_rejected(self) -> None:
        for name in ("credentials.env", "model.json"):
            with self.assertRaises(ValueError):
                safe_path(self.root / name)
        (self.root / "link").symlink_to(self.root / "records", target_is_directory=True)
        with self.assertRaises(ValueError):
            safe_path(self.root / "link/calls.jsonl")

    def test_existing_output_is_never_overwritten(self) -> None:
        path = self.root / "existing.txt"
        write_text(path, "original")
        with self.assertRaises(FileExistsError):
            write_text(path, "changed")
        self.assertEqual(path.read_text(), "original")

    def test_redaction_uses_only_explicit_values(self) -> None:
        self.assertEqual(redact("Authorization: Bearer parser-dummy", ()), "Authorization: Bearer [REDACTED]")
        self.assertNotIn("parser-key", redact("parser-key", ("parser-key",)))

    def test_run_prerequisite_gate_does_not_launch(self) -> None:
        with patch.object(harness.subprocess, "Popen", side_effect=AssertionError("process launch")):
            with self.assertRaises(ValueError):
                harness.prereqs(self.args())

    def test_missing_credentials_fail_without_launch(self) -> None:
        args = self.args()
        args.disposable_runtime = True
        args.endpoint = "https://example.test/infer"
        with patch.object(harness.os, "environ", {}), patch.object(
            harness.subprocess, "Popen", side_effect=AssertionError("process launch")
        ):
            with self.assertRaisesRegex(ValueError, "credentials"):
                harness.prereqs(args)

    def test_summarizer_never_reads_environment_or_launches(self) -> None:
        class NoEnvironment(dict):
            def get(self, key: str, default: object = None) -> object:
                raise AssertionError("summarizer accessed environment")

        with patch.object(harness.os, "environ", NoEnvironment()), patch.object(
            harness.subprocess, "Popen", side_effect=AssertionError("process launch")
        ):
            report = harness.summarize_case(self.root)
        self.assertEqual(report["counts"]["actual_provider_calls"], 0)
        self.assertEqual(report["status"], "not_exercised")


if __name__ == "__main__":
    unittest.main()
