"""Keep the candidate image and fork profile isolated and evidence-based."""

import copy
import hashlib
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import textwrap
from unittest.mock import patch

from test_ci_checks import AW, GateFixture
import resident_container
from check_native import load, select
from native_acceptance import verify_bundle
from resident_container import source_files, stage
from resident_contract import BINARIES, shards, suites, verify, verify_shard


class ResidentContainerTests(GateFixture):
    def setUp(self):
        super().setUp()
        self.manifest = load(AW)

    def reports(self):
        image = self.root / "image"
        image.mkdir()
        (image / "image.json").write_text(json.dumps({
            "candidate_sha": "a" * 40, "image_removed": True, "image": "fixture-image",
        }))
        result = {}
        for shard, plan in shards(self.manifest).items():
            directory = self.root / shard
            directory.mkdir()
            report = {
                "candidate_sha": "a" * 40, "shard": shard, "passed": True,
                "suites": [{"agent": plan["agent"], "name": c["name"]} for c in plan["cases"]],
                "runtime": {
                    "python": self.manifest["agents"]["claude-sdk"]["python"],
                    "node": "v24.16.0", "claude_cli": "2.1.161 (Claude Code)", "pi": "0.84.2",
                    "rtk": "rtk 0.49.0",
                    "python_packages": self.manifest["agents"]["claude-sdk"]["versions"],
                },
            }
            cleanup = {"candidate_sha": "a" * 40, "shard": shard, "network": "none",
                       "mounts": [], "removed": True, "image_removed": True, "image": "fixture-image"}
            (directory / "result.json").write_text(json.dumps(report))
            (directory / "container.json").write_text(json.dumps(cleanup))
            result[shard] = (directory, report, cleanup)
        return result

    def test_fork_profile_preserves_all_credential_free_agents_and_full_default(self):
        self.assertEqual(
            set(select(self.manifest)["agents"]), set(self.manifest["agents"])
        )
        reduced = select(self.manifest, True)
        self.assertEqual(
            set(reduced["agents"]),
            set(self.manifest["agents"]) - {"qoder", "qoder-sdk"},
        )
        self.assertEqual(suites(reduced), suites(self.manifest))

    def test_container_tracks_inventory_additions_and_rejects_empty_or_invalid_commands(
        self,
    ):
        edited = copy.deepcopy(self.manifest)
        row = edited["agents"]["pi"]
        case = copy.deepcopy(
            next(c for c in row["cases"] if c["name"] == "resident-lifecycle")
        )
        case["name"] = "new-resident-case"
        row["cases"].append(case)
        self.assertIn(case, suites(edited)["pi"]["cases"])
        case["argv"].insert(1, "-c")
        with self.assertRaisesRegex(ValueError, "shared harness"):
            suites(edited)
        row["cases"] = []
        with self.assertRaisesRegex(ValueError, "missing"):
            suites(edited)

    def test_image_context_excludes_extra_payload_and_environment_files(self):
        binaries, context = self.root / "bin", self.root / "context"
        binaries.mkdir()
        context.mkdir()
        names = BINARIES | {
            "policy",
            "aw-provider-sec-core",
            "agent-sec-cli",
            "agent-sec-daemon",
        }
        lines = []
        for name in names:
            (binaries / name).write_text(name)
            (binaries / name).chmod(0o700)
            lines.append(hashlib.sha256(name.encode()).hexdigest() + "  " + name)
        (binaries / "SHA256SUMS").write_text("\n".join(lines))
        (binaries / "source-sha.txt").write_text("a" * 40)
        for name in (".env", "credentials.json", "deployment.yaml"):
            (binaries / name).write_text("excluded-fixture-content")
        stage(AW, binaries, context, "a" * 40, self.manifest)
        self.assertEqual(
            {p.name for p in (context / "payload").iterdir()},
            {"aw", "aw-provider-tokenless", "tokenless", "rtk", "SHA256SUMS", "source-sha.txt"},
        )
        verify_bundle(context / "payload", "a" * 40, BINARIES)
        for path in context.rglob("*"):
            if path.is_file():
                self.assertNotIn(b"excluded-fixture-content", path.read_bytes())
        self.assertTrue(
            all("target" not in path.relative_to(AW).parts for path in source_files(AW))
        )

    def test_result_requires_every_shard_actual_reports_versions_and_cleanup(self):
        reports = self.reports()
        with self.assertRaises(ValueError):
            verify(self.root, self.manifest, "a" * 40)
        with patch("resident_contract.verify_case") as check:
            result = verify(self.root, self.manifest, "a" * 40)
            self.assertEqual(check.call_count, len(result["suites"]))
            self.assertEqual(len(result["suites"]), 24)
        for shard, (directory, report, cleanup) in reports.items():
            for field, value in (("passed", False), ("candidate_sha", "b" * 40),
                                 ("shard", "unknown"), ("suites", report["suites"][:-1])):
                (directory / "result.json").write_text(json.dumps({**report, field: value}))
                with self.subTest(shard=shard, field=field), patch("resident_contract.verify_case"):
                    with self.assertRaises(ValueError):
                        verify(self.root, self.manifest, "a" * 40)
            (directory / "result.json").write_text(json.dumps(report))
            for field, value in (("removed", False), ("image_removed", False),
                                 ("network", "bridge"), ("mounts", [{}]), ("image", "other")):
                (directory / "container.json").write_text(json.dumps({**cleanup, field: value}))
                with self.subTest(shard=shard, field=field), patch("resident_contract.verify_case"):
                    with self.assertRaises(ValueError):
                        verify(self.root, self.manifest, "a" * 40)
            (directory / "container.json").write_text(json.dumps(cleanup))
            (directory / "result.json").unlink()
            with patch("resident_contract.verify_case"), self.assertRaises(ValueError):
                verify(self.root, self.manifest, "a" * 40)
            (directory / "result.json").write_text(json.dumps(report))

    def test_long_and_regular_shards_retain_lifecycle_cases_with_nested_deadlines(self):
        planned = shards(self.manifest)
        self.assertEqual(set(planned), {"claude-sdk-long", "claude-sdk-regular", "pi-long", "pi-regular"})
        actual = [(p["agent"], c["name"]) for p in planned.values() for c in p["cases"]]
        expected = [(a, c["name"]) for a, row in suites(self.manifest).items() for c in row["cases"]]
        self.assertCountEqual(actual, expected)
        self.assertIn(("claude-sdk", "resident-drain-timeout"), actual)
        self.assertIn(("claude-sdk", "resident-model-boundaries"), actual)
        self.assertIn(("claude-sdk", "resident-native-denial"), actual)
        for agent in ("claude-sdk", "pi"):
            self.assertIn((agent, "resident-long"), actual)
            self.assertIn((agent, "resident-drain"), actual)
        for plan in planned.values():
            self.assertGreater(plan["timeout_seconds"], sum(c["timeout_seconds"] for c in plan["cases"]))
            self.assertGreaterEqual(plan["job_minutes"] * 60, plan["timeout_seconds"] + 900)

    def test_versions_build_identity_and_duplicate_shards_cannot_report_success(self):
        reports = self.reports()
        shard, (directory, report, _) = next(iter(reports.items()))
        for key in ("python", "node", "claude_cli", "pi", "rtk"):
            changed = copy.deepcopy(report)
            changed["runtime"][key] = "wrong-version"
            (directory / "result.json").write_text(json.dumps(changed))
            with patch("resident_contract.verify_case"), self.assertRaises(ValueError):
                verify(self.root, self.manifest, "a" * 40)
        (directory / "result.json").write_text(json.dumps(report))
        duplicate = self.root / "duplicate"
        duplicate.mkdir()
        (duplicate / "result.json").write_text(json.dumps(report))
        with patch("resident_contract.verify_case"), self.assertRaises(ValueError):
            verify(self.root, self.manifest, "a" * 40)
        (duplicate / "result.json").unlink()
        (self.root / "image/image.json").write_text(json.dumps({
            "candidate_sha": "b" * 40, "image_removed": True, "image": "fixture-image",
        }))
        with patch("resident_contract.verify_case"), self.assertRaises(ValueError):
            verify(self.root, self.manifest, "a" * 40)

    def test_failed_build_still_removes_its_owned_image(self):
        state = {"image": False}

        def run(_docker, *argv, timeout):
            state["image"] = True
            raise InterruptedError("cancelled build")

        def query(_docker, *argv):
            if argv[:2] == ("image", "ls"):
                return "image" if state["image"] else ""
            if argv[:2] == ("image", "rm"):
                state["image"] = False
            return ""

        with patch.object(resident_container, "stage", return_value={}), \
                patch.object(resident_container.Docker, "run", autospec=True, side_effect=run), \
                patch.object(resident_container.Docker, "query", autospec=True, side_effect=query):
            with self.assertRaises(InterruptedError):
                resident_container.build(self.root, self.root / "build", "a" * 40, self.manifest)
        self.assertFalse(state["image"])
        self.assertTrue(json.loads((self.root / "build/image.json").read_text())["image_removed"])

    def test_wrong_or_corrupt_archive_is_rejected_before_loading(self):
        bundle = self.root / "bundle"
        bundle.mkdir()
        (bundle / "image.tar").write_bytes(b"modified")
        source = {"candidate_sha": "a" * 40, "image_removed": True,
                  "archive_sha256": hashlib.sha256(b"original").hexdigest()}
        for fields in ({}, {"candidate_sha": "b" * 40}, {"image_removed": False}):
            (bundle / "image.json").write_text(json.dumps({**source, **fields}))
            with patch.object(resident_container, "Docker") as docker, self.assertRaises(ValueError):
                resident_container.run_shard(bundle, self.root / "out", "a" * 40, self.manifest, "pi-long")
            docker.assert_not_called()

    def test_image_mutation_settles_before_interruption_and_cleanup(self):
        def interrupted(signum, _frame):
            raise InterruptedError(f"fixture signal {signum}")

        previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
        try:
            for argv in (("image", "load"), ("buildx", "build")):
                state = {"image": False, "completed": False}
                docker = resident_container.Docker(self.root)

                def run(_commands, command, cwd, timeout):
                    self.assertEqual(timeout, 120)
                    signal.raise_signal(signal.SIGINT)
                    signal.raise_signal(signal.SIGTERM)
                    # Docker publishes its tag only after the interrupt arrives.
                    state.update(image=True, completed=True)
                    return ""

                def query(*command):
                    if command[:2] == ("image", "rm"):
                        state["image"] = False
                    return "fixture-image" if state["image"] else ""

                with patch.object(resident_container.Commands, "run", autospec=True, side_effect=run), \
                        patch.object(docker, "query", side_effect=query):
                    with self.assertRaises(InterruptedError):
                        docker.run(*argv, timeout=120)
                    self.assertTrue(docker.remove_image("fixture-tag"))
                self.assertEqual(state, {"image": False, "completed": True})
                for sig in previous:
                    self.assertIs(signal.getsignal(sig), interrupted)
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)

    def test_unmerged_profile_requires_manual_fork_exact_sha_and_no_secret(self):
        workflow = (AW.parents[1] / ".github/workflows/aw-preview.yml").read_text()
        block = workflow.split(
            "      - name: Require a trusted candidate before executing repository code\n",
            1,
        )[1]
        script = textwrap.dedent(
            block.split("        run: |\n", 1)[1].split(
                "      - name: Validate native coverage", 1
            )[0]
        )
        expected = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=AW, text=True
        ).strip()
        environment = {
            **os.environ,
            "CANDIDATE_SHA": expected,
            "WORKFLOW_SHA": expected,
            "WORKFLOW_REF": "refs/heads/feature/example",
            "REPOSITORY": "fixture/fork",
            "EVENT_NAME": "workflow_dispatch",
            "CREDENTIAL_FREE": "true",
        }
        for overrides, passed in (
            ({}, True),
            ({"REPOSITORY": "agentic-os-org/ANOLISA"}, False),
            ({"WORKFLOW_SHA": "b" * 40}, False),
            ({"EVENT_NAME": "pull_request"}, False),
            ({"WORKFLOW_REF": "refs/tags/fixture"}, False),
        ):
            result = subprocess.run(
                ["bash", "-euo", "pipefail", "-c", script],
                cwd=AW,
                env={**environment, **overrides},
                timeout=10,
                capture_output=True,
            )
            self.assertEqual(result.returncode == 0, passed, result.stderr)
        self.assertIn("!inputs.credential_free && (matrix.agent", workflow)
        caller = (AW.parents[1] / ".github/workflows/aw-ci.yml").read_text()
        self.assertIn(
            "!inputs.credential_free && secrets.QODER_PERSONAL_ACCESS_TOKEN", caller
        )
        container = workflow.split("  resident-container:\n", 1)[1].split(
            "  result:\n", 1
        )[0]
        self.assertNotIn("secrets.", container)

    def test_failed_container_execution_or_copy_still_removes_owned_resources(self):
        bundle = self.root / "bundle"
        bundle.mkdir()
        (bundle / "image.tar").write_bytes(b"fixture image")
        tag = "aw-resident:" + "a" * 32
        (bundle / "image.json").write_text(json.dumps({
            "candidate_sha": "a" * 40, "image_removed": True, "tag": tag,
            "image": "fixture-image", "archive_sha256": hashlib.sha256(b"fixture image").hexdigest(),
        }))
        for failure in ("execution", "copy", "interrupt", "load"):
            output = self.root / failure
            state = {"container": False, "image": False}

            def query(_docker, *command):
                if command[0] == "create":
                    state["container"] = True
                    return "fixture-id"
                if command[:2] == ("image", "inspect"):
                    return json.dumps([{"Id": "fixture-image", "Config": {"Labels": {
                        "org.opencontainers.image.revision": "a" * 40}}}])
                if command[0] == "inspect":
                    return json.dumps([{"Image": "fixture-image", "Mounts": [],
                                        "HostConfig": {"NetworkMode": "none"}}])
                if command[0] == "cp" and failure == "copy":
                    raise subprocess.CalledProcessError(1, command)
                if command[0] == "ps":
                    return "fixture-id" if state["container"] else ""
                if command[0] == "rm":
                    state["container"] = False
                if command[:2] == ("image", "ls"):
                    return "fixture-image" if state["image"] else ""
                if command[:2] == ("image", "rm"):
                    state["image"] = False
                return ""

            def run(commands, command, cwd, timeout):
                self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
                argv = tuple(command[1:])
                if argv[:2] == ("image", "load"):
                    state["image"] = True
                    if failure == "load":
                        raise subprocess.TimeoutExpired(argv, timeout)
                elif failure == "execution":
                    raise subprocess.TimeoutExpired(argv, timeout)
                elif failure == "interrupt":
                    raise InterruptedError("fixture interrupt")

            with patch.object(resident_container.Docker, "query", autospec=True, side_effect=query), \
                    patch.object(resident_container.Commands, "run", autospec=True, side_effect=run), \
                    patch.dict(os.environ, {"QODER_PERSONAL_ACCESS_TOKEN": "excluded-fixture-token"}):
                with self.assertRaises((subprocess.TimeoutExpired, subprocess.CalledProcessError, InterruptedError)):
                    resident_container.run_shard(bundle, output, "a" * 40, self.manifest, "pi-long")
            self.assertEqual(state, {"container": False, "image": False})
            cleanup = json.loads((output / "container.json").read_text())
            self.assertTrue(cleanup["removed"])
            # A timed-out daemon mutation cannot certify that its tag stays absent.
            self.assertEqual(cleanup["image_removed"], failure != "load")



if __name__ == "__main__":
    import unittest

    unittest.main()
