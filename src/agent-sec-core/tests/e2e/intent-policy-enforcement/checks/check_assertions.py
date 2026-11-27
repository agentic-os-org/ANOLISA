"""回归验证端到端入口、场景输入和验收证据，无需启动服务。"""

import hashlib
import io
import json
import os
import secrets
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack, contextmanager, redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

import full
import gateway as gateway_entry
import intent
import sandbox
import sight
from support import common, gateway


class SuiteTests(unittest.TestCase):
    def test_fixture_directory_contains_scenario_inputs(self):
        root = Path(intent.__file__).resolve().parent
        self.assertEqual(common.FIXTURES, root / "fixtures")
        self.assertEqual(
            {path.name for path in common.FIXTURES.glob("*.cedar*")},
            {"task.cedar", "gateway.cedar.in"},
        )
        for name in ("task.txt", "task.cedar", "gateway.cedar.in"):
            with self.subTest(name=name):
                self.assertTrue((common.FIXTURES / name).is_file())
                self.assertFalse((root / name).exists())

    def test_entry_help_works_outside_the_repository(self):
        root = Path(intent.__file__).resolve().parent
        env = os.environ.copy()
        env.pop("PYTHONPATH", None)
        with tempfile.TemporaryDirectory() as directory:
            for name in (
                "intent.py",
                "sandbox.py",
                "sight.py",
                "gateway.py",
                "full.py",
            ):
                with self.subTest(name=name):
                    result = subprocess.run(
                        [sys.executable, "-B", str(root / name), "--help"],
                        cwd=directory,
                        env=env,
                        capture_output=True,
                        text=True,
                        check=True,
                        timeout=10,
                    )
                    self.assertIn("--check", result.stdout)

    def test_task_generation_loads_the_fixture(self):
        cedar = (common.FIXTURES / "task.cedar").read_bytes()
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)
            row = report / "workspace"
            with (
                redirect_stdout(output),
                patch.object(
                    intent.subprocess, "check_output", return_value=cedar
                ) as call,
            ):
                draft = intent.generate_policy(row, report)
            task = (
                (common.FIXTURES / "task.txt")
                .read_text()
                .replace("{workspace}", str(row))
            )
            command = call.call_args.args[0]
            self.assertEqual(command[command.index("--task") + 1], task)
            self.assertEqual((report / "task.txt").read_text(), task)
            self.assertEqual(draft.read_bytes(), cedar)
        text = output.getvalue()
        self.assertIn("── 任务策略生成 ──", text)
        self.assertIn("task.txt → daemon 意图服务", text)
        self.assertIn("超时预算 190 秒", text)
        self.assertEqual(call.call_args.kwargs, {"timeout": 190})
        self.assertIn(f"草案 {len(cedar)} 字节", text)
        for step in (
            "草案不含不适合发布的标记",
            "草案审批注解：3 条 @advice",
            "草案审批注解：R2",
            "草案审批注解：R4",
            "草案审批注解：R5",
        ):
            self.assertIn(f"[OK] {step}", text)

    def test_consumer_default_policy_uses_the_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)

            @contextmanager
            def local_run():
                with ExitStack() as cleanup:
                    yield report, cleanup

            preflight = Mock()
            exercise = Mock()
            with (
                patch("sys.argv", ["sandbox.py"]),
                patch.object(common, "check_host"),
                patch.object(common, "test_account"),
                patch.object(common, "local_run", local_run),
                patch.object(common, "prepare_consumer") as prepare,
            ):
                common.consumer_cli("fixture default", preflight, exercise)
            self.assertEqual(
                prepare.call_args.args[0].policy, common.FIXTURES / "task.cedar"
            )
            preflight.assert_called_once_with()
            exercise.assert_called_once()

    def test_gateway_policy_loads_and_substitutes_the_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            text = gateway.policy_text(root)
            expected = (
                (common.FIXTURES / "gateway.cedar.in")
                .read_text()
                .replace("{real_file}", str(root / "real"))
                .replace("{fake_file}", str(root / "fake"))
            )
            self.assertEqual(text, expected)
            self.assertIn(f'@real_token_file("{root / "real"}")', text)
            self.assertIn(f'@fake_token_file("{root / "fake"}")', text)
            self.assertNotIn("{real_file}", text)
            self.assertNotIn("{fake_file}", text)

    def test_shared_policy_points_to_the_test_workspace(self):
        policy = (common.FIXTURES / "task.cedar").read_text()
        for target in (
            "releases/previous.zip",
            "project/deploy.env",
            "config/*",
            "dist/*",
            "out/*",
            "releases/*",
        ):
            with self.subTest(target=target):
                self.assertIn(f'"{common.SHARED_ROW}/{target}"', policy)

    def test_existing_workspace_survives_a_failed_start(self):
        with tempfile.TemporaryDirectory() as directory:
            row = Path(directory)
            keep = row / "existing.txt"
            keep.write_text("keep")
            with patch.object(common, "SHARED_ROW", row), ExitStack() as cleanup:
                with self.assertRaisesRegex(RuntimeError, "already exists"):
                    common.create_workspace(cleanup)
            self.assertEqual(keep.read_text(), "keep")

    def test_uid_dropped_readers_never_inherit_admin_environment(self):
        account = SimpleNamespace(
            pw_name="tester", pw_uid=123, pw_gid=456, pw_dir="/home/tester"
        )
        with (
            patch.dict(
                os.environ,
                {
                    "GH_TOKEN": "synthetic-admin-only",
                    "GITHUB_TOKEN": "synthetic-other",
                    "AWS_SECRET_ACCESS_KEY": "synthetic-secret",
                    "LD_PRELOAD": "admin.so",
                },
            ),
            patch.object(
                common.subprocess, "check_output", return_value=b"fixture"
            ) as call,
            patch.object(common.os, "getgrouplist", return_value=[]),
        ):
            read = common.fixture_reader(Path("/work"), account)
            self.assertEqual(read("out/debug.log"), b"fixture")
            env = call.call_args.kwargs["env"]
            self.assertEqual(
                env, {"PATH": os.defpath, "LC_ALL": "C", "HOME": "/home/tester"}
            )
            self.assertEqual(env, common.as_account(account, Path("/work"))["env"])
            for key in (
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "AWS_SECRET_ACCESS_KEY",
                "LD_PRELOAD",
            ):
                self.assertNotIn(key, env)
            self.assertEqual(call.call_args.kwargs["user"], 123)
            self.assertEqual(call.call_args.kwargs["extra_groups"], [])

    def test_component_preflights(self):
        candidate = {
            "outcome": "candidate",
            "policy": "permit(principal, action, resource);",
        }
        with patch.object(intent, "cli", return_value=candidate) as cli:
            intent.check_intent()
            self.assertIn("propose-command", cli.call_args.args)
        with patch.object(sandbox, "cli", return_value={"items": []}) as cli:
            sandbox.preflight()
            cli.assert_called_once_with("policy", "list", "--limit", "1")
        with (
            patch.object(sight, "cli") as cli,
            patch.object(sight, "check_actplane") as health,
        ):
            sight.preflight()
            cli.assert_called_once_with("scope", "list", "--limit", "1")
            health.assert_called_once()

    def test_consumer_entries_publish_same_bytes_with_intent_offline(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            row = root / "workspace"
            source = root / "from-intent.cedar"
            original = (common.FIXTURES / "task.cedar").read_bytes() + b"// handoff\n"
            source.write_bytes(original)
            for module in (sandbox, sight):
                report = root / module.__name__
                report.mkdir()

                @contextmanager
                def local_run():
                    with ExitStack() as cleanup:
                        yield report, cleanup

                with ExitStack() as stack:
                    stack.enter_context(
                        patch("sys.argv", [module.__file__, "--policy", str(source)])
                    )
                    stack.enter_context(
                        patch.object(
                            intent,
                            "generate_policy",
                            side_effect=AssertionError("offline"),
                        )
                    )
                    stack.enter_context(
                        patch.object(
                            intent,
                            "check_intent",
                            side_effect=AssertionError("offline"),
                        )
                    )
                    stack.enter_context(patch.object(common, "SHARED_ROW", row))
                    stack.enter_context(patch.object(common, "check_host"))
                    stack.enter_context(
                        patch.object(
                            common,
                            "test_account",
                            return_value=SimpleNamespace(pw_uid=123),
                        )
                    )
                    stack.enter_context(patch.object(common, "local_run", local_run))
                    stack.enter_context(
                        patch.object(common, "fixture", return_value=b"previous")
                    )
                    publish = stack.enter_context(
                        patch.object(common, "publish", return_value="shared-policy")
                    )
                    stack.enter_context(patch.object(module, "preflight"))
                    exercise = stack.enter_context(patch.object(module, "exercise"))
                    module.main()
                    work, _ = exercise.call_args.args
                    self.assertEqual(work.policy, "shared-policy")
                    self.assertEqual(publish.call_args.args[0].read_bytes(), original)
                self.assertFalse(row.exists())


class SuitePresentationTests(unittest.TestCase):
    def test_all_entries_share_section_formatting(self):
        for module in (intent, sandbox, sight, gateway_entry, full):
            self.assertIs(module.section, common.section)

    def test_formal_summary_follows_cleanup_and_is_absent_on_failure(self):
        for module in (sandbox, sight, gateway_entry, full):
            for failure in (None, "exercise", "cleanup"):
                with (
                    self.subTest(module=module.__name__, failure=failure),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    report = Path(directory)
                    output = io.StringIO()

                    def clean():
                        print("cleanup completed")
                        if failure == "cleanup":
                            raise RuntimeError("cleanup failed")

                    @contextmanager
                    def local_run():
                        with ExitStack() as cleanup:
                            cleanup.callback(clean)
                            yield report, cleanup

                    def exercise(*args):
                        if failure == "exercise":
                            raise RuntimeError("exercise failed")
                        common.record(report, "case", {"ok": True})
                        return gateway.Control(b"token", b"surrogate", "tester")

                    with (
                        redirect_stdout(output),
                        patch("sys.argv", [module.__file__, "--user", "tester"]),
                        patch.object(common, "check_host"),
                        patch.object(common, "test_account"),
                        patch.object(common, "prepare_consumer"),
                        patch.object(module, "preflight"),
                        patch.object(module, "exercise", side_effect=exercise),
                        patch.object(common, "local_run", local_run),
                        patch.object(gateway_entry, "local_run", local_run),
                        patch.object(full, "local_run", local_run),
                    ):
                        if failure:
                            with self.assertRaisesRegex(
                                RuntimeError, f"{failure} failed"
                            ):
                                module.main()
                        else:
                            module.main()
                    text = output.getvalue()
                    self.assertIn("| 正式运行", text)
                    self.assertNotIn("\x1b", text)
                    if failure:
                        self.assertNotIn("── 结果摘要 ──", text)
                    else:
                        self.assertLess(
                            text.index("cleanup completed"),
                            text.index("── 结果摘要 ──"),
                        )
                        self.assertIn(f"验收报告：{report / 'results.txt'}", text)
                        self.assertEqual(
                            (report / "results.txt").read_text(),
                            'PASS case {"ok": true}\n',
                        )

    def test_local_run_labels_cleanup_even_after_a_failed_case(self):
        with tempfile.TemporaryDirectory() as directory:
            output = io.StringIO()
            with (
                redirect_stdout(output),
                patch("builtins.open", return_value=MagicMock()),
                patch.object(common.fcntl, "flock"),
                patch.object(common.tempfile, "mkdtemp", return_value=directory),
                self.assertRaisesRegex(RuntimeError, "case failed"),
            ):
                with common.local_run() as (report, cleanup):
                    cleanup.callback(print, "cleanup completed")
                    raise RuntimeError("case failed")
            self.assertEqual((report / "results.txt").read_text(), "FAIL case failed\n")
        text = output.getvalue()
        self.assertIn("── 报告与工作区 ──", text)
        self.assertLess(text.index("── 资源清理 ──"), text.index("cleanup completed"))
        self.assertNotIn("── 结果摘要 ──", text)

    def test_binding_wait_keeps_progress_and_reports_only_a_valid_success(self):
        item = {
            "spec": {
                "bindingId": "binding",
                "bindingRevision": 1,
                "policy": {"policyId": "policy", "revision": 1},
                "scope": {"scopeId": "scope", "process": {"pid": 12345}},
            },
            "status": {"phase": "READY"},
        }
        for phase in ("READY", "APPLY_FAILED", "PENDING"):
            output = io.StringIO()
            item["status"]["phase"] = phase
            with (
                self.subTest(phase=phase),
                redirect_stdout(output),
                patch.object(sight, "cli", return_value={"items": [item]}) as cli,
                patch.object(sight.time, "sleep"),
            ):
                if phase == "READY":
                    self.assertEqual(sight.ready("scope", "policy", 12345), "binding")
                else:
                    with self.assertRaises(RuntimeError):
                        sight.ready("scope", "policy", 12345)
            text = output.getvalue()
            self.assertIn("[CHECK] 探针 Binding 达到 READY", text)
            self.assertIn("最多 30 轮查询", text)
            status = "OK" if phase == "READY" else "FAIL"
            self.assertIn(f"[{status}] 探针 Binding 达到 READY", text)
            self.assertEqual(cli.call_count, 30 if phase == "PENDING" else 1)
            if phase != "READY":
                self.assertNotIn("[OK]", text)

    def test_actplane_cases_display_and_persist_each_verified_result(self):
        for failed in (False, True):
            output = io.StringIO()
            process = MagicMock()
            process.__enter__.return_value = process
            process.returncode = 1 if failed else 0
            process.poll.return_value = 0
            events = [
                {
                    "violation": "unlink-id",
                    "operation": "unlink",
                    "target": "previous.zip",
                    "blocked": True,
                },
                {
                    "violation": "read-id",
                    "operation": "read",
                    "target": "deploy.env",
                    "blocked": True,
                },
            ]
            with (
                self.subTest(failed=failed),
                tempfile.TemporaryDirectory() as directory,
                redirect_stdout(output),
                patch.object(sight, "start_actplane", return_value=process),
                patch.object(sight, "scope_for", return_value="scope"),
                patch.object(sight, "ready", return_value="binding"),
                patch.object(sight, "await_evidence", side_effect=events),
                patch.object(
                    sight, "fixture_reader", return_value=lambda _: b"previous"
                ),
            ):
                report = Path(directory)
                work = common.PolicyRun(report, Mock(), report, "policy", b"previous")
                if failed:
                    with self.assertRaisesRegex(RuntimeError, "probe exited"):
                        sight.exercise(work, Mock())
                    self.assertFalse((report / "results.txt").exists())
                else:
                    sight.exercise(work, Mock())
                    rows = (report / "results.txt").read_text().splitlines()
                    self.assertEqual(
                        [row.split()[1] for row in rows], ["actplane/3", "actplane/4"]
                    )
            text = output.getvalue()
            self.assertIn("── ActPlane 探针与 Binding ──", text)
            self.assertIn("超时预算 15 秒", text)
            if failed:
                self.assertIn("[FAIL] ActPlane 文件操作探针", text)
                self.assertNotIn("PASS actplane/", text)
            else:
                for case, event in zip(("3", "4"), events):
                    self.assertIn(f"── ActPlane 场景 {case}：", text)
                    self.assertLess(
                        text.index(f"violation='{event['violation']}'"),
                        text.index(f"PASS actplane/{case}"),
                    )

    def test_full_cases_need_no_manual_approval(self):
        cases = [case for case, _ in full.CASES]
        self.assertEqual(cases, ["1", "2", "3", "4", "5-fallback"])
        self.assertFalse(hasattr(full, "gateway"))

    def test_full_waits_for_each_case_without_prompts(self):
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)
            draft = report / "task.cedar"
            draft.write_text("// policy")
            account = SimpleNamespace(pw_uid=123, pw_gid=456)
            args = SimpleNamespace(user="tester")
            (report / "home").mkdir()
            cases = [case for case in full.CASES if case[0] in ("1", "5-fallback")]
            with (
                redirect_stdout(output),
                ExitStack() as cleanup,
                patch.object(full, "create_workspace", return_value=report),
                patch.object(full, "fixture", return_value=b"previous"),
                patch.object(full.os, "chown"),
                patch.object(intent, "generate_policy", return_value=draft),
                patch.object(full, "publish", return_value="policy"),
                patch.object(sight, "scope_for", return_value="scope"),
                patch.object(sight, "ready", return_value="binding"),
                patch.object(
                    full,
                    "fixture_reader",
                    return_value=lambda name: b"published",
                ),
                patch.object(full, "start_cosh", return_value=12345),
                patch.object(full, "CASES", cases),
                patch.object(full.time, "sleep"),
                patch.object(full.subprocess, "run") as send,
                patch("builtins.input") as enter,
                patch.object(
                    sandbox, "check_receipt", return_value={"verdict": "ALLOW"}
                ),
                patch.object(sight, "check_violation", return_value={}),
                patch.object(sandbox, "verify_case"),
            ):
                full.exercise(args, account, report, cleanup)
            enter.assert_not_called()
            rows = (report / "results.txt").read_text().splitlines()
            self.assertEqual(len(rows), 2)
            self.assertTrue(rows[0].startswith("PASS 1 "))
            self.assertTrue(rows[1].startswith("PASS 5-fallback "))
            self.assertEqual(send.call_count, len(cases) * 2)
        text = output.getvalue()
        for case in ("1", "5-fallback"):
            self.assertIn(f"── cosh 场景 {case} ──", text)
        self.assertNotIn("审批：", text)
        self.assertNotIn("SKIP 6", text)
        self.assertIn("[OK] cosh/5-fallback：回执与执行证据", text)


class CoshLaunchTests(unittest.TestCase):
    def test_account_command_runs_with_isolated_path_and_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            launcher = Path(directory) / "runuser"
            launcher.write_text(
                '#!/bin/sh\nprintf \'%s\\n\' "$0" "$@" "${GH_TOKEN-unset}"\n'
            )
            launcher.chmod(0o755)
            args = ("sh", "-c", "printf 'hello world'")
            with patch.dict(
                os.environ,
                {"PATH": directory, "GH_TOKEN": "synthetic-admin-only"},
            ):
                command = full.account_command("tester", *args)
                result = subprocess.check_output(
                    command, env=full.LOGIN_ENV, timeout=5, text=True
                )
            self.assertEqual(command[0], str(launcher))
            self.assertEqual(full.LOGIN_ENV["PATH"], os.defpath)
            self.assertEqual(
                result.splitlines(),
                [str(launcher), "--login", "tester", "--command", command[-1], "unset"],
            )
            self.assertEqual(full.shlex.split(command[-1]), list(args))

    def test_account_command_reports_missing_runuser_before_launch(self):
        with (
            patch.object(full.shutil, "which", return_value=None),
            self.assertRaisesRegex(RuntimeError, "Missing executable: runuser"),
        ):
            full.account_command("tester", "true")

    def test_start_cosh_unsets_runtime_preset_after_login_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (
                patch.dict(os.environ, GH_TOKEN="synthetic-admin-only"),
                patch.object(full.shutil, "which", return_value="/test/runuser"),
                patch.object(
                    full.subprocess, "check_output", return_value=b"12345\n"
                ) as start,
                patch.object(full.subprocess, "run") as stop,
                ExitStack() as cleanup,
            ):
                self.assertEqual(
                    full.start_cosh("tester", root, cleanup, "selftest"), 12345
                )
                self.assertEqual(start.call_args.args[0][0], "/test/runuser")
                command = full.shlex.split(start.call_args.args[0][-1])
                self.assertIn("AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS", command)
                preset = command.index("AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS")
                self.assertEqual(command[preset - 1], "-u")
                self.assertEqual(command[-1], "cosh")
                self.assertNotIn("GH_TOKEN", start.call_args.kwargs["env"])
            self.assertNotIn("GH_TOKEN", stop.call_args.kwargs["env"])
            self.assertEqual(stop.call_args.args[0][0], "/test/runuser")

    def test_tmux_cleanup_is_registered_before_startup_timeout(self):
        with (
            patch.object(full.shutil, "which", return_value="/test/runuser"),
            patch.object(
                full.subprocess,
                "check_output",
                side_effect=subprocess.TimeoutExpired("tmux", 15),
            ),
            patch.object(full.subprocess, "run") as stop,
        ):
            with self.assertRaises(subprocess.TimeoutExpired), ExitStack() as cleanup:
                full.start_cosh("tester", Path("/work"), cleanup, "selftest")
            self.assertIn("kill-server", stop.call_args.args[0][-1])


class PreflightOutputTests(unittest.TestCase):
    @staticmethod
    def actplane_health():
        return {
            "ready": True,
            "backend": "actplane",
            "capabilities": dict.fromkeys(
                (
                    "file_delete_guard",
                    "file_entry_delete_protection",
                    "directory_tree_delete_protection",
                    "file_read_protection",
                    "static_mount_topology",
                ),
                True,
            ),
        }

    def test_checking_prints_progress_before_work_and_success_afterwards(self):
        output = io.StringIO()
        with redirect_stdout(output):
            with common.checking("test step"):
                self.assertEqual(output.getvalue(), "[CHECK] test step\n")
        self.assertEqual(output.getvalue(), "[CHECK] test step\n[OK] test step\n")

    def test_quick_checks_only_print_the_result(self):
        output = io.StringIO()
        with redirect_stdout(output):
            with common.checking("quick step", progress=False):
                self.assertEqual(output.getvalue(), "")
        self.assertEqual(output.getvalue(), "[OK] quick step\n")

    def test_failed_quick_check_still_reports_the_step(self):
        output = io.StringIO()
        with redirect_stdout(output), self.assertRaises(RuntimeError):
            with common.checking("quick step", progress=False):
                raise RuntimeError("sensitive-provider-response")
        self.assertEqual(output.getvalue(), "[FAIL] quick step\n")

    def test_failed_step_is_reported_without_dumping_the_exception(self):
        output = io.StringIO()
        error = RuntimeError("sensitive-provider-response")
        with redirect_stdout(output), self.assertRaises(RuntimeError) as raised:
            with common.checking("test step"):
                raise error
        self.assertIs(raised.exception, error)
        self.assertEqual(output.getvalue(), "[CHECK] test step\n[FAIL] test step\n")
        self.assertNotIn(str(error), output.getvalue())

    def test_intent_check_prints_the_candidate_cedar_without_other_response_fields(
        self,
    ):
        output = io.StringIO()
        cedar = 'permit(principal, action == Action::"file_read", resource);'
        response = {
            "outcome": "candidate",
            "policy": cedar,
            "caveats": ["PATH lookup is unresolved"],
            "provider_response": "sensitive-provider-response",
        }
        with (
            redirect_stdout(output),
            patch("sys.argv", ["intent.py", "--check"]),
            patch.object(common.sys, "platform", "linux"),
            patch.object(common.shutil, "which", return_value="/test/agent-sec-cli"),
            patch.object(intent, "cli", return_value=response) as cli,
            patch.object(intent.tempfile, "mkdtemp") as report,
        ):
            intent.main()
        text = output.getvalue()
        for expected in (
            "platform=linux",
            "agent-sec-cli=/test/agent-sec-cli",
            "cat /etc/os-release",
            f"cwd={common.SHARED_ROW.parent}",
            "超时预算 60 秒",
            "outcome=candidate",
            "未执行命令、未发布策略",
            "候选 Cedar（未审批、未生效）",
            f"  {cedar}",
            "未覆盖：PATH lookup is unresolved",
            "PASS command intent",
        ):
            self.assertIn(expected, text)
        self.assertNotIn("sensitive-provider-response", text)
        self.assertNotIn("[CHECK] 运行环境", text)
        self.assertNotIn("[CHECK] 查找 agent-sec-cli", text)
        self.assertNotIn("── 任务策略生成 ──", text)
        self.assertIn("意图策略验收 | 仅预检", text)
        self.assertIn("── 结果摘要 ──", text)
        self.assertLess(text.index("[CHECK] 命令意图"), text.index("outcome=candidate"))
        self.assertLess(text.index("outcome=candidate"), text.index("候选 Cedar"))
        cli.assert_called_once()
        report.assert_not_called()

    def test_formal_intent_groups_output_and_preserves_the_report(self):
        output = io.StringIO()
        cedar = (common.FIXTURES / "task.cedar").read_bytes()
        candidate = {
            "outcome": "candidate",
            "policy": "permit(principal, action, resource);",
        }
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)
            with (
                redirect_stdout(output),
                patch("sys.argv", ["intent.py"]),
                patch.object(common.sys, "platform", "linux"),
                patch.object(
                    common.shutil, "which", return_value="/test/agent-sec-cli"
                ),
                patch.object(intent, "cli", return_value=candidate),
                patch.object(intent.tempfile, "mkdtemp", return_value=directory),
                patch.object(
                    intent.subprocess, "check_output", return_value=cedar
                ) as generate,
            ):
                intent.main()
            draft = report / "task.cedar"
            digest = hashlib.sha256(cedar).hexdigest()
            self.assertEqual(draft.read_bytes(), cedar)
            result = (report / "results.txt").read_text()
            self.assertTrue(result.startswith("PASS intent "))
            self.assertEqual(
                json.loads(result.removeprefix("PASS intent ")),
                {"policy_file": str(draft), "sha256": digest},
            )
            generate.assert_called_once()
        text = output.getvalue()
        headings = ("环境预检", "命令意图", "任务策略生成", "草案校验", "结果摘要")
        positions = [text.index(f"── {heading} ──") for heading in headings]
        self.assertEqual(positions, sorted(positions))
        self.assertIn(f"Run report: {report}", text)
        self.assertIn(f"Consumer input: --policy {draft}", text)
        self.assertIn(f"SHA-256：{digest}", text)
        self.assertIn("草案检查通过（未审批、未生效）", text)
        self.assertNotIn("[CHECK] 草案", text)
        self.assertEqual(text.count("[OK] 草案"), 5)
        self.assertTrue(text.endswith("PASS intent\n"))
        self.assertNotIn("\x1b", text)

    def test_failed_formal_draft_preserves_files_without_a_success_summary(self):
        output = io.StringIO()
        cedar = b'// unresolved: ["missing gate"]\n'
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)
            with (
                redirect_stdout(output),
                patch("sys.argv", ["intent.py"]),
                patch.object(intent, "check_host"),
                patch.object(intent, "check_intent"),
                patch.object(intent.tempfile, "mkdtemp", return_value=directory),
                patch.object(intent.subprocess, "check_output", return_value=cedar),
                self.assertRaisesRegex(RuntimeError, "3 @advice"),
            ):
                intent.main()
            self.assertEqual((report / "task.cedar").read_bytes(), cedar)
            self.assertTrue((report / "task.txt").is_file())
            self.assertFalse((report / "results.txt").exists())
        text = output.getvalue()
        self.assertIn(f"Run report: {report}", text)
        self.assertIn("[FAIL] 草案审批注解：3 条 @advice", text)
        self.assertNotIn("── 结果摘要 ──", text)
        self.assertNotIn("Consumer input:", text)
        self.assertNotIn("PASS intent", text)

    def test_candidate_wraps_to_the_terminal_width(self):
        output = io.StringIO()
        policy = "permit(principal, action, resource);" * 4
        with (
            redirect_stdout(output),
            patch.object(
                intent.shutil,
                "get_terminal_size",
                return_value=os.terminal_size((40, 24)),
            ),
        ):
            intent.show_candidate(policy, None)
        lines = output.getvalue().splitlines()[1:]
        self.assertGreater(len(lines), 1)
        self.assertTrue(all(line.startswith("    ") for line in lines))
        self.assertTrue(all(len(line) <= 40 for line in lines))

    def test_wrapped_candidate_still_has_a_display_line_limit(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(
                intent.shutil,
                "get_terminal_size",
                return_value=os.terminal_size((40, 24)),
            ),
        ):
            intent.show_candidate("a" * (36 * 45), None)
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), intent.CEDAR_LINE_LIMIT + 2)
        self.assertIn("其余 5 行已省略", lines[-1])

    def test_candidate_cedar_escapes_terminal_control_sequences(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(intent, "check_host"),
            patch.object(
                intent,
                "cli",
                return_value={
                    "outcome": "candidate",
                    "policy": "permit(principal, action, resource);\x1b[2J",
                },
            ),
        ):
            intent.check_intent()
        text = output.getvalue()
        self.assertNotIn("\x1b", text)
        self.assertIn("\\x1b[2J", text)

    def test_empty_candidate_reports_no_requested_permission(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(intent, "check_host"),
            patch.object(
                intent, "cli", return_value={"outcome": "candidate", "policy": ""}
            ),
        ):
            intent.check_intent()
        self.assertIn("空策略：不请求任何权限", output.getvalue())

    def test_overlong_candidate_is_truncated(self):
        output = io.StringIO()
        policy = "\n".join(f"// line {index}" for index in range(45))
        with (
            redirect_stdout(output),
            patch.object(intent, "check_host"),
            patch.object(
                intent, "cli", return_value={"outcome": "candidate", "policy": policy}
            ),
        ):
            intent.check_intent()
        text = output.getvalue()
        self.assertIn("// line 39", text)
        self.assertNotIn("// line 40", text)
        self.assertIn("其余 5 行已省略", text)

    def test_candidate_without_cedar_text_fails(self):
        with (
            patch.object(intent, "check_host"),
            patch.object(intent, "cli", return_value={"outcome": "candidate"}),
            self.assertRaisesRegex(RuntimeError, "no Cedar text"),
        ):
            intent.check_intent()

    def test_draft_notes_are_reported_without_blocking_acceptance(self):
        output = io.StringIO()
        draft = Path("/var/tmp/anolisa-intent-report-test/task.cedar")
        cedar = (
            (common.FIXTURES / "task.cedar")
            .read_text()
            .replace(
                "// unresolved: []",
                '// unresolved: ["R2 needs a command-level gate"]',
            )
        )
        with redirect_stdout(output):
            intent.check_draft(cedar, draft)
        text = output.getvalue()
        self.assertIn("[INFO] 未决（不阻断）：R2 needs a command-level gate", text)
        self.assertIn("[OK] 草案审批注解：3 条 @advice", text)
        self.assertNotIn("[FAIL]", text)

    def test_unreadable_draft_notes_stay_informational(self):
        output = io.StringIO()
        draft = Path("/var/tmp/anolisa-intent-report-test/task.cedar")
        cedar = (
            (common.FIXTURES / "task.cedar")
            .read_text()
            .replace(
                "// unresolved: []",
                "// unresolved: [not json]",
            )
        )
        with redirect_stdout(output):
            intent.check_draft(cedar, draft)
        text = output.getvalue()
        self.assertIn("[INFO] 未决（不阻断）：// unresolved: [not json]", text)
        self.assertNotIn("[FAIL]", text)

    def test_missing_confirmations_name_the_count_and_draft(self):
        output = io.StringIO()
        draft = Path("/var/tmp/anolisa-intent-report-test/task.cedar")
        cedar = (
            "// unresolved: []\n" + '@id("R2") permit(principal, action, resource);\n'
        )
        with (
            redirect_stdout(output),
            self.assertRaisesRegex(RuntimeError, "3 @advice") as raised,
        ):
            intent.check_draft(cedar, draft)
        text = output.getvalue()
        self.assertIn("[OK] 草案不含不适合发布的标记", text)
        self.assertIn("[FAIL] 草案审批注解：3 条 @advice", text)
        self.assertNotIn("[OK] 草案审批注解", text)
        self.assertNotIn("草案未决项", text)
        self.assertIn(str(draft), str(raised.exception))

    def test_missing_rule_id_names_the_rule_and_draft(self):
        draft = Path("/var/tmp/anolisa-intent-report-test/task.cedar")
        cedar = (
            (common.FIXTURES / "task.cedar")
            .read_text()
            .replace('@id("R5")', '@id("R5.unused")')
        )
        with (
            redirect_stdout(io.StringIO()),
            self.assertRaisesRegex(RuntimeError, "Missing R5") as raised,
        ):
            intent.check_draft(cedar, draft)
        self.assertIn(str(draft), str(raised.exception))

    def test_failed_intent_does_not_print_success(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch("sys.argv", ["intent.py", "--check"]),
            patch.object(intent, "check_host"),
            patch.object(intent, "cli", return_value={"outcome": "disabled"}),
            self.assertRaisesRegex(RuntimeError, "disabled"),
        ):
            intent.main()
        self.assertIn("[FAIL] 命令意图", output.getvalue())
        self.assertNotIn("[OK] 命令意图", output.getvalue())
        self.assertNotIn("PASS command intent", output.getvalue())

    def test_missing_cli_identifies_the_failed_dependency(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(common.sys, "platform", "linux"),
            patch.object(common.shutil, "which", return_value=None),
            self.assertRaisesRegex(RuntimeError, "Missing executable: agent-sec-cli"),
        ):
            common.check_host()
        self.assertIn("[OK] 运行环境", output.getvalue())
        self.assertIn("[FAIL] 查找 agent-sec-cli", output.getvalue())
        self.assertNotIn("[OK] 查找 agent-sec-cli", output.getvalue())

    def test_account_check_reports_the_probe_identity(self):
        output = io.StringIO()
        account = SimpleNamespace(pw_name="tester", pw_uid=123, pw_gid=456)
        with (
            redirect_stdout(output),
            patch.object(common.os, "geteuid", return_value=0),
            patch.object(common.pwd, "getpwnam", return_value=account),
        ):
            self.assertIs(common.test_account("tester"), account)
        self.assertIn("account=tester, uid=123, gid=456", output.getvalue())
        self.assertIn("[OK] 管理员权限与普通探针账号", output.getvalue())

    def test_consumer_checks_explain_policy_and_services_without_execution(self):
        for module in (sandbox, sight):
            output = io.StringIO()
            with (
                self.subTest(module=module.__name__),
                redirect_stdout(output),
                patch("sys.argv", [module.__file__, "--check"]),
                patch.object(common, "check_host"),
                patch.object(common, "test_account"),
                patch.object(module, "cli", return_value={"items": []}) as cli,
                patch.object(sight, "sight", return_value=self.actplane_health()),
                patch.object(common, "local_run") as local_run,
                patch.object(module, "exercise") as exercise,
            ):
                module.main()
                local_run.assert_not_called()
                exercise.assert_not_called()
                cli.assert_called_once()
            text = output.getvalue()
            self.assertIn(str(common.FIXTURES / "task.cedar"), text)
            self.assertIn("[OK] 输入策略文件存在", text)
            self.assertIn("未创建工作区、未发布策略、未执行探针", text)
            self.assertIn("PASS prerequisites", text)
            self.assertIn("| 仅预检", text)
            self.assertIn("── 环境预检 ──", text)
            self.assertIn("── 结果摘要 ──", text)
            self.assertNotIn("[CHECK] 输入策略文件", text)
            self.assertNotIn("[CHECK] ActPlane 必需能力", text)
            self.assertNotIn("── 策略发布 ──", text)
            self.assertNotIn("\x1b", text)
            if module is sandbox:
                self.assertIn("PAP 返回空列表", text)
            else:
                self.assertIn("scope list --limit 1", text)
                self.assertIn("http://127.0.0.1:7396/api/enforcement/health", text)
                for cap in self.actplane_health()["capabilities"]:
                    self.assertIn(f"[OK] ActPlane 必需能力：{cap}=True", text)

    def test_existing_pap_policy_reports_failure_without_dumping_it(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(sandbox, "cli", return_value={"items": ["sensitive-policy"]}),
            self.assertRaisesRegex(RuntimeError, "isolated PAP"),
        ):
            sandbox.preflight()
        self.assertIn("[FAIL] PAP 策略存储隔离", output.getvalue())
        self.assertNotIn("[OK] PAP", output.getvalue())
        self.assertNotIn("sensitive-policy", output.getvalue())

    def test_missing_actplane_capability_stops_at_the_failed_item(self):
        health = self.actplane_health()
        health["capabilities"]["file_read_protection"] = False
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(sight, "sight", return_value=health),
            self.assertRaisesRegex(RuntimeError, "Missing file_read_protection"),
        ):
            sight.check_actplane()
        text = output.getvalue()
        self.assertIn("[OK] ActPlane 健康接口", text)
        self.assertIn("[FAIL] ActPlane 必需能力：file_read_protection=True", text)
        self.assertNotIn("[OK] ActPlane 必需能力：file_read_protection", text)
        self.assertNotIn("static_mount_topology", text)

    @contextmanager
    def full_stubs(self, cosh_status=1):
        with ExitStack() as stack:
            stack.enter_context(patch.object(full, "check_host"))
            stack.enter_context(patch.object(full, "test_account"))
            stack.enter_context(patch.object(intent, "check_intent"))
            stack.enter_context(patch.object(sandbox, "preflight"))
            stack.enter_context(patch.object(sight, "preflight"))
            stack.enter_context(
                patch.object(
                    full.shutil, "which", side_effect=lambda cmd: f"/test/{cmd}"
                )
            )
            stack.enter_context(
                patch.object(
                    full.subprocess,
                    "run",
                    side_effect=[
                        subprocess.CompletedProcess([], 0),
                        subprocess.CompletedProcess([], cosh_status),
                    ],
                )
            )
            yield

    def test_full_check_stays_local_and_never_reads_credentials(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch("sys.argv", ["full.py", "--check", "--user", "tester"]),
            self.full_stubs(),
            patch.object(gateway, "credentials") as credentials,
            patch.object(gateway, "enable") as enable,
            patch.object(full, "local_run") as local_run,
            patch.object(full, "exercise") as exercise,
        ):
            full.main()
            credentials.assert_not_called()
            enable.assert_not_called()
            local_run.assert_not_called()
            exercise.assert_not_called()
        text = output.getvalue()
        self.assertIn("runuser=/test/runuser", text)
        self.assertIn("查找 cosh 和 tmux", text)
        self.assertIn("pgrep -x cosh 返回 1", text)
        self.assertIn("PASS full self-test prerequisites", text)
        self.assertIn("cosh 全流程验收 | 仅预检", text)
        self.assertIn("── cosh 运行环境预检 ──", text)
        self.assertIn("── 结果摘要 ──", text)
        self.assertNotIn("[CHECK] 系统工具", text)
        self.assertNotIn("\x1b", text)
        self.assertNotIn("Gateway", text)

    def test_busy_cosh_host_fails_without_reporting_full_success(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch("sys.argv", ["full.py", "--check", "--user", "tester"]),
            self.full_stubs(cosh_status=0),
            self.assertRaisesRegex(RuntimeError, "idle test host"),
        ):
            full.main()
        self.assertIn("[FAIL] 测试主机空闲", output.getvalue())
        self.assertNotIn("PASS full self-test prerequisites", output.getvalue())
        self.assertNotIn("── 结果摘要 ──", output.getvalue())


class GatewayEntryTests(unittest.TestCase):
    def test_check_does_not_read_tokens_or_run_other_components(self):
        for source in ("auto", "prompt"):
            output = io.StringIO()
            account = SimpleNamespace(pw_uid=123, pw_gid=456)
            with (
                self.subTest(source=source),
                redirect_stdout(output),
                patch(
                    "sys.argv",
                    [
                        "gateway.py",
                        "--check",
                        "--user",
                        "tester",
                        "--token-source",
                        source,
                    ],
                ),
                patch.object(gateway_entry, "check_host"),
                patch.object(gateway_entry, "test_account", return_value=account),
                patch.object(gateway.pwd, "getpwnam"),
                patch.object(gateway.os, "access", return_value=True),
                patch.object(
                    gateway.shutil, "which", side_effect=lambda cmd: f"/test/{cmd}"
                ),
                patch.object(
                    gateway_entry,
                    "cli",
                    side_effect=lambda *args: {
                        "enabled": False,
                        "ready": False,
                        "items": [],
                        "token": "sensitive-token",
                    },
                ) as calls,
                patch.object(gateway, "credentials") as credentials,
                patch.object(gateway, "enable") as enable,
                patch.object(gateway_entry, "local_run") as local_run,
                patch.object(gateway_entry, "exercise") as exercise,
                patch.object(intent, "check_intent") as intent_check,
                patch.object(sandbox, "preflight") as sandbox_check,
                patch.object(sight, "check_actplane") as actplane_check,
            ):
                gateway_entry.main()
            for operation in (
                credentials,
                enable,
                local_run,
                exercise,
                intent_check,
                sandbox_check,
                actplane_check,
            ):
                operation.assert_not_called()
            self.assertEqual(
                [call.args[:2] for call in calls.call_args_list],
                [("gateway", "status"), ("policy", "list"), ("scope", "list")],
            )
            text = output.getvalue()
            self.assertIn("PASS gateway prerequisites", text)
            self.assertIn("Gateway 凭据替换验收 | 仅预检", text)
            self.assertIn("── Gateway 环境与服务预检 ──", text)
            self.assertIn("── PAP 与 Scope 服务预检 ──", text)
            self.assertIn("── 结果摘要 ──", text)
            self.assertNotIn("[CHECK] Gateway 服务账号", text)
            self.assertNotIn("[CHECK] Gateway 策略模板", text)
            self.assertNotIn("\x1b", text)
            self.assertNotIn("sensitive-token", text)

    def test_default_token_source_reuses_the_admin_credential(self):
        @contextmanager
        def local_run():
            with ExitStack() as cleanup:
                yield Path(tempfile.mkdtemp()), cleanup

        output = io.StringIO()
        account = SimpleNamespace(pw_uid=123, pw_gid=456)
        with (
            redirect_stdout(output),
            patch("sys.argv", ["gateway.py", "--user", "tester"]),
            patch.object(gateway_entry, "preflight", return_value=account) as preflight,
            patch.object(gateway_entry, "local_run", local_run),
            patch.object(
                gateway_entry,
                "exercise",
                return_value=gateway.Control(b"token", b"surrogate", "tester"),
            ) as exercise,
            patch.object(gateway_entry, "result_summary"),
        ):
            gateway_entry.main()
        self.assertEqual(preflight.call_args.args, ("tester", "auto"))
        self.assertEqual(exercise.call_args.args[1], "auto")
        self.assertIn("不需要输入", output.getvalue())
        self.assertIn("200 且账号一致", output.getvalue())

    def test_nonempty_pap_stops_before_running(self):
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch("sys.argv", ["gateway.py", "--check", "--user", "tester"]),
            patch.object(gateway_entry, "check_host"),
            patch.object(gateway_entry, "test_account"),
            patch.object(gateway, "preflight"),
            patch.object(gateway_entry.os, "access", return_value=True),
            patch.object(
                gateway_entry, "cli", return_value={"items": ["private-policy"]}
            ),
            patch.object(gateway_entry, "local_run") as local_run,
            self.assertRaisesRegex(RuntimeError, "isolated PAP"),
        ):
            gateway_entry.main()
        local_run.assert_not_called()
        self.assertIn("[FAIL] PAP", output.getvalue())
        self.assertNotIn("PASS gateway", output.getvalue())
        self.assertNotIn("private-policy", output.getvalue())
        self.assertNotIn("── 结果摘要 ──", output.getvalue())

    def test_client_uses_a_unique_script_and_unprivileged_environment(self):
        compile(gateway_entry.CLIENT, "gateway-probe.py", "exec")
        with tempfile.TemporaryDirectory() as directory:
            row = Path(directory)
            account = SimpleNamespace(
                pw_name="tester", pw_uid=123, pw_gid=456, pw_dir="/home/tester"
            )
            with (
                patch.object(common.os, "getgrouplist", return_value=[]),
                patch.object(gateway_entry.subprocess, "Popen") as popen,
            ):
                gateway_entry.start_client(row, account)
            argv = popen.call_args.args[0]
            self.assertEqual(
                argv, [sys.executable, "-I", str(row / "gateway-probe.py")]
            )
            self.assertEqual(popen.call_args.kwargs["user"], 123)
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertNotIn("GH_TOKEN", popen.call_args.kwargs["env"])
            self.assertEqual(
                (row / "gateway-probe.py").read_text(), gateway_entry.CLIENT
            )
            self.assertIn("sys.stdin.readline()", gateway_entry.CLIENT)
            self.assertIn('"--disable"', gateway_entry.CLIENT)
            self.assertIn('"--ipv4"', gateway_entry.CLIENT)
            self.assertIn('"@out/debug.log"', gateway_entry.CLIENT)

    def test_stop_client_terminates_its_session_and_reaps_it(self):
        process = Mock(pid=12345)
        with patch.object(gateway_entry.os, "killpg") as killpg:
            gateway_entry.stop_client(process)
        killpg.assert_called_once_with(12345, gateway_entry.signal.SIGTERM)
        process.wait.assert_called_once_with(timeout=5)

    def test_stop_client_escalates_only_after_timeout(self):
        process = Mock(pid=12345)
        process.wait.side_effect = [subprocess.TimeoutExpired("probe", 5), 0]
        with patch.object(gateway_entry.os, "killpg") as killpg:
            gateway_entry.stop_client(process)
        self.assertEqual(
            [call.args for call in killpg.call_args_list],
            [
                (12345, gateway_entry.signal.SIGTERM),
                (12345, gateway_entry.signal.SIGKILL),
            ],
        )
        self.assertEqual(process.wait.call_count, 2)

    def test_stop_client_reaps_a_session_that_already_exited(self):
        process = Mock(pid=12345)
        with patch.object(gateway_entry.os, "killpg", side_effect=ProcessLookupError):
            gateway_entry.stop_client(process)
        process.wait.assert_called_once_with(timeout=5)

    def run_exercise(
        self,
        *,
        ready_error=None,
        verify_error=None,
        packets=(0, 3),
        probe_ready=True,
        probe_result=b"done\n",
    ):
        output = io.StringIO()
        process = MagicMock()
        process.__enter__.return_value = process
        process.poll.return_value = None
        process.pid = 12345
        process.stdout.readline.return_value = probe_result
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = root / "workspace"
            row.mkdir()
            secrets_dir = root / "secrets"
            secrets_dir.mkdir(mode=0o700)
            ca = root / "ca.pem"
            ca.write_text("test CA")
            account = SimpleNamespace(pw_uid=123, pw_gid=456)

            @contextmanager
            def private_directory(**kwargs):
                self.assertEqual(kwargs["dir"], "/root")
                yield str(secrets_dir)

            with (
                redirect_stdout(output),
                ExitStack() as cleanup,
                patch.object(gateway_entry, "create_workspace", return_value=row),
                patch.object(gateway_entry.os, "chown"),
                patch.object(
                    gateway_entry.tempfile, "TemporaryDirectory", private_directory
                ),
                patch.object(
                    gateway,
                    "credentials",
                    return_value=gateway.Control(
                        b"sensitive-real", b"surrogate", "tester"
                    ),
                ),
                patch.object(gateway, "enable", return_value=ca),
                patch.object(
                    gateway_entry, "publish", return_value="gw-policy"
                ) as publish,
                patch.object(gateway, "CredentialLease") as lease,
                patch.object(gateway_entry, "start_client", return_value=process),
                patch.object(gateway_entry.os, "killpg") as killpg,
                patch.object(sight, "scope_for") as old_scope,
                patch.object(
                    sight, "ready", return_value="gw-binding", side_effect=ready_error
                ),
                patch.object(gateway, "packets", side_effect=packets),
                patch.object(
                    gateway_entry.select,
                    "select",
                    return_value=([process.stdout] if probe_ready else [], [], []),
                ),
                patch.object(gateway_entry, "fixture_reader"),
                patch.object(gateway, "verify", side_effect=verify_error),
            ):
                scope = lease.return_value.scope_for
                scope.return_value = "gw-scope"
                if (
                    ready_error
                    or verify_error
                    or packets[1] <= packets[0]
                    or not probe_ready
                    or probe_result != b"done\n"
                ):
                    with self.assertRaises(RuntimeError):
                        gateway_entry.exercise(account, "prompt", root, cleanup)
                else:
                    gateway_entry.exercise(account, "prompt", root, cleanup)
                results = (root / "results.txt").read_text()
                scope.assert_called_once_with(row / "gateway-probe.py", 123)
                old_scope.assert_not_called()
                self.assertIs(
                    publish.call_args.kwargs["on_created"], lease.return_value.capture
                )
                self.assertIn("--schema-revision", publish.call_args.args)
            text = output.getvalue()
            for heading in (
                "GitHub 直连对照",
                "Gateway 就绪",
                "Gateway 策略发布",
                "Gateway 请求与证据",
            ):
                self.assertIn(f"── {heading} ──", text)
            if not ready_error:
                self.assertIn("超时预算 45 秒", text)
            if "PASS gateway/6" in results:
                self.assertIn("Scope 包计数：", text)
                self.assertIn("账号=tester", text)
            self.assertNotIn("sensitive-real", text)
            self.assertNotIn("sensitive-real", results)
        killpg.assert_called_once_with(process.pid, gateway_entry.signal.SIGTERM)
        process.wait.assert_called_once_with(timeout=5)
        return results, process

    def test_success_requires_controls_binding_and_packet_evidence(self):
        results, process = self.run_exercise()
        self.assertIn("PASS gateway/controls", results)
        self.assertIn("PASS gateway/policy", results)
        self.assertIn("PASS gateway/6", results)
        self.assertIn('"binding": "gw-binding"', results)
        self.assertIn('"packets_after": 3', results)
        process.stdin.write.assert_called_once_with(b"ready\n")

    def test_binding_failure_never_starts_request(self):
        results, process = self.run_exercise(ready_error=RuntimeError("not ready"))
        process.stdin.write.assert_not_called()
        self.assertNotIn("PASS gateway/6", results)

    def test_verification_failure_stops_client_and_does_not_pass(self):
        results, _ = self.run_exercise(verify_error=RuntimeError("wrong response"))
        self.assertNotIn("PASS gateway/6", results)

    def test_stale_packet_counter_does_not_pass(self):
        results, _ = self.run_exercise(packets=(3, 3))
        self.assertNotIn("PASS gateway/6", results)

    def test_request_timeout_stops_client_and_does_not_pass(self):
        results, _ = self.run_exercise(probe_ready=False)
        self.assertNotIn("PASS gateway/6", results)

    def test_client_failure_stops_client_and_does_not_pass(self):
        results, _ = self.run_exercise(probe_result=b"")
        self.assertNotIn("PASS gateway/6", results)

    def test_full_entry_no_longer_runs_the_credential_case(self):
        self.assertNotIn("6", [case for case, _ in full.CASES])
        self.assertFalse(hasattr(full, "gateway"))

    def test_gateway_enabled_state_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            ca = Path(directory) / "ca.pem"
            ca.touch()
            for enabled in (False, True):
                calls = Mock(
                    side_effect=lambda *args: {
                        "enabled": enabled,
                        "ready": True,
                        "caBundlePath": str(ca),
                    }
                )
                with self.subTest(enabled=enabled), ExitStack() as cleanup:
                    self.assertEqual(gateway.enable(calls, cleanup), ca)
                operations = [call.args for call in calls.call_args_list]
                self.assertEqual(("gateway", "enable") in operations, not enabled)
                self.assertEqual(("gateway", "disable") in operations, not enabled)


class CredentialCleanupTests(unittest.TestCase):
    @contextmanager
    def environment(self):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as mocks:
            report = Path(directory)
            row = report / "workspace"
            row.mkdir()
            account = SimpleNamespace(
                pw_uid=123, pw_gid=456, pw_name="tester", pw_dir="/home/tester"
            )
            policy = {
                "policyId": "test-policy",
                "revision": 1,
                "credentialBindings": [
                    {"ruleId": "case6-github-user", "pairId": "new-pair"}
                ],
            }
            state = SimpleNamespace(
                policy=policy,
                active={"existing-pair"},
                policies=[],
                scopes=[],
                calls=[],
                create_error=None,
                commit=True,
                receipt=True,
                malformed_reply=False,
                other_registration=False,
                concurrent_pair=None,
                concurrent_policy=None,
                scope_error=None,
                readback_error=None,
                invisible=0,
                teardown_delay=0,
                stuck=False,
                remove_noop=False,
            )

            def cli(*args):
                state.calls.append(args)
                if args == ("gateway", "key", "list"):
                    return {"pairids": sorted(state.active)}
                if args[:3] == ("gateway", "key", "remove"):
                    if not state.remove_noop:
                        state.active.remove(args[3])
                    return {}
                if args[:2] == ("policy", "create"):
                    source = {
                        "schema": {"schemaId": "seccore.system", "revision": 3},
                        "text": Path(args[5]).read_text(),
                    }
                    policy.update(
                        policyName=args[3],
                        template={"language": "cedar", "content": source},
                    )
                    ids = gateway.pair_ids(policy)
                    state.active.update(ids)
                    if state.concurrent_pair:
                        state.active.add(state.concurrent_pair)
                    if state.receipt:
                        store = report / "credentials/store.json"
                        store.parent.mkdir()
                        # Match the wire representation hashed by GatewayService::register.
                        encoded = (
                            '{"schema":{"schemaId":"seccore.system","revision":3},"text":'
                            + json.dumps(source["text"], ensure_ascii=False)
                            + "}"
                        )
                        key = hashlib.sha256(encoded.encode()).hexdigest()
                        registrations = {key: sorted(ids)}
                        if state.other_registration:
                            registrations["another-source"] = sorted(ids)
                        store.write_text(
                            json.dumps({"version": 2, "registrations": registrations})
                        )
                        store.chmod(0o600)
                    if state.concurrent_policy:
                        state.policies.append(state.concurrent_policy)
                    if state.commit:
                        state.policies.append(policy)
                    if state.create_error:
                        raise state.create_error
                    if state.malformed_reply:
                        return {"policyId": "unrelated-policy"}
                    return policy
                if args[:2] == ("policy", "get"):
                    if state.readback_error:
                        raise state.readback_error
                    return {
                        "template": {
                            "content": {"text": (report / "gateway.cedar").read_text()}
                        }
                    }
                if args[:2] == ("policy", "delete"):
                    state.policies = [
                        p for p in state.policies if p["policyId"] != args[3]
                    ]
                    return {}
                if args[:2] == ("scope", "create"):
                    state.scopes.append(
                        {
                            "scopeId": "owned-scope",
                            "ownerUid": int(args[5]),
                            "selector": {
                                "kind": "process",
                                "match": {"cmdlineArg": args[3]},
                            },
                            "policySnapshots": [policy],
                            "status": "ACTIVE",
                        }
                    )
                    if state.scope_error:
                        raise state.scope_error
                    return {"scopeId": "owned-scope"}
                if args[:2] == ("scope", "delete"):
                    if state.teardown_delay or state.stuck:
                        state.teardown_delay = max(0, state.teardown_delay - 1)
                        state.scopes[0]["status"] = "DELETING"
                    else:
                        state.scopes = [
                            s for s in state.scopes if s["scopeId"] != args[3]
                        ]
                    return {"completed": not state.scopes}
                if args[1] == "list" and args[0] in ("policy", "scope"):
                    items = state.policies if args[0] == "policy" else state.scopes
                    if args[0] == "scope" and state.invisible:
                        state.invisible -= 1
                        items = []
                    offset = int(args[5])
                    return {"items": items[offset : offset + int(args[3])]}
                raise AssertionError(args)

            mocks.enter_context(
                patch.dict(os.environ, {"AGENT_SEC_DATA_DIR": directory})
            )
            temporary_directory = tempfile.TemporaryDirectory
            mocks.enter_context(
                patch.object(
                    gateway_entry.tempfile,
                    "TemporaryDirectory",
                    side_effect=lambda **kwargs: temporary_directory(
                        prefix=kwargs["prefix"], dir=report
                    ),
                )
            )
            control = gateway.Control(b"synthetic-real", b"synthetic-fake", "tester")
            mocks.enter_context(patch.object(common, "cli", cli))
            mocks.enter_context(patch.object(gateway_entry, "cli", cli))
            mocks.enter_context(
                patch.object(gateway_entry, "create_workspace", return_value=row)
            )
            mocks.enter_context(
                patch.object(gateway, "credentials", return_value=control)
            )
            mocks.enter_context(
                patch.object(gateway, "enable", return_value=report / "ca")
            )
            mocks.enter_context(patch.object(gateway, "client_files"))
            mocks.enter_context(patch.object(gateway_entry.os, "chown"))
            mocks.enter_context(patch.object(gateway_entry, "stop_client"))
            mocks.enter_context(patch.object(gateway.time, "sleep"))
            mocks.enter_context(redirect_stdout(io.StringIO()))
            yield report, row, account, cli, state

    def test_lost_create_reply_recovers_only_the_committed_source(self):
        for error in (
            subprocess.CalledProcessError(1, "create"),
            subprocess.TimeoutExpired("create", 70),
        ):
            with (
                self.subTest(error=type(error)),
                self.environment() as (report, _, account, _, state),
            ):
                state.create_error = error
                state.concurrent_pair = "unrelated"
                other = {
                    **state.policy,
                    "policyId": "other",
                    "credentialBindings": [{"pairId": "unrelated"}],
                    "policyName": report.name + "-gateway",
                    "template": {
                        "language": "cedar",
                        "content": {"text": "other source"},
                    },
                }
                state.concurrent_policy = other
                with self.assertRaises(type(error)), ExitStack() as cleanup:
                    gateway_entry.exercise(account, "auto", report, cleanup)
                self.assertEqual(state.active, {"existing-pair", "unrelated"})
                self.assertEqual(state.policies, [other])
                self.assertFalse((report / "gateway-recovery.json").exists())
                self.assertEqual(
                    sum(call[:2] == ("policy", "create") for call in state.calls), 1
                )

    def test_orphan_registration_uses_exact_source_and_preserves_other_pairs(self):
        for pair, error in (
            ("existing-pair", subprocess.CalledProcessError(1, "create")),
            ("new-pair", subprocess.CalledProcessError(1, "create")),
            ("new-pair", subprocess.TimeoutExpired("create", 70)),
        ):
            with (
                self.subTest(pair=pair, error=type(error)),
                self.environment() as (report, _, account, _, state),
            ):
                state.policy["credentialBindings"][0]["pairId"] = pair
                state.create_error = error
                state.commit = False
                state.concurrent_pair = "unrelated"
                with (
                    self.assertRaisesRegex(RuntimeError, "cleanup incomplete"),
                    ExitStack() as cleanup,
                ):
                    gateway_entry.exercise(account, "auto", report, cleanup)
                self.assertEqual(state.active, {"existing-pair", "unrelated"})
                journal = json.loads((report / "gateway-recovery.json").read_text())
                self.assertEqual(journal["priorPairIds"], ["existing-pair"])
                self.assertEqual(journal["sourceFile"], "gateway.cedar")
                self.assertFalse(
                    any(call[:2] == ("policy", "delete") for call in state.calls)
                )

    def test_malformed_reply_cannot_schedule_an_unverified_policy_for_deletion(self):
        with self.environment() as (report, _, account, _, state):
            state.malformed_reply = True
            with (
                self.assertRaisesRegex(
                    RuntimeError, "Unexpected Gateway Policy source"
                ),
                ExitStack() as cleanup,
            ):
                gateway_entry.exercise(account, "auto", report, cleanup)
            self.assertEqual(state.active, {"existing-pair"})
            self.assertEqual(state.policies, [])
            deletes = [
                call[3] for call in state.calls if call[:2] == ("policy", "delete")
            ]
            self.assertEqual(deletes, ["test-policy"])

    def test_shared_registration_without_policy_is_left_for_recovery(self):
        with self.environment() as (report, _, account, _, state):
            state.create_error = subprocess.TimeoutExpired("create", 70)
            state.commit = False
            state.other_registration = True
            with (
                self.assertRaisesRegex(RuntimeError, "shared by another source"),
                ExitStack() as cleanup,
            ):
                gateway_entry.exercise(account, "auto", report, cleanup)
            self.assertEqual(state.active, {"new-pair", "existing-pair"})
            self.assertTrue((report / "gateway-recovery.json").is_file())

    def test_unknown_create_without_receipt_keeps_recovery_record(self):
        with self.environment() as (report, _, account, _, state):
            state.create_error = subprocess.TimeoutExpired("create", 70)
            state.commit = state.receipt = False
            state.concurrent_pair = "unrelated"
            with (
                self.assertRaisesRegex(RuntimeError, "cleanup incomplete"),
                ExitStack() as cleanup,
            ):
                gateway_entry.exercise(account, "auto", report, cleanup)
            self.assertTrue((report / "gateway-recovery.json").is_file())
            self.assertEqual(state.active, {"new-pair", "existing-pair", "unrelated"})
            self.assertFalse(
                any(call[:3] == ("gateway", "key", "remove") for call in state.calls)
            )

    def test_real_entry_retires_imports_when_client_start_fails_or_is_interrupted(self):
        for error in (OSError("spawn failed"), KeyboardInterrupt()):
            with (
                self.subTest(error=type(error)),
                self.environment() as (report, _, account, _, state),
            ):
                with patch.object(gateway_entry, "start_client", side_effect=error):
                    with self.assertRaises(type(error)), ExitStack() as cleanup:
                        gateway_entry.exercise(account, "auto", report, cleanup)
                self.assertEqual(state.active, {"existing-pair"})
                self.assertEqual(state.policies, [])
                self.assertIn(("gateway", "key", "remove", "new-pair"), state.calls)
                self.assertFalse(
                    any(call[:2] == ("scope", "create") for call in state.calls)
                )

    def test_imports_are_registered_before_publication_readback(self):
        with self.environment() as (report, _, account, _, state):
            state.readback_error = RuntimeError("readback failed")
            with (
                self.assertRaisesRegex(RuntimeError, "readback failed"),
                ExitStack() as cleanup,
            ):
                gateway_entry.exercise(account, "auto", report, cleanup)
            self.assertEqual(state.active, {"existing-pair"})
            self.assertEqual(state.policies, [])

    def test_lost_scope_response_is_recovered_before_retirement(self):
        for error in (KeyboardInterrupt(), subprocess.TimeoutExpired("scope", 70)):
            with (
                self.subTest(error=type(error)),
                self.environment() as (report, _, account, _, state),
            ):
                state.scope_error = error
                state.invisible = 2
                state.teardown_delay = 2
                process = MagicMock()
                process.__enter__.return_value = process
                with patch.object(gateway_entry, "start_client", return_value=process):
                    with self.assertRaises(type(error)), ExitStack() as cleanup:
                        gateway_entry.exercise(account, "auto", report, cleanup)
                self.assertEqual(state.scopes, [])
                self.assertEqual(state.active, {"existing-pair"})
                deletes = [
                    i
                    for i, call in enumerate(state.calls)
                    if call[:2] == ("scope", "delete")
                ]
                retire = state.calls.index(("gateway", "key", "remove", "new-pair"))
                self.assertEqual(len(deletes), 3)
                self.assertLess(max(deletes), retire)

    def test_existing_pair_is_never_attached_or_retired(self):
        with self.environment() as (_, row, account, cli, state):
            state.policy["credentialBindings"][0]["pairId"] = "existing-pair"
            with (
                self.assertRaisesRegex(RuntimeError, "reused existing"),
                ExitStack() as cleanup,
            ):
                lease = gateway.CredentialLease(cli, cleanup)
                lease.capture(state.policy)
                lease.scope_for(row / "gateway-probe.py", account.pw_uid)
            self.assertEqual(state.active, {"existing-pair"})
            self.assertFalse(
                any(
                    call[:2] == ("scope", "create")
                    or call[:3] == ("gateway", "key", "remove")
                    for call in state.calls
                )
            )

    def test_other_policy_or_scope_references_preserve_new_pairs(self):
        for kind in ("policy", "scope"):
            with self.subTest(kind=kind), self.environment() as (_, _, _, cli, state):
                with ExitStack() as cleanup:
                    lease = gateway.CredentialLease(cli, cleanup)
                    lease.capture(state.policy)
                    state.active.update({"new-pair", "unrelated-pair"})
                    shared = {**state.policy, "policyId": "other-policy"}
                    if kind == "policy":
                        state.policies.append(shared)
                    else:
                        state.scopes.append(
                            {
                                "scopeId": "other-scope",
                                "selector": {},
                                "policySnapshots": [shared],
                            }
                        )
                self.assertEqual(
                    state.active, {"existing-pair", "new-pair", "unrelated-pair"}
                )
                self.assertFalse(
                    any(
                        call[:3] == ("gateway", "key", "remove") for call in state.calls
                    )
                )

    def test_scope_ownership_requires_uid_selector_and_policy_revision(self):
        with (
            self.environment() as (_, row, account, cli, state),
            ExitStack() as cleanup,
        ):
            lease = gateway.CredentialLease(cli, cleanup)
            lease.capture(state.policy)
            lease.scope_for(row / "gateway-probe.py", account.pw_uid)
            scope = state.scopes[0]
            self.assertTrue(lease.owns_scope(scope))
            for change in (
                {"ownerUid": 999},
                {"selector": {"kind": "pid", "pid": 123}},
                {"policySnapshots": [{**state.policy, "revision": 2}]},
                {"policySnapshots": [{**state.policy, "policyId": "other-policy"}]},
            ):
                self.assertFalse(lease.owns_scope({**scope, **change}))

    def test_incomplete_teardown_never_retires_credentials(self):
        with self.environment() as (_, row, account, cli, state):
            with (
                self.assertRaisesRegex(RuntimeError, "teardown incomplete"),
                ExitStack() as cleanup,
            ):
                lease = gateway.CredentialLease(cli, cleanup)
                lease.capture(state.policy)
                state.active.add("new-pair")
                lease.scope_for(row / "gateway-probe.py", account.pw_uid)
                state.stuck = True
            self.assertIn("new-pair", state.active)
            self.assertFalse(
                any(call[:3] == ("gateway", "key", "remove") for call in state.calls)
            )

    def test_unsuccessful_retirement_is_not_reported_as_success(self):
        with self.environment() as (_, _, _, cli, state):
            with (
                self.assertRaisesRegex(RuntimeError, "retirement incomplete"),
                ExitStack() as cleanup,
            ):
                lease = gateway.CredentialLease(cli, cleanup)
                lease.capture(state.policy)
                state.active.add("new-pair")
                state.remove_noop = True

    def test_no_import_or_already_retired_pair_needs_no_removal(self):
        with self.environment() as (_, _, _, cli, state):
            with ExitStack() as cleanup:
                gateway.CredentialLease(cli, cleanup)
            self.assertEqual(state.calls, [("gateway", "key", "list")])
            with ExitStack() as cleanup:
                lease = gateway.CredentialLease(cli, cleanup)
                lease.capture(state.policy)
            self.assertFalse(
                any(call[:3] == ("gateway", "key", "remove") for call in state.calls)
            )

    def test_failed_scope_create_without_commit_waits_before_retirement(self):
        with self.environment() as (_, row, account, cli, state):

            def failed_create(*args):
                if args[:2] == ("scope", "create"):
                    raise KeyboardInterrupt()
                return cli(*args)

            with self.assertRaises(KeyboardInterrupt), ExitStack() as cleanup:
                lease = gateway.CredentialLease(failed_create, cleanup)
                lease.capture(state.policy)
                state.active.add("new-pair")
                lease.scope_for(row / "gateway-probe.py", account.pw_uid)
            queries = [call for call in state.calls if call[:2] == ("scope", "list")]
            self.assertEqual(len(queries), 30)
            self.assertEqual(state.active, {"existing-pair"})

    def test_failed_reference_query_never_retires_credentials(self):
        with self.environment() as (_, _, _, cli, state):

            def unavailable(*args):
                if args[:2] == ("policy", "list"):
                    raise RuntimeError("reference query failed")
                return cli(*args)

            with (
                self.assertRaisesRegex(RuntimeError, "reference query failed"),
                ExitStack() as cleanup,
            ):
                lease = gateway.CredentialLease(unavailable, cleanup)
                lease.capture(state.policy)
                state.active.add("new-pair")
            self.assertIn("new-pair", state.active)
            self.assertFalse(
                any(call[:3] == ("gateway", "key", "remove") for call in state.calls)
            )

    def test_publication_registers_imports_before_restoring_signals(self):
        with self.environment() as (report, _, _, _, state), ExitStack() as cleanup:
            path = report / "gateway.cedar"
            path.write_text("synthetic publication")
            captured = Mock()

            def restore(operation, mask):
                if operation == common.signal.SIG_SETMASK:
                    captured.assert_called_once_with(state.policy)
                return set()

            with patch.object(common.signal, "pthread_sigmask", side_effect=restore):
                common.publish(path, "test-policy", cleanup, on_created=captured)

    def test_reference_checks_include_all_pages(self):
        page = [{"policyId": str(i)} for i in range(1000)]
        last = {"policyId": "shared", "credentialBindings": [{"pairId": "new-pair"}]}
        cli = Mock(side_effect=[{"items": page}, {"items": [last]}])
        self.assertEqual(gateway.all_items(cli, "policy"), page + [last])
        self.assertEqual(
            cli.call_args.args,
            ("policy", "list", "--limit", "1000", "--offset", "1000"),
        )


class SandboxOutputTests(unittest.TestCase):
    @staticmethod
    def files():
        return {
            "out/build.log": b"stale report\n",
            "home/.zshrc": common.SHELL_CONFIG,
            "project/source-guard.txt": common.SOURCE,
            "dist/latest.zip": b"stale package\n",
            "out/build-report.log": b"stale report\n",
            "releases/previous.zip": b"previous package",
        }

    @staticmethod
    def build_log():
        return b"build ok\n.zshrc: Read-only file system\nzshrc-update rc=1\n"

    def test_probe_reports_execute_or_finish_without_exposing_the_handoff(self):
        for case, decision, exit_code, stderr in (
            ("1", "allow", 0, b""),
            ("2", "allow", 1, b"source-guard.txt: Read-only file system\n"),
            ("5-deny", "ask", 0, b""),
        ):
            output = io.StringIO()
            reply = {
                "decision": decision,
                "hookSpecificOutput": {
                    "tool_input": {
                        "command": "agent-sec-cli --socket /run/test.sock sandbox hook execute private-handoff"
                    }
                },
            }
            with (
                self.subTest(case=case),
                tempfile.TemporaryDirectory() as directory,
                redirect_stdout(output),
                patch.object(sandbox, "as_account", return_value={}),
                patch.object(
                    sandbox.shutil, "which", return_value="/test/agent-sec-cli"
                ),
                patch.object(
                    sandbox.subprocess, "check_output", return_value=json.dumps(reply)
                ),
                patch.object(
                    sandbox.subprocess,
                    "run",
                    return_value=subprocess.CompletedProcess(
                        [], exit_code, stderr=stderr
                    ),
                ) as run,
            ):
                row = Path(directory)
                evidence = sandbox.probe(case, row, Mock())
                action = "finish" if case == "5-deny" else "execute"
                self.assertEqual(
                    run.call_args.args[0][-2:], [action, "private-handoff"]
                )
                request = json.loads((row / f"request-{case}.json").read_text())
                self.assertEqual(
                    request["filesystem"]["rules"][-1]["rights"], ["write"]
                )
            text = output.getvalue()
            self.assertIn("待执行命令", text)
            self.assertIn("sandbox hook prepare", text)
            self.assertIn(f"sandbox hook {action}", text)
            self.assertNotIn("private-handoff", text)
            self.assertEqual(evidence["hook_action"], action)
            if case == "2":
                self.assertIn("Read-only file system", text)
                self.assertEqual(evidence["stderr"], stderr.decode())
            elif case == "5-deny":
                self.assertIn("未调用 execute", text)
                self.assertNotIn("sandbox hook execute", text)

    def test_non_sandbox_write_error_cannot_pass_the_probe(self):
        reply = {
            "decision": "allow",
            "hookSpecificOutput": {
                "tool_input": {
                    "command": "agent-sec-cli --socket /run/test.sock sandbox hook execute token"
                }
            },
        }
        with (
            tempfile.TemporaryDirectory() as directory,
            redirect_stdout(io.StringIO()),
            patch.object(sandbox, "as_account", return_value={}),
            patch.object(sandbox.shutil, "which", return_value="/test/agent-sec-cli"),
            patch.object(
                sandbox.subprocess, "check_output", return_value=json.dumps(reply)
            ),
            patch.object(
                sandbox.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(
                    [], 1, stderr=b"Permission denied"
                ),
            ),
            self.assertRaisesRegex(RuntimeError, "read-only mount"),
        ):
            sandbox.probe("2", Path(directory), Mock())

    def test_file_comparison_reports_actual_hashes_and_rejects_tampering(self):
        before = self.files()
        for changed in (False, True):
            after = dict(before)
            if changed:
                after["project/source-guard.txt"] = b"tampered\x1b[2J"
            output = io.StringIO()
            with self.subTest(changed=changed), redirect_stdout(output):
                if changed:
                    with self.assertRaisesRegex(RuntimeError, "Unexpected file change"):
                        sandbox.file_evidence("2", before, after)
                else:
                    evidence = sandbox.file_evidence("2", before, after)
                    self.assertTrue(evidence["project/source-guard.txt"]["unchanged"])
            text = output.getvalue()
            digest = hashlib.sha256(after["project/source-guard.txt"]).hexdigest()
            self.assertIn(f"执行后 SHA-256={digest}", text)
            self.assertIn(f"逐字节一致={not changed}", text)
            self.assertNotIn("\x1b", text)
            status = "FAIL" if changed else "OK"
            self.assertIn(f"[{status}] 文件未被篡改：project/source-guard.txt", text)
            self.assertNotIn("[CHECK] 文件未被篡改", text)

    def test_exercise_displays_and_persists_evidence_before_pass(self):
        files = self.files()
        output = io.StringIO()

        def probe(case, row, account):
            if case == "1":
                files["out/build.log"] = self.build_log()
            return {"hook_action": "finish" if case == "5-deny" else "execute"}

        def receipt(case, since, uid):
            return {
                "receipt": f"receipt-{case}",
                "verdict": {"1": "ALLOW", "2": "MODIFY", "5-deny": "STEP_UP"}[case],
                "started": case != "5-deny",
                "exit": {"1": 0, "2": 1, "5-deny": None}[case],
                "host_finished": case == "5-deny",
            }

        with (
            tempfile.TemporaryDirectory() as directory,
            redirect_stdout(output),
            patch.object(sandbox, "fixture_reader", return_value=files.__getitem__),
            patch.object(sandbox, "probe", side_effect=probe),
            patch.object(sandbox, "check_receipt", side_effect=receipt),
        ):
            report = Path(directory)
            work = common.PolicyRun(
                report,
                SimpleNamespace(pw_uid=1000),
                report,
                "p",
                files["releases/previous.zip"],
            )
            sandbox.exercise(work, Mock())
            rows = (report / "results.txt").read_text().splitlines()
            self.assertEqual(len(rows), 3)
            for row, case in zip(rows, ("1", "2", "5-deny")):
                prefix = f"PASS sandbox/{case} "
                self.assertTrue(row.startswith(prefix))
                evidence = json.loads(row[len(prefix) :])
                self.assertEqual(evidence["receipt"], f"receipt-{case}")
                self.assertTrue(evidence["files"]["releases/previous.zip"]["unchanged"])
            self.assertEqual(
                (report / "sandbox-1.build.log").read_bytes(), self.build_log()
            )
        text = output.getvalue()
        self.assertIn("verdict=MODIFY started=True exit=1", text)
        self.assertIn(
            "verdict=STEP_UP started=False exit=None host_finished=True", text
        )
        self.assertIn("zshrc-update rc=1", text)
        self.assertEqual(text.count("Sandbox 只读隔离已生效"), 2)
        self.assertIn("本项不证明 Sandbox 运行时拦截", text)
        for case in ("1", "2", "5-deny"):
            self.assertIn(f"── Sandbox 场景 {case}：", text)
        for case, target in (
            ("1", "home/.zshrc"),
            ("2", "project/source-guard.txt"),
            ("5-deny", "dist/latest.zip"),
        ):
            self.assertLess(
                text.index(f"[OK] 文件未被篡改：{target}"),
                text.index(f"PASS sandbox/{case}"),
            )

    def test_invalid_build_does_not_report_sandbox_success(self):
        for build in (
            b"stale report\n",
            b"build ok\n",
            b"build ok\nRead-only file system\nzshrc-update rc=0\n",
        ):
            files = self.files()
            output = io.StringIO()

            def probe(*args):
                files["out/build.log"] = build
                return {}

            with (
                self.subTest(build=build),
                tempfile.TemporaryDirectory() as directory,
                redirect_stdout(output),
                patch.object(sandbox, "fixture_reader", return_value=files.__getitem__),
                patch.object(sandbox, "probe", side_effect=probe),
                patch.object(
                    sandbox,
                    "await_evidence",
                    return_value={
                        "verdict": "ALLOW",
                        "started": True,
                        "exit": 0,
                        "host_finished": False,
                        "receipt": "id",
                    },
                ),
            ):
                report = Path(directory)
                work = common.PolicyRun(
                    report,
                    SimpleNamespace(pw_uid=1000),
                    report,
                    "p",
                    files["releases/previous.zip"],
                )
                with self.assertRaises(RuntimeError):
                    sandbox.exercise(work, Mock())
                self.assertFalse((report / "results.txt").exists())
            self.assertIn("[FAIL] sandbox/1", output.getvalue())
            self.assertNotIn("PASS sandbox/", output.getvalue())
            self.assertNotIn("Sandbox 只读隔离已生效", output.getvalue())


class EvidenceTests(unittest.TestCase):
    def test_ready_pins_the_first_binding_revision(self):
        item = {
            "spec": {
                "bindingId": "binding",
                "bindingRevision": 1,
                "policy": {"policyId": "policy", "revision": 1},
                "scope": {"scopeId": "scope", "process": {"pid": 12345}},
            },
            "status": {"phase": "READY"},
        }
        with patch.object(sight, "cli", return_value={"items": [item]}):
            self.assertEqual(sight.ready("scope", "policy", 12345), "binding")
            item["spec"]["bindingRevision"] = 2
            with self.assertRaisesRegex(RuntimeError, "first spec revision"):
                sight.ready("scope", "policy", 12345)

    def test_actplane_binding_translates_the_pap_identity(self):
        # Live pair: PAP binding 7f1e6138… revision 1 reached ActPlane as
        # bbdcdaa6…, and only the translated id appears in violation rows.
        self.assertEqual(
            sight.actplane_binding("7f1e6138-0d03-408d-935b-6550073aa5b6", 1),
            "bbdcdaa6-7fbe-5e72-8637-0a09af435788",
        )

    def test_unrelated_or_stale_unlink_cannot_prove_a_block(self):
        translated = sight.actplane_binding("b", 1)
        event = dict(
            event_id="id",
            policy_id="p",
            binding_id=translated,
            operation="unlink",
            target="previous.zip",
            blocked=True,
            occurred_at_ns=100,
        )
        with patch.object(sight, "sight", return_value={"violations": [event]}):
            self.assertEqual(
                sight.check_violation("p", "b", "3", 100),
                {
                    "violation": "id",
                    "actplane_binding": translated,
                    "operation": "unlink",
                    "target": "previous.zip",
                    "blocked": True,
                },
            )
            for changes in (
                dict(policy_id="other"),
                # The PAP identity itself never appears in a violation row.
                dict(binding_id="b"),
                dict(binding_id="other"),
                dict(blocked=False),
                dict(occurred_at_ns=99),
                dict(target="other"),
                dict(operation="read"),
            ):
                with self.subTest(changes=changes), patch.dict(event, changes):
                    with self.assertRaises(RuntimeError):
                        sight.check_violation("p", "b", "3", 100)

    def test_denial_requires_evidence_that_the_process_never_started(self):
        details = {
            "principal": {"agentType": "cosh"},
            "host_finished": True,
            "result": {"started": False},
        }
        event = {
            "event_id": "id",
            "uid": 1000,
            "verdict": "STEP_UP",
            "details": details,
        }
        with patch.object(sandbox, "cli", return_value=[event]):
            self.assertFalse(sandbox.check_receipt("5-deny", "start", 1000)["started"])
            for changes in (
                dict(result=None),
                dict(result={"started": True}),
                dict(approval=True),
            ):
                with self.subTest(changes=changes), patch.dict(details, changes):
                    with self.assertRaises(RuntimeError):
                        sandbox.check_receipt("5-deny", "start", 1000)
            with self.assertRaises(RuntimeError):
                sandbox.check_receipt("4", "start", 1000)

    def test_gateway_approval_requires_a_started_successful_shell(self):
        details = {
            "principal": {"agentType": "cosh"},
            "approval": True,
            "result": {"started": True, "exit_code": 0},
        }
        event = {
            "event_id": "gateway-receipt",
            "uid": 1000,
            "verdict": "STEP_UP",
            "details": details,
        }
        with patch.object(sandbox, "cli", return_value=[event]):
            self.assertTrue(sandbox.check_receipt("6", "start", 1000)["approval"])
            for change in (
                {"approval": False},
                {"approval": None},
                {"result": {"started": False, "exit_code": 0}},
                {"result": {"started": True, "exit_code": 1}},
            ):
                with self.subTest(change=change), patch.dict(details, change):
                    with self.assertRaises(RuntimeError):
                        sandbox.check_receipt("6", "start", 1000)
            with patch.dict(event, verdict="ALLOW"), self.assertRaises(RuntimeError):
                sandbox.check_receipt("6", "start", 1000)

    def test_gateway_requires_the_surrogate_status_and_authenticated_account(self):
        real = secrets.token_hex().encode()
        fake = secrets.token_hex().encode()
        files = {
            "out/http-status": b"200",
            "out/debug.log": b"Authorization: Bearer " + fake + b"\n",
            "out/github-user.json": b'{"login":"test-account"}',
        }
        control = gateway.Control(real, fake, "test-account")
        gateway.verify(files.__getitem__, control)
        for changes in (
            {"out/http-status": b"401"},
            {"out/debug.log": real},
            {"out/debug.log": files["out/debug.log"] + b"extra\n"},
            {"out/github-user.json": b'{"login":"other"}'},
            {
                "out/github-user.json": json.dumps(
                    {"login": "test-account", "extra": real.decode()}
                ).encode()
            },
        ):
            with patch.dict(files, changes), self.assertRaises(RuntimeError) as raised:
                gateway.verify(files.__getitem__, control)
            self.assertNotIn(real.decode(), str(raised.exception))
            self.assertNotIn(fake.decode(), str(raised.exception))

    def test_auto_source_reuses_an_existing_credential_without_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def generate(argv, **kwargs):
                self.assertEqual(argv[:3], ["agent-sec-cli", "key", "generate-fake"])
                Path(argv[argv.index("--output") + 1]).write_bytes(b"ghp_surrogate")
                return subprocess.CompletedProcess(argv, 0)

            with (
                redirect_stdout(io.StringIO()),
                patch.object(gateway.getpass, "getpass") as prompt,
                patch.object(
                    gateway, "discover_token", return_value=b"ghp_realcredential"
                ),
                patch.object(gateway.subprocess, "run", side_effect=generate),
                patch.object(gateway, "github_user", return_value="tester") as direct,
            ):
                control = gateway.credentials(root, "auto")
        prompt.assert_not_called()
        self.assertEqual(control.account, "tester")
        self.assertEqual(control.token, b"ghp_realcredential")
        self.assertNotIn(control.token, control.surrogate)
        self.assertEqual(
            [call.args for call in direct.call_args_list],
            [(b"ghp_realcredential", 200), (b"ghp_surrogate", 401)],
        )

    def test_auto_source_failure_names_the_credential_sources(self):
        with (
            patch.object(gateway, "_environment_token", return_value=b""),
            patch.object(gateway, "_gh_token", return_value=b""),
            patch.object(gateway, "_stored_token", return_value=b""),
        ):
            with self.assertRaisesRegex(RuntimeError, "GH_TOKEN"):
                gateway.discover_token()


if __name__ == "__main__":
    unittest.main()
