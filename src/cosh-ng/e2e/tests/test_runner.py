import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest import mock

RUNNER_PATH = pathlib.Path(__file__).parents[1] / "run.py"
SPEC = importlib.util.spec_from_file_location("cosh_e2e_runner", RUNNER_PATH)
assert SPEC and SPEC.loader
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class ManifestTests(unittest.TestCase):
    def test_repository_manifest_is_valid(self):
        manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
        self.assertEqual(set(manifest["profiles"]), {"local", "g2", "g3", "g4", "g5"})
        self.assertEqual([case["id"] for case in manifest["cases"]], [f"E2E-{index:02d}" for index in range(1, 9)])
        schema = RUNNER.load_result_schema()
        self.assertEqual(schema["properties"]["cases"]["items"]["properties"]["status"]["enum"], ["PASS", "FAIL", "BLOCKED", "FLAKY"])

    def test_duplicate_case_id_is_rejected(self):
        manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
        manifest["cases"].append(dict(manifest["cases"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            RUNNER.validate_manifest(manifest)

    def test_local_plan_selects_install_and_pty_cases(self):
        manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
        args = RUNNER.parse_args(["--profile", "local", "--plan"])
        self.assertEqual([case["id"] for case in RUNNER.select_cases(manifest, args)], ["E2E-01", "E2E-02"])

    def test_plan_reports_runner_level_requirements(self):
        manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
        args = RUNNER.parse_args(
            [
                "--profile",
                "local",
                "--plan",
                "--cosh-bin",
                "/definitely/missing/cosh",
            ]
        )
        cases = RUNNER.select_cases(manifest, args)
        with mock.patch.object(RUNNER.shutil, "which", return_value=None):
            missing = {
                case["id"]: RUNNER.missing_requirements(
                    case, pathlib.Path(args.cosh_bin)
                )
                for case in cases
            }
        self.assertEqual(missing["E2E-01"], ["executable:/definitely/missing/cosh"])
        self.assertEqual(
            missing["E2E-02"],
            ["executable:/definitely/missing/cosh", "shell-use"],
        )

    def test_soak_case_requires_shell_use(self):
        manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
        soak = next(case for case in manifest["cases"] if case["id"] == "E2E-08")
        with mock.patch.object(RUNNER.shutil, "which", return_value=None):
            self.assertIn("shell-use", RUNNER.missing_requirements(soak))

    def test_process_snapshot_counts_post_exec_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            args = RUNNER.parse_args(["--artifact-dir", str(pathlib.Path(directory) / "bundle")])
            runner = RUNNER.Runner(args, RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json"))
            ps_output = "\n".join(
                [
                    "  101 2048 /usr/libexec/cosh/cosh-shell raw cosh-core",
                    "  102 1024 /usr/libexec/cosh/cosh-core --registry",
                    "  103  512 /usr/bin/cosh -c true",
                    "  104  256 tail -f /var/log/cosh-shell.log",
                    "  105  128 python3 e2e/run.py --profile g3",
                ]
            )
            fake = mock.Mock(stdout=ps_output)
            with mock.patch.object(RUNNER.subprocess, "run", return_value=fake):
                snapshot = runner.process_snapshot()
        self.assertEqual(snapshot["processes"], 3)
        self.assertEqual(snapshot["rss_kib"], 2048 + 1024 + 512)

    def test_soak_reports_nearest_rank_p95(self) -> None:
        # Small samples expose rounding drift; 20 and 40 pin exact ranks.
        for sample_count, expected in ((0, 0), (1, 1), (5, 5), (20, 19), (29, 28), (40, 38)):
            with self.subTest(samples=sample_count), tempfile.TemporaryDirectory() as directory:
                args = RUNNER.parse_args(["--profile", "g3", "--artifact-dir", directory])
                manifest = RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json")
                manifest["profiles"]["g3"] = {
                    "duration_seconds": 0,
                    "minimum_cycles": sample_count,
                }
                runner = RUNNER.Runner(args, manifest)
                self.addCleanup(runner.home.rmdir)
                case = next(case for case in manifest["cases"] if case["id"] == "E2E-08")
                # Descending latencies also require the production path to sort.
                ticks = [0]
                now = 0
                for latency in range(sample_count, 0, -1):
                    ticks.extend((now, now + latency))
                    now += latency
                ticks.append(now)
                snapshot = {"processes": 0, "rss_kib": 0, "fds": 0}
                completed = mock.Mock(returncode=0, stdout="COSH_E2E_SOAK\n")
                with (
                    mock.patch.object(RUNNER.time, "monotonic", side_effect=ticks),
                    mock.patch.object(runner, "process_snapshot", return_value=snapshot),
                    mock.patch.object(runner, "pty", return_value=("PASS", "", [])),
                    mock.patch.object(runner, "command", return_value=completed),
                ):
                    status, reason, evidence = runner.soak(case)
                self.assertEqual((status, reason), ("PASS", ""))
                metrics = json.loads((runner.bundle / evidence[0]).read_text(encoding="utf-8"))
                self.assertEqual(metrics["cycles"], sample_count)
                self.assertEqual(metrics["p95_latency_seconds"], expected)
                self.assertEqual(metrics["max_latency_seconds"], sample_count)

    def test_cleanup_only_uses_persisted_state(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = pathlib.Path(directory) / "bundle"
            home = pathlib.Path(directory) / "home"
            bundle.mkdir()
            home.mkdir()
            (bundle / "cleanup-state.json").write_text(
                json.dumps({"home": str(home), "sessions": []}), encoding="utf-8"
            )
            args = RUNNER.parse_args(["--cleanup-only", "--artifact-dir", str(bundle)])
            runner = RUNNER.Runner(args, RUNNER.load_manifest(RUNNER_PATH.parent / "manifest.json"))
            self.assertEqual(runner.cleanup()["status"], "PASS")
            self.assertFalse(home.exists())

    def cleanup_bundle(self, directory: str, sessions: list[str]) -> pathlib.Path:
        bundle = pathlib.Path(directory) / "bundle"
        home = pathlib.Path(directory) / "home"
        bundle.mkdir()
        home.mkdir()
        (bundle / "cleanup-state.json").write_text(
            json.dumps({"home": str(home), "sessions": sessions}), encoding="utf-8"
        )
        return bundle

    def run_cleanup_only(self, bundle: pathlib.Path, close: mock.Mock) -> tuple[int, dict]:
        output = io.StringIO()
        with (
            mock.patch.object(RUNNER.subprocess, "run", return_value=close),
            redirect_stdout(output),
        ):
            code = RUNNER.main(["--cleanup-only", "--artifact-dir", str(bundle)])
        return code, json.loads(output.getvalue())

    def test_cleanup_only_failed_session_close_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.cleanup_bundle(directory, ["cosh-e2e-session"])
            code, report = self.run_cleanup_only(bundle, mock.Mock(returncode=1))
            state = json.loads((bundle / "cleanup-state.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "FAIL")
        self.assertEqual(
            report["actions"],
            [
                "close shell-use session cosh-e2e-session",
                f"remove isolated HOME {bundle.parent / 'home'}",
            ],
        )
        self.assertEqual(state["status"], "FAIL")

    def test_cleanup_only_successful_session_close_exits_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.cleanup_bundle(directory, ["cosh-e2e-session"])
            code, report = self.run_cleanup_only(bundle, mock.Mock(returncode=0))
            state = json.loads((bundle / "cleanup-state.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "PASS")
        self.assertEqual(state["status"], "PASS")


if __name__ == "__main__":
    unittest.main()
