"""Exercise the AW gate's failure boundaries without compiling Rust."""

from copy import deepcopy
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from typing import Union
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("aw_check", AW / "scripts/check.py")
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class GateFixture(unittest.TestCase):
    def setUp(self) -> None:
        (AW / "target").mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-check-", dir=AW / "target")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)


class PackageWorkflowTests(GateFixture):
    def test_real_cli_paths_use_the_container_workspace(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-preview.yml").read_text()
        step = workflow.split(
            "      - name: Verify Tokenless PreTool and PostTool contracts\n", 1
        )[1]
        step = step.split("      - name: Verify installed packages", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        workspace = self.root / "container workspace"
        expected = [
            workspace / "src/tokenless/target/release/tokenless",
            workspace / "src/aw/target/release/rtk",
        ]
        for executable in expected:
            executable.parent.mkdir(parents=True, exist_ok=True)
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o700)
        binary = self.root / "bin"
        binary.mkdir()
        cargo = binary / "cargo"
        cargo.write_text(
            "#!/bin/sh\nprintf '%s\\n' \"$AW_TOKENLESS_TEST_CLI\" "
            '"$AW_TOKENLESS_TEST_RTK" >> "$AW_PACKAGE_CAPTURE"\n'
        )
        cargo.chmod(0o700)
        capture = self.root / "paths"
        environment = {
            **os.environ,
            "PATH": str(binary),
            "GITHUB_WORKSPACE": str(workspace),
            "AW_PACKAGE_CAPTURE": str(capture),
            "AW_TOKENLESS_TEST_CLI": "/host/runner/src/tokenless/target/release/tokenless",
            "AW_TOKENLESS_TEST_RTK": "/host/runner/src/aw/target/release/rtk",
        }
        subprocess.run(
            ["/bin/sh", "-eu", "-c", script],
            env=environment,
            cwd=self.root,
            check=True,
            timeout=10,
        )
        self.assertEqual(capture.read_text().splitlines(), [str(p) for p in expected] * 2)


class ScopeTests(GateFixture):
    def git(self, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE, timeout=10
        ).strip()

    def commit(self, path: str, text: str) -> str:
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text, encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def init_git(self) -> str:
        self.git("init", "-q", "--initial-branch=main")
        self.git("config", "user.name", "AW fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        return self.commit("README.md", "base")

    def test_scope_uses_full_pr_without_base_only_changes(self) -> None:
        base = self.init_git()
        self.git("checkout", "-qb", "feature")
        self.commit("src/aw/schema with spaces.json", "{}")
        head = self.commit("other.txt", "unrelated last commit")
        event = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertTrue(gate.scope("pull_request", event, head, self.root))

        self.git("checkout", "-qb", "unrelated", base)
        head = self.commit("other.txt", "unrelated")
        self.git("checkout", "main")
        advanced = self.commit("src/aw/Cargo.lock", "base advanced")
        self.git("merge", "--no-edit", "unrelated")
        candidate = self.git("rev-parse", "HEAD")
        event["pull_request"] = {"base": {"sha": advanced}, "head": {"sha": head}}
        self.assertFalse(gate.scope("pull_request", event, candidate, self.root))

        self.git("checkout", "-qb", "workflow-only", advanced)
        workflow_head = self.commit(".github/workflows/aw-ci.yml", "workflow only")
        event["pull_request"] = {"base": {"sha": advanced}, "head": {"sha": workflow_head}}
        self.assertFalse(gate.scope("pull_request", event, workflow_head, self.root))

    def test_scope_handles_removal_noop_and_event_errors(self) -> None:
        base = self.init_git()
        head = self.commit("other.txt", "no AW change")
        self.assertFalse(gate.scope("push", {"before": base, "after": head}, head, self.root))
        before = self.commit("src/aw/schema.json", "{}")
        self.git("mv", "src/aw/schema.json", "schema.json")
        self.git("commit", "-qm", "move out of AW")
        after = self.git("rev-parse", "HEAD")
        self.assertTrue(gate.scope("push", {"before": before, "after": after}, after, self.root))
        self.assertTrue(gate.scope("workflow_dispatch", {}, after, self.root))
        self.assertTrue(gate.scope("push", {"before": "0" * 40, "after": after}, after, self.root))
        forced = {"before": "1" * 40, "after": after, "forced": True}
        self.assertTrue(gate.scope("push", forced, after, self.root))
        with self.assertRaises(ValueError):
            gate.scope("push", forced, base, self.root)
        with self.assertRaises(subprocess.CalledProcessError):
            gate.scope("push", {"before": "1" * 40, "after": after}, after, self.root)
        with self.assertRaises(ValueError):
            gate.scope("push", {"before": before, "after": base}, after, self.root)
        for event in ("pull_request_target", "unknown"):
            with self.assertRaises(ValueError):
                gate.scope(event, {}, after, self.root)
        with patch.dict(os.environ, {"GITHUB_SHA": base}):
            with self.assertRaises(ValueError):
                gate.candidate(self.root)
        for invalid in ("HEAD", "--help", "a" * 39, "a" * 40 + "\n"):
            with self.assertRaises(ValueError):
                gate.sha(invalid)

    def test_provider_only_changes_select_the_gate(self) -> None:
        base = self.init_git()
        head = self.commit("providers/tokenless/src/lib.rs", "Provider change")
        event = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertTrue(gate.scope("pull_request", event, head, self.root))
        self.git("rm", "providers/tokenless/src/lib.rs")
        self.git("commit", "-qm", "remove Provider")
        removed = self.git("rev-parse", "HEAD")
        self.assertTrue(gate.scope("push", {"before": head, "after": removed}, removed, self.root))

    def test_merge_group_selects_only_aw_changes(self) -> None:
        base = self.init_git()
        head = self.commit("src/tokenless/unrelated.txt", "unrelated")
        event = {"merge_group": {"base_sha": base, "head_sha": head}}
        self.assertFalse(gate.scope("merge_group", event, head, self.root))
        head = self.commit(".github/workflows/aw-ci.yml", "gate change")
        event["merge_group"]["head_sha"] = head
        self.assertTrue(gate.scope("merge_group", event, head, self.root))
        with self.assertRaises(ValueError):
            gate.scope("merge_group", event, base, self.root)

    def test_required_result_truth_table(self) -> None:
        sha = "a" * 40
        valid = {
            "GITHUB_SHA": sha,
            "SCOPE_RESULT": "success",
            "CHECKS_RESULT": "success",
            "SELECTED": "true",
            "CANDIDATE_SHA": sha,
            "TESTED_SHA": sha,
        }
        gate.required(valid)
        gate.required({**valid, "SELECTED": "false", "CHECKS_RESULT": "skipped", "TESTED_SHA": ""})
        failures = [{key: ""} for key in valid]
        failures += [{"CHECKS_RESULT": value} for value in ("failure", "cancelled", "skipped")]
        failures += [
            {"SCOPE_RESULT": "failure"},
            {"SELECTED": "false"},
            {"TESTED_SHA": "b" * 40},
            {"CANDIDATE_SHA": "b" * 40},
            {"SELECTED": "unknown"},
        ]
        for changes in failures:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                gate.required({**valid, **changes})


class GateTests(GateFixture):
    def test_structure_enforces_crate_boundaries_and_source_limits(self) -> None:
        from contextlib import chdir

        self.root = self.root / "src/aw"
        self.root.mkdir(parents=True)
        core = self.root / "crates/aw-core"
        provider_directories = {
            "aw-provider-sec-core": self.root.parents[1] / "providers/agent-sec-core",
            "aw-provider-tokenless": self.root.parents[1] / "providers/tokenless",
        }
        packages = []
        for name, directory, dependencies in (
            ("aw-contracts", self.root / "crates/aw-contracts", ["serde_json"]),
            ("aw-core", core, ["aw-contracts", "serde_json", "thiserror"]),
            (
                "aw-config",
                self.root / "crates/aw-config",
                ["jsonschema", "serde", "serde_json", "serde_yaml_ng", "thiserror"],
            ),
            (
                "aw-provider",
                self.root / "crates/aw-provider",
                ["aw-config", "jsonschema", "serde", "serde_json", "sha2", "thiserror"],
            ),
            ("aw-exec", self.root / "crates/aw-exec", ["libc", "thiserror"]),
            (
                "aw-host", self.root / "crates/aw-host",
                ["aw-config", "aw-exec", "aw-provider", "serde_json", "sha2", "thiserror"],
            ),
            (
                "aw-service", self.root / "crates/aw-service",
                ["aw-config", "aw-core", "aw-exec", "aw-host", "aw-provider", "libc", "aw-adapter", "serde", "serde_json", "serde_yaml_ng", "sha2", "thiserror"],
            ),
            (
                "aw-provider-sec-core", self.root.parents[1] / "providers/agent-sec-core",
                ["aw-exec", "aw-provider", "serde", "serde_json", "thiserror", "libc"],
            ),
            (
                "aw-package", self.root / "crates/aw-package",
                ["aw-config", "aw-exec", "aw-service", "serde", "serde_json", "serde_yaml_ng", "sha2", "thiserror", "libc", "flate2", "tar", "tempfile"],
            ),
            (
                "aw-provider-tokenless", self.root.parents[1] / "providers/tokenless",
                ["aw-exec", "aw-provider", "serde", "serde_json", "thiserror", "tempfile", "libc", "shlex", "aw-provider-sec-core"],
            ),
            ("aw-adapter", self.root / "crates/aw-adapter",
             ["aw-config", "aw-contracts", "aw-exec", "aw-provider", "serde_json"]),
            (
                "aw-cli", self.root / "crates/aw-cli",
                ["aw-adapter", "aw-config", "aw-contracts", "aw-exec", "aw-host", "aw-package",
                 "aw-provider", "aw-service", "clap", "clap_complete", "libc", "serde", "serde_json",
                 "serde_yaml_ng", "sha2", "tempfile"],
            ),
        ):
            (directory / "src").mkdir(parents=True)
            (directory / "src/lib.rs").write_text("//! Fixture.\n", encoding="utf-8")
            binary = {"aw-cli": "aw", "aw-package": "aw-build"}.get(name)
            if name in provider_directories:
                binary = name
            targets = [{"src_path": str(directory / "src/lib.rs"), "kind": ["lib"]}]
            if binary:
                targets.append({"name": binary, "kind": ["bin"],
                                "src_path": str(directory / "src/bin" / f"{binary}.rs")})
            packages.append(
                {
                    "id": name,
                    "name": name,
                    "manifest_path": str(directory / "Cargo.toml"),
                    "targets": targets,
                    "dependencies": [
                        {
                            "name": dependency,
                            "path": (
                                str(provider_directories.get(dependency, self.root / "crates" / dependency))
                                if dependency.startswith("aw-") else None
                            ),
                            "source": None if dependency.startswith("aw-") else "registry+fixture",
                        }
                        for dependency in dependencies
                    ],
                }
            )
        metadata = {
            "packages": packages,
            "workspace_members": [p["id"] for p in packages if p["name"] not in provider_directories],
            "provider_workspaces": [
                {"packages": [p], "workspace_members": [p["id"]],
                 "workspace_root": str(provider_directories[p["name"]])}
                for p in packages if p["name"] in provider_directories
            ],
        }
        gate.structure(metadata, self.root)
        for name in ("aw-cli", "aw-package", "aw-core", "aw-provider-sec-core"):
            invalid = deepcopy(metadata)
            package = next(p for p in invalid["packages"] if p["name"] == name)
            package["targets"].append({"name": "aw-package", "kind": ["bin"],
                                       "src_path": str(Path(package["manifest_path"]).parent / "src/bin/aw-package.rs")})
            with self.subTest(binary_owner=name), self.assertRaisesRegex(ValueError, "executable targets changed"):
                gate.structure(invalid, self.root)
        for name in ("aw-cli", "aw-package", "aw-provider-sec-core", "aw-provider-tokenless"):
            invalid = deepcopy(metadata)
            package = next(p for p in invalid["packages"] if p["name"] == name)
            package["targets"] = [target for target in package["targets"] if "bin" not in target["kind"]]
            with self.subTest(missing_binary=name), self.assertRaisesRegex(ValueError, "executable targets changed"):
                gate.structure(invalid, self.root)
        for workspaces in ([], metadata["provider_workspaces"][:1],
                           metadata["provider_workspaces"][:1] * 2):
            with self.assertRaises(ValueError):
                gate.structure({**metadata, "provider_workspaces": workspaces}, self.root)
        invalid = deepcopy(metadata)
        invalid["workspace_members"].append("aw-provider-sec-core")
        with self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        invalid = deepcopy(metadata)
        invalid["provider_workspaces"][0]["workspace_root"] = str(self.root)
        with self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        for package, dependency in (
            (0, "aw-core"), (1, "tokio"), (1, "semver"), (2, "aw-core"), (2, "aw-contracts"),
            (3, "aw-core"), (4, "aw-core"), (4, "aw-provider"), (4, "aw-config"),
            (5, "aw-core"), (5, "aw-contracts"), (5, "libc"),
            (6, "aw-contracts"), (6, "tokio"), (6, "aw-provider-tokenless"),
            (7, "asc-daemon-client"), (7, "aw-core"),
            (10, "aw-service"), (10, "aw-host"), (10, "aw-provider-tokenless"),
            (11, "aw-provider-tokenless"), (11, "aw-provider-sec-core"),
            (6, "clap_complete"), (10, "clap_complete"),
        ):
            invalid = deepcopy(metadata)
            invalid["packages"][package]["dependencies"].append({"name": dependency})
            with self.assertRaises(ValueError):
                gate.structure(invalid, self.root)
        for package in metadata["packages"]:
            if package["name"] == "aw-cli":
                continue
            invalid = deepcopy(metadata)
            library = next(p for p in invalid["packages"] if p["name"] == package["name"])
            library["dependencies"].append({"name": "aw-cli"})
            with self.subTest(library=package["name"]), self.assertRaisesRegex(
                ValueError, "unreviewed dependency aw-cli"
            ):
                gate.structure(invalid, self.root)
        invalid = deepcopy(metadata)
        invalid["packages"][1]["dependencies"][0]["path"] = str(self.root / "other-contracts")
        with self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        invalid["packages"][1]["dependencies"][0] = {
            "name": "aw-contracts",
            "source": "registry+fixture",
        }
        with chdir(self.root), self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        invalid = deepcopy(metadata)
        invalid["packages"][3]["dependencies"][0]["path"] = str(self.root / "other-config")
        with self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        for dependency in ("aw-config", "aw-exec", "aw-provider"):
            invalid = deepcopy(metadata)
            local = next(d for d in invalid["packages"][5]["dependencies"] if d["name"] == dependency)
            local["path"] = str(self.root / "other-local-crate")
            with self.subTest(dependency=dependency), self.assertRaises(ValueError):
                gate.structure(invalid, self.root)
        with self.assertRaises(ValueError):
            gate.structure({**metadata, "workspace_members": ["aw-contracts"]}, self.root)
        invalid = deepcopy(metadata)
        invalid["packages"][1]["targets"][0]["src_path"] = str(core / "lib.rs")
        with self.assertRaises(ValueError):
            gate.structure(invalid, self.root)
        for name, directory in (
            ("aw-provider-sec-core", "crates/aw-provider-sec-core"),
            ("aw-provider-tokenless", "crates/aw-provider-tokenless"),
            ("aw-provider", "providers/protocol"),
        ):
            invalid = deepcopy(metadata)
            package = next(p for p in invalid["packages"] if p["name"] == name)
            package["manifest_path"] = str(self.root / directory / "Cargo.toml")
            with self.subTest(package=name), self.assertRaises(ValueError):
                gate.structure(invalid, self.root)
        # Contracts must be a regular crate; root-level manifests and targets fail.
        for field, value in (
            ("manifest_path", str(self.root / "Cargo.toml")),
            ("targets", [{"src_path": str(self.root / "src/lib.rs")}]),
        ):
            invalid = deepcopy(metadata)
            invalid["packages"][0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                gate.structure(invalid, self.root)
        source = core / "src/lib.rs"
        source.write_text("// fixture\n" * 700, encoding="utf-8")
        with self.assertRaises(ValueError):
            gate.structure(metadata, self.root)
        source.write_text("// fixture\n" * 699, encoding="utf-8")
        legacy = self.root / "crates/aw-contracts/src/validation.rs"
        legacy.write_text("// fixture\n" * 711, encoding="utf-8")
        gate.structure(metadata, self.root)
        legacy.write_text("// fixture\n" * 712, encoding="utf-8")
        with self.assertRaises(ValueError):
            gate.structure(metadata, self.root)
        legacy.unlink()
        module = core / "src/mod.rs"
        module.touch()
        with self.assertRaises(ValueError):
            gate.structure(metadata, self.root)
        module.unlink()
        common = core / "tests/common/mod.rs"
        common.parent.mkdir(parents=True)
        common.touch()
        gate.structure(metadata, self.root)
        alias = core / "src/alias.rs"
        alias.symlink_to(source)
        with self.assertRaises(ValueError):
            gate.structure(metadata, self.root)

    def test_commands_fail_and_stop_the_sequence(self) -> None:
        commands = self.root / "commands.jsonl"
        cargo = self.root / "cargo"
        cargo.write_text(
            f"#!{sys.executable}\nimport json, sys\n"
            f"with open({str(commands)!r}, 'a') as out: out.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "if sys.argv[1] == 'fmt': sys.exit(7)\n",
            encoding="utf-8",
        )
        cargo.chmod(0o755)
        with patch.dict(os.environ, {"PATH": str(self.root)}), patch.object(
            gate, "AW", self.root
        ), patch.object(gate, "candidate", return_value="a" * 40), patch.object(
            gate, "selftest"
        ), self.assertRaises(
            subprocess.CalledProcessError
        ) as caught:
            gate.check()
        self.assertEqual(caught.exception.returncode, 7)
        self.assertEqual(
            [json.loads(line)[0] for line in commands.read_text().splitlines()], ["fmt"]
        )
        with self.assertRaises(FileNotFoundError):
            gate.run([str(self.root / "missing-tool")], self.root)

    def test_selftest_rejects_missing_empty_and_skipped_discovery(self) -> None:
        tests = self.root / "tests"
        tests.mkdir()
        module = tests / "test_ci_checks.py"
        for scope_only in (False, True):
            with self.subTest(scope_only=scope_only), patch.object(gate, "AW", self.root):
                module.unlink(missing_ok=True)
                with self.assertRaises(subprocess.CalledProcessError):
                    gate.selftest(scope_only=scope_only)
                module.write_text("# Empty test module\n", encoding="utf-8")
                with self.assertRaises(subprocess.CalledProcessError):
                    gate.selftest(scope_only=scope_only)
                name = "ScopeTests" if scope_only else "Fixture"
                module.write_text(
                    f"import unittest\nclass {name}(unittest.TestCase): pass\n",
                    encoding="utf-8",
                )
                with self.assertRaises(subprocess.CalledProcessError):
                    gate.selftest(scope_only=scope_only)
                module.write_text(
                    f"import unittest\n@unittest.skip('fixture')\nclass {name}(unittest.TestCase):\n"
                    "    def test_skipped(self): self.fail('must not run')\n",
                    encoding="utf-8",
                )
                with self.assertRaises(subprocess.CalledProcessError):
                    gate.selftest(scope_only=scope_only)
                if scope_only:
                    module.write_text(
                        "import unittest\nclass UnrelatedTests(unittest.TestCase):\n"
                        "    def test_present(self): self.assertTrue(True)\n",
                        encoding="utf-8",
                    )
                    with self.assertRaises(subprocess.CalledProcessError):
                        gate.selftest(scope_only=True)
                module.write_text(
                    f"import unittest\nclass {name}(unittest.TestCase):\n"
                    "    def test_present(self): self.assertTrue(True)\n",
                    encoding="utf-8",
                )
                gate.selftest(scope_only=scope_only)

    def test_scope_selftests_run_with_sparse_files_and_no_node(self) -> None:
        sparse = self.root / "sparse"
        (sparse / "scripts").mkdir(parents=True)
        (sparse / "tests").mkdir()
        shutil.copyfile(AW / "scripts/check.py", sparse / "scripts/check.py")
        module = sparse / "tests/test_ci_checks.py"
        source = Path(__file__).read_text(encoding="utf-8")
        # A mistaken full discovery must fail immediately, without recursively
        # launching this integration fixture again.
        module.write_text(
            source + "\nclass GateTests(unittest.TestCase):\n"
            "    def test_full_gate_must_not_run(self):\n"
            "        self.fail('full gate selected for sparse scope checkout')\n",
            encoding="utf-8",
        )
        tools = self.root / "bin"
        tools.mkdir()
        git = shutil.which("git")
        self.assertIsNotNone(git, "scope fixtures require Git")
        (tools / "git").symlink_to(git)
        self.assertIsNone(shutil.which("node", path=str(tools)))
        self.assertFalse((sparse / "tests/fixtures").exists())
        with patch.dict(os.environ, {"PATH": str(tools)}), patch.object(gate, "AW", sparse):
            gate.selftest(scope_only=True)

    def test_timeout_stops_an_ignoring_descendant(self) -> None:
        pid_file = self.root / "child.pid"
        child = (
            "import os, signal, time; from pathlib import Path; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)"
        )
        parent = (
            "import signal, subprocess, sys, time; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"subprocess.Popen([sys.executable, '-c', {child!r}]); time.sleep(60)"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            gate.run([sys.executable, "-c", parent], self.root, timeout=1)
        self.assertTrue(pid_file.exists(), "descendant did not start before timeout")
        pid = int(pid_file.read_text())
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            status = Path(f"/proc/{pid}/stat")
            try:
                state = status.read_text().split()[2]
            except (FileNotFoundError, ProcessLookupError):
                # Reaping can remove the process during open or the subsequent read.
                break
            if state == "Z":
                break
            time.sleep(0.02)
        else:
            os.kill(pid, signal.SIGKILL)
            self.fail("owned descendant remained running after timeout")

    def _simulate_descendant_status(self, status: Union[str, OSError]) -> None:
        original_read_text = Path.read_text
        pid = 424242

        def read_text(path: Path, *args: object, **kwargs: object) -> str:
            if path == self.root / "child.pid":
                return str(pid)
            if path == Path(f"/proc/{pid}/stat"):
                if isinstance(status, OSError):
                    raise status
                return f"{pid} (fixture) {status}"
            return original_read_text(path, *args, **kwargs)

        with (
            patch.object(gate, "run", side_effect=subprocess.TimeoutExpired("fixture", 1)),
            patch.object(Path, "exists", return_value=True),
            patch.object(Path, "read_text", read_text),
            patch.object(time, "monotonic", side_effect=[0.0, 0.1, 2.1]),
            patch.object(time, "sleep"),
            patch.object(os, "kill") as kill,
        ):
            try:
                self.test_timeout_stops_an_ignoring_descendant()
            finally:
                if status == "S":
                    kill.assert_called_once_with(pid, signal.SIGKILL)
                else:
                    kill.assert_not_called()

    def test_descendant_disappearing_during_stat_read_is_stopped(self) -> None:
        for error in (
            FileNotFoundError("reaped before open"),
            ProcessLookupError("reaped during read"),
        ):
            with self.subTest(error=type(error).__name__):
                self._simulate_descendant_status(error)

    def test_descendant_zombie_is_stopped(self) -> None:
        self._simulate_descendant_status("Z")

    def test_descendant_read_failure_remains_an_error(self) -> None:
        for error in (PermissionError("cannot inspect process"), OSError("read failed")):
            with self.subTest(error=type(error).__name__), self.assertRaises(type(error)):
                self._simulate_descendant_status(error)

    def test_descendant_still_running_remains_a_failure(self) -> None:
        with self.assertRaisesRegex(AssertionError, "owned descendant remained running"):
            self._simulate_descendant_status("S")

    def test_canonical_vectors_fail_even_with_python_optimization(self) -> None:
        script = self.root / "tests/check_canonical.py"
        script.parent.mkdir()
        shutil.copyfile(AW / "tests/check_canonical.py", script)
        fixtures = self.root / "crates/aw-contracts/tests/fixtures"
        fixtures.mkdir(parents=True)
        original = json.loads((AW / "crates/aw-contracts/tests/fixtures/canonical-vectors.json").read_text())
        damaged = json.loads(json.dumps(original))
        damaged[0]["digest"] = "0" * 64
        for vector, valid in ((original, True), ([], False), ({}, False), (damaged, False)):
            (fixtures / "canonical-vectors.json").write_text(json.dumps(vector), encoding="utf-8")
            for optimization in ([], ["-O"]):
                with self.subTest(valid=valid, optimization=optimization):
                    result = subprocess.run(
                        [sys.executable, *optimization, str(script)],
                        capture_output=True,
                        text=True,
                        timeout=20,
                    )
                    self.assertEqual(result.returncode == 0, valid, result.stderr)
                    if vector == damaged:
                        self.assertIn("Python canonical digest differs", result.stderr)


if __name__ == "__main__":
    unittest.main()
