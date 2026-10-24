"""AW's backend consumer gate must run real contracts and reject false success."""

import json
import os
import re
from unittest.mock import patch

from test_ci_checks import AW, GateFixture, gate
import check_tokenless


class TokenlessTests(GateFixture):
    def test_artifact_upload_requires_an_executed_consumer_step(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-ci.yml").read_text()
        upload = workflow.split("      - uses: actions/upload-artifact@v4\n", 1)[1]
        upload = upload.split("      - name:", 1)[0]
        condition = re.search(r"^\s+if: (.+)$", upload, re.MULTILINE).group(1)
        self.assertIn("always()", condition)
        self.assertIn("if-no-files-found: error", upload)
        for outcome in ("", "skipped", "success", "failure", "cancelled"):
            expression = condition.replace("always()", "True").replace("&&", "and")
            expression = expression.replace("steps.tokenless.outcome", repr(outcome))
            with self.subTest(outcome=outcome):
                self.assertEqual(
                    eval(expression, {"__builtins__": {}}),
                    outcome in {"success", "failure", "cancelled"},
                )

    def test_public_backend_and_installer_changes_select_both_checks(self):
        for path in (
            "src/tokenless/crates/tokenless-runtime/src/entry/rtk.rs",
            "src/tokenless/crates/tokenless-protocol/src/lib.rs",
            "src/tokenless/crates/tokenless-cli/src/main.rs",
            "src/tokenless/third_party/patches/rtk-tokenless-stats.patch",
            "src/tokenless/scripts/setup-rtk.sh", "src/tokenless/Cargo.lock",
            "providers/tokenless/src/pretool/retry.rs",
            "src/aw/crates/aw-package/src/package/rtk.rs",
            "src/aw/crates/aw-package/src/package.rs",
            "src/aw/crates/aw-package/src/manifest.rs",
            "src/aw/crates/aw-package/src/bin/aw-build.rs",
            "src/aw/crates/aw-package/src/bin/support/arguments.rs",
            "src/aw/crates/aw-provider/schemas/request-v1alpha1.schema.json",
            "src/aw/crates/aw-provider/src/protocol.rs",
            "src/aw/crates/aw-exec/src/linux/child.rs",
            "src/aw/crates/aw-exec/Cargo.toml",
            "src/tokenless/crates/tokenless-runtime/Cargo.toml",
            "src/tokenless/rust-toolchain.toml", "src/aw/rust-toolchain.toml",
            "providers/tokenless/tests/pretool.rs", "providers/tokenless/Cargo.lock",
            ".github/workflows/aw-ci.yml",
        ):
            event = {"pull_request": {"base": {"sha": "a" * 40}, "head": {"sha": "b" * 40}}}
            with self.subTest(path=path), patch.object(gate, "git", return_value=path):
                self.assertTrue(gate.scope("pull_request", event, "c" * 40))
                self.assertTrue(gate.scope("pull_request", event, "c" * 40, compatibility=True))
        for path in ("src/tokenless/README.md", "src/tokenless/adapters/other.js", "src/agent-memory/src/lib.rs"):
            with self.subTest(path=path), patch.object(gate, "git", return_value=path):
                self.assertFalse(gate.scope("pull_request", event, "c" * 40))
                self.assertFalse(gate.scope("pull_request", event, "c" * 40, compatibility=True))
        workflow = (AW.parents[1] / ".github/workflows/aw-ci.yml").read_text()
        for event in ("push", "pull_request"):
            block = re.split(r"\n  \w", workflow.split(f"  {event}:\n", 1)[1], maxsplit=1)[0]
            # Both entry filters admit backend changes; check.py narrows public inputs.
            self.assertIn("'src/tokenless/**'", block)
        self.assertIn("TOKENLESS_TESTED_SHA: ${{ needs.tokenless.outputs.tested_sha }}", workflow)

    def test_unrelated_aw_changes_run_fast_contracts_without_real_backend_builds(self):
        for path in (
            "src/aw/crates/aw-service/src/runtime.rs",
            "src/aw/crates/aw-adapter/native/claude/sdk.py",
            "src/aw/crates/aw-cli/src/commands/inspect.rs",
            "src/aw/crates/aw-package/src/install.rs",
            "src/aw/crates/aw-package/src/transaction.rs",
            "src/aw/crates/aw-package/tests/management.rs",
            "src/aw/crates/aw-provider/tests/protocol.rs",
            "src/aw/crates/aw-provider/examples/policy.rs",
            "src/aw/crates/aw-exec/tests/execution.rs",
            "src/aw/CONTRIBUTING.md", "src/aw/scripts/check_native.py",
            "providers/tokenless/README.md", ".github/workflows/aw-preview.yml",
        ):
            event = {"pull_request": {"base": {"sha": "a" * 40}, "head": {"sha": "b" * 40}}}
            with self.subTest(path=path), patch.object(gate, "git", return_value=path):
                self.assertTrue(gate.scope("pull_request", event, "c" * 40))
                self.assertFalse(gate.scope("pull_request", event, "c" * 40, compatibility=True))
        for path in ("src/tokenless/crates/tokenless-runtime/tests/runtime.rs",
                     "src/tokenless/crates/tokenless-cli/README.md"):
            with self.subTest(path=path), patch.object(gate, "git", return_value=path):
                self.assertFalse(gate.scope("pull_request", event, "c" * 40, compatibility=True))

    def test_real_backend_selection_keeps_mixed_changes_and_forced_acceptance(self):
        paths = "src/aw/README.md\0src/aw/crates/aw-package/src/package/rtk.rs\0"
        event = {"pull_request": {"base": {"sha": "a" * 40}, "head": {"sha": "b" * 40}}}
        with patch.object(gate, "git", return_value=paths):
            self.assertTrue(gate.scope("pull_request", event, "c" * 40, compatibility=True))
        self.assertTrue(gate.scope("workflow_dispatch", {}, "a" * 40, compatibility=True))
        push = {"before": "b" * 40, "after": "a" * 40, "forced": True}
        self.assertTrue(gate.scope("push", push, "a" * 40, compatibility=True))

    def test_required_check_rejects_missing_skipped_failed_or_stale_consumer_results(self):
        env = {"GITHUB_SHA": "a" * 40, "CANDIDATE_SHA": "a" * 40,
               "SCOPE_RESULT": "success", "SELECTED": "true", "CHECKS_RESULT": "success",
               "TESTED_SHA": "a" * 40, "TOKENLESS_SELECTED": "true",
               "TOKENLESS_RESULT": "success", "TOKENLESS_TESTED_SHA": "a" * 40}
        gate.required(env)
        for key, value in (("TOKENLESS_RESULT", None), ("TOKENLESS_RESULT", "skipped"),
                           ("TOKENLESS_RESULT", "cancelled"), ("TOKENLESS_RESULT", "failure"),
                           ("TOKENLESS_TESTED_SHA", "b" * 40), ("TOKENLESS_SELECTED", None)):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                gate.required({**env, key: value})

    def test_result_rejects_empty_ignored_and_renamed_rust_tests(self):
        name = check_tokenless.CASES["managed_rtk"]
        good = f"test {name} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 3 filtered out;\n"
        check_tokenless.verify_result(good, name)
        for bad in ("", good.replace(name, "different"),
                    good.replace("1 passed", "0 passed"),
                    good.replace("0 ignored", "1 ignored"),
                    good.replace("... ok", "... ignored")):
            with self.assertRaises(ValueError):
                check_tokenless.verify_result(bad, name)

    def test_consumer_build_and_tests_propagate_failure_and_bind_the_candidate(self):
        aw = self.root / "src/aw"
        for file in (self.root / "src/tokenless/target/release/tokenless", aw / "target/release/rtk"):
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(b"fixture binary")
        for mode in ("pass", "empty", "failure"):
            calls = []

            def run(commands, argv, cwd, timeout):
                calls.append(argv)
                self.assertEqual(commands.environment["TOKENLESS_STATS_ENABLED"], "0")
                self.assertEqual(commands.environment["TOKENLESS_SLS_ENABLED"], "0")
                if argv[1] == "run":
                    self.assertIn("tokenless", argv)
                    return ""
                if mode == "failure":
                    raise RuntimeError("backend test failed")
                if mode == "empty":
                    return "test result: ok. 0 passed; 0 failed; 0 ignored;"
                return f"test {argv[5]} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;"

            output = self.root / mode
            actions_output = self.root / f"{mode}-actions-output"
            with patch.object(check_tokenless, "AW", aw), patch.object(check_tokenless, "REPO", self.root), \
                    patch.object(check_tokenless, "candidate", return_value="a" * 40), \
                    patch.dict(os.environ, {"GITHUB_OUTPUT": str(actions_output)}), \
                    patch.object(check_tokenless.Commands, "run", autospec=True, side_effect=run):
                if mode == "pass":
                    check_tokenless.check(output)
                else:
                    with self.assertRaises((RuntimeError, ValueError)):
                        check_tokenless.check(output)
            report = json.loads((output / "result.json").read_text())
            self.assertEqual(report["passed"], mode == "pass")
            self.assertEqual(report["candidate_sha"], "a" * 40)
            self.assertEqual(len(report["cases"]), 4 if mode == "pass" else 0)
            if mode == "pass":
                self.assertEqual(actions_output.read_text(), "tested_sha=" + "a" * 40 + "\n")
            else:
                self.assertFalse(actions_output.exists())
