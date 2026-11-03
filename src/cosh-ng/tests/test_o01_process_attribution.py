import copy
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

MODULE_PATH = Path(__file__).parents[1] / "scripts" / "o01_process_attribution.py"
SPEC = importlib.util.spec_from_file_location("o01_process_attribution", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class AttributionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.proc = Path(self.temporary.name) / "proc"
        self.groups = Path(self.temporary.name) / "cgroups"
        self.proc.mkdir()
        self.groups.mkdir()
        self.group = self.groups / "cosh-o01-owned"
        self.group.mkdir()
        self.write_process(100, 9, 20, "cosh", ["-cosh"], MODULE.SHELL_EXE)
        self.write_process(101, 100, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
        self.held = MODULE.capture_held_root(
            100, self.group, proc_root=self.proc, cgroup_root=self.groups
        )
        self.assertIsNotNone(self.held)
        self.busy = {"pid": 101, "start_ticks": 30, "name": "dnf", "cpu_percent": 4.0}

    def write_process(
        self,
        pid: int,
        ppid: int,
        start_ticks: int,
        name: str,
        argv: list[str],
        executable: str,
        cgroup: str = "0::/cosh-o01-owned",
    ) -> None:
        directory = self.proc / str(pid)
        directory.mkdir(exist_ok=True)
        fields = ["0"] * 22
        fields[0], fields[1], fields[19] = "S", str(ppid), str(start_ticks)
        (directory / "stat").write_text(f"{pid} ({name}) " + " ".join(fields))
        (directory / "cgroup").write_text(cgroup + "\n")
        (directory / "cmdline").write_bytes(b"\0".join(x.encode() for x in argv) + b"\0")
        link = directory / "exe"
        link.unlink(missing_ok=True)
        link.symlink_to(executable)

    def classify(self, busy: list[dict] | None = None, held: list[dict] | None = None) -> dict:
        return MODULE.attribute_busy_processes(
            [self.busy] if busy is None else busy,
            [self.held] if held is None else held,
            proc_root=self.proc,
            cgroup_root=self.groups,
        )

    def assert_blocked(self, result: dict) -> None:
        self.assertEqual(len(result["blocking_processes"]), 1)
        self.assertTrue(result["observations"][0]["blocking"])
        self.assertNotEqual(result["observations"][0]["classification"], "candidate-update-probe")

    def test_owned_exact_probe_retains_original_busy_record_and_cpu(self) -> None:
        original = copy.deepcopy(self.busy)
        result = self.classify()
        self.assertEqual(result["blocking_processes"], [])
        row = result["observations"][0]
        self.assertEqual(row["classification"], "candidate-update-probe")
        self.assertEqual(row["process"], original)
        self.assertEqual(self.busy, original)
        self.assertEqual(row["evidence"]["child"]["argv"], list(MODULE.PROBE_ARGV))
        self.assertEqual(row["evidence"]["root"]["before"]["start_ticks"], 20)

    def test_same_exe_and_argv_outside_held_group_remains_blocking(self) -> None:
        (self.groups / "external").mkdir()
        self.write_process(
            101, 9, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE, "0::/external"
        )
        self.assert_blocked(self.classify())

    def test_whitespace_cgroup_sibling_preserves_membership_and_blocks(self) -> None:
        for suffix in (" ", "\t", "\r"):
            with self.subTest(suffix=repr(suffix)):
                sibling = self.groups / ("cosh-o01-owned" + suffix)
                sibling.mkdir()
                membership = "0::/" + sibling.name
                self.write_process(
                    101, 100, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE, membership
                )
                result = self.classify()
                self.assert_blocked(result)
                child = result["observations"][0]["evidence"]["child"]
                self.assertEqual(child["before"]["cgroup"], membership)

    def test_nearby_cgroup_prefix_is_not_owned(self) -> None:
        (self.groups / "cosh-o01-owned-extra").mkdir()
        (self.proc / "101/cgroup").write_text("0::/cosh-o01-owned-extra\n")
        self.assert_blocked(self.classify())

    def test_only_the_observed_command_is_recognized(self) -> None:
        alternatives = [
            ["/usr/bin/dnf", "check-update", "cosh-ng"],
            ["/usr/bin/python3", "/usr/bin/dnf", "--cacheonly", "check-update", "cosh-ng"],
            ["/usr/bin/python3", "/usr/bin/dnf", "check-update", "other"],
            ["/usr/bin/python3", "/usr/bin/dnf", "remove", "cosh-ng"],
            ["/usr/bin/python3", "/tmp/dnf", "check-update", "cosh-ng"],
            ["/usr/bin/python", "/usr/bin/dnf", "check-update", "cosh-ng"],
            ["/usr/bin/python3", "/usr/bin/dnf", "check-update", "cosh-ng", ""],
        ]
        for argv in alternatives:
            with self.subTest(argv=argv):
                self.write_process(101, 100, 30, "dnf", argv, MODULE.PROBE_EXE)
                self.assert_blocked(self.classify())

    def test_unexpected_interpreter_is_not_accepted(self) -> None:
        for executable in [
            "/tmp/python3.11",
            "/usr/bin/python3.12",
            "/usr/bin/dnf",
            MODULE.PROBE_EXE + " (deleted)",
        ]:
            with self.subTest(executable=executable):
                self.write_process(101, 100, 30, "dnf", list(MODULE.PROBE_ARGV), executable)
                self.assert_blocked(self.classify())

    def test_other_busy_processes_in_owned_group_are_not_exempted(self) -> None:
        for name in ["cargo", "rustc", "rpm", "rpmbuild", "unknown"]:
            with self.subTest(name=name):
                self.assert_blocked(self.classify([dict(self.busy, name=name)]))

    def test_owned_probe_does_not_hide_another_busy_process(self) -> None:
        foreign = {"pid": 404, "name": "cargo", "cpu_percent": 99.0}
        result = self.classify([self.busy, foreign])
        self.assertEqual(result["blocking_processes"], [foreign])
        self.assertEqual(len(result["observations"]), 2)

    def test_missing_or_invalid_busy_identity_fails_closed(self) -> None:
        for key in ["pid", "start_ticks"]:
            for value in [None, 0, -1, True, "30"]:
                with self.subTest(key=key, value=value):
                    record = dict(self.busy)
                    if value is None:
                        record.pop(key)
                    else:
                        record[key] = value
                    self.assert_blocked(self.classify([record]))

    def test_missing_held_identity_fields_fail_closed(self) -> None:
        for key in ["pid", "start_ticks", "cgroup", "cgroup_inode", "cgroup_device"]:
            with self.subTest(key=key):
                held = dict(self.held)
                held.pop(key)
                self.assert_blocked(self.classify(held=[held]))
        self.assert_blocked(self.classify(held=[]))

    def test_invalid_held_identity_types_fail_closed(self) -> None:
        for key in ["pid", "start_ticks", "cgroup_inode", "cgroup_device"]:
            for value in [True, "20", 20.0, None, 0, -1]:
                with self.subTest(key=key, value=value):
                    self.assert_blocked(self.classify(held=[dict(self.held, **{key: value})]))

    def test_proc_name_disagreeing_with_busy_record_fails_closed(self) -> None:
        self.write_process(101, 100, 30, "python", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
        self.assert_blocked(self.classify())

    def test_child_pid_reuse_fails_closed(self) -> None:
        self.write_process(101, 100, 31, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
        self.assert_blocked(self.classify())

    def test_held_root_pid_reuse_fails_closed(self) -> None:
        self.write_process(100, 9, 21, "cosh", ["-cosh"], MODULE.SHELL_EXE)
        self.assert_blocked(self.classify())

    def test_wrong_parent_and_non_candidate_root_fail_closed(self) -> None:
        self.write_process(101, 99, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
        self.assert_blocked(self.classify())
        self.write_process(101, 100, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
        self.write_process(100, 9, 20, "cosh", ["-cosh"], "/tmp/cosh-shell")
        self.assert_blocked(self.classify())

    def test_wrong_root_argv_fails_closed(self) -> None:
        self.write_process(100, 9, 20, "cosh", ["sh", "-c", "dnf"], MODULE.SHELL_EXE)
        self.assert_blocked(self.classify())

    def test_recreated_cgroup_fails_closed(self) -> None:
        self.group.rename(self.groups / "old-group")
        self.group.mkdir()
        self.assert_blocked(self.classify())

    def test_missing_process_files_fail_closed(self) -> None:
        for name in ["stat", "cgroup", "cmdline", "exe"]:
            with self.subTest(name=name):
                self.write_process(101, 100, 30, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
                (self.proc / "101" / name).unlink()
                self.assert_blocked(self.classify())

    def test_incomplete_or_non_utf8_argv_fails_closed(self) -> None:
        for raw in [b"/usr/bin/dnf", b"\xff\0", b"", b"\0"]:
            with self.subTest(raw=raw):
                (self.proc / "101/cmdline").write_bytes(raw)
                self.assert_blocked(self.classify())

    def test_malformed_and_non_v2_cgroups_fail_closed(self) -> None:
        for value in [
            "0::/../cosh-o01-owned",
            "1:cpu:/cosh-o01-owned",
            "0::/",
            "",
            "0::/cosh-o01-owned\n1:cpu:/cosh-o01-owned",
        ]:
            with self.subTest(value=value):
                (self.proc / "101/cgroup").write_text(value)
                self.assert_blocked(self.classify())

    def test_stat_identity_change_during_capture_is_not_stable(self) -> None:
        original = os.readlink

        def reuse(path: Path) -> str:
            self.write_process(101, 100, 31, "dnf", list(MODULE.PROBE_ARGV), MODULE.PROBE_EXE)
            return original(path)

        with mock.patch.object(MODULE.os, "readlink", side_effect=reuse):
            proof = MODULE.observe_process(101, proc_root=self.proc, cgroup_root=self.groups)
        self.assertFalse(proof["stable"])
        self.assertNotEqual(proof["before"]["start_ticks"], proof["after"]["start_ticks"])

    def test_capture_held_root_rejects_wrong_group_and_symlink(self) -> None:
        other = self.groups / "other"
        other.mkdir()
        self.assertIsNone(
            MODULE.capture_held_root(100, other, proc_root=self.proc, cgroup_root=self.groups)
        )
        alias = self.groups / "alias"
        alias.symlink_to(self.group, target_is_directory=True)
        self.assertIsNone(
            MODULE.capture_held_root(100, alias, proc_root=self.proc, cgroup_root=self.groups)
        )

    def test_exec_changed_with_same_pid_start_and_name_is_blocked(self) -> None:
        original = os.readlink

        def change_executable(path: Path) -> str:
            value = original(path)
            if path == self.proc / "101/exe" and value == MODULE.PROBE_EXE:
                path.unlink()
                path.symlink_to("/tmp/external/dnf")
            return value

        with mock.patch.object(MODULE.os, "readlink", side_effect=change_executable):
            self.assert_blocked(self.classify())

    def test_argv_changed_with_same_pid_start_and_name_is_blocked(self) -> None:
        original = Path.read_bytes

        def change_arguments(path: Path) -> bytes:
            value = original(path)
            if path == self.proc / "101/cmdline":
                path.write_bytes(b"dnf\0remove\0cosh-ng\0")
            return value

        with mock.patch.object(Path, "read_bytes", change_arguments):
            self.assert_blocked(self.classify())

    def test_permission_error_is_unknown_and_blocking(self) -> None:
        with mock.patch.object(Path, "read_bytes", side_effect=PermissionError("fixture")):
            self.assert_blocked(self.classify())


if __name__ == "__main__":
    unittest.main()
