"""Exercise package acceptance command selection without compiling or running AW."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import textwrap

from test_ci_checks import AW, GateFixture
from compare_installed import main as compare_installed, packages as installed_packages


class InstalledComparisonTests(GateFixture):
    def report(self, name: str, identity: str) -> Path:
        path = self.root / name
        path.write_text(json.dumps({"format":1,"status":"succeeded","result":{"inventory":{
            "source":"database","installations":[{"installation_id":identity,"prefix":"/" + identity,
            "packages":[{"package_id":"tool","version":"1.2.3","os":"linux","arch":"x86_64",
            "files":{"bin/tool":{"sha256":"a" * 64,"mode":493}},
            "source":{"bundle_sha256":identity,"source_commit":"b" * 40},
            "dependencies":[{"installation_id":identity,"package_id":"base","version":"2.0.0"}]}]}]}}}))
        return path

    def test_comparison_preserves_package_facts_without_comparing_prefix_identity(self) -> None:
        first, second = self.report("first.json", "one"), self.report("second.json", "two")
        self.assertEqual(compare_installed([str(first), str(second)]), 0)
        value = json.loads(second.read_text())
        value["result"]["inventory"]["installations"][0]["packages"][0]["files"]["bin/tool"]["mode"] = 420
        second.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "contents differ"):
            compare_installed([str(first), str(second)])

    def test_comparison_rejects_failed_empty_legacy_and_duplicate_records(self) -> None:
        path = self.report("report.json", "one")
        original = json.loads(path.read_text())
        import copy
        for change in ("failed", "empty", "legacy", "duplicate"):
            value = copy.deepcopy(original)
            inventory = value["result"]["inventory"]
            if change == "failed": value["status"] = "failed"
            elif change == "empty": inventory["installations"][0]["packages"] = []
            elif change == "legacy": inventory["source"] = "legacy"
            else: inventory["installations"][0]["packages"] *= 2
            path.write_text(json.dumps(value))
            with self.subTest(change=change), self.assertRaises(ValueError):
                installed_packages(path)


class BundleCommandTests(GateFixture):
    def prepare(self, missing_launcher: str = "") -> tuple[str, dict[str, str], Path]:
        workflow = (AW.parents[1] / ".github/workflows/aw-preview.yml").read_text()
        step = workflow.split(
            "      - name: Verify installed packages and service lifecycle\n", 1
        )[1].split("      - uses: actions/upload-artifact@v4\n", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        preview = self.root / "preview output"
        packages = preview / "packages"
        packages.mkdir(parents=True)
        capture = self.root / "aw-commands.jsonl"
        launcher = f"#!{sys.executable}\n" + textwrap.dedent('''\
            import json
            import os
            from pathlib import Path
            import shutil
            import sys

            args = sys.argv[1:]
            with open(os.environ["AW_PACKAGE_COMMANDS"], "a") as output:
                output.write(json.dumps({"program": sys.argv[0], "args": args}) + "\\n")
            def option(name):
                return args[args.index(name) + 1]
            if args[0] == "install":
                bundle = Path(option("--bundle"))
                prefix = Path(option("--prefix"))
                assert bundle.is_absolute() and prefix.is_absolute()
                assert bundle / "aw" == Path(sys.argv[0])
                parts = ["core", "sec-core", "tokenless"] if bundle.name.startswith("aw-all-in-one-") else [bundle.name.removeprefix("aw-provider-").removeprefix("aw-").removesuffix("-fixture")]
                records = prefix / ".aw-packages"
                records.mkdir(parents=True, exist_ok=True)
                for part in parts:
                    (records / (part + ".json")).write_text(part)
                if "core" in parts:
                    (prefix / "bin").mkdir(exist_ok=True)
                    shutil.copy2(sys.argv[0], prefix / "bin/aw")
                if "tokenless" in parts:
                    payload = prefix / "libexec/aw/providers/tokenless"
                    payload.mkdir(parents=True, exist_ok=True)
                    for name in ("tokenless", "rtk"):
                        (payload / name).write_text("fixture")
            elif args[:2] == ["config", "init"]:
                config = Path(option("--config"))
                config.write_text("fixture")
                Path(option("--state-dir")).mkdir()
            elif args[:2] == ["config", "validate"]:
                assert Path(option("--config")).is_file()
            elif args[:2] == ["list", "--installed"]:
                prefix = Path(option("--prefix"))
                packages = [{"package_id":path.stem,"version":"1.0.0","os":"linux","arch":"x86_64","files":{},"source":{"source_commit":"a" * 40},"dependencies":[]} for path in sorted((prefix / ".aw-packages").iterdir())]
                print(json.dumps({"format":1,"status":"succeeded","result":{"inventory":{"source":"database","installations":[{"packages":packages}]}}}))
            elif args[0] == "uninstall":
                assert "--yes" in args
                shutil.rmtree(option("--prefix"))
            else:
                raise AssertionError(args)
            ''')
        for name in ("aw-core", "aw-provider-sec-core", "aw-provider-tokenless", "aw-all-in-one"):
            bundle = self.root / "source bundles" / f"{name}-fixture"
            bundle.mkdir(parents=True)
            (bundle / "manifest.json").write_text("{}")
            if name != missing_launcher:
                (bundle / "aw").write_text(launcher)
                (bundle / "aw").chmod(0o755)
            with tarfile.open(packages / f"{name}-fixture.tar.gz", "w:gz") as archive:
                archive.add(bundle, arcname=bundle.name)
        (packages / "SHA256SUMS").write_text("checksums exercised by Linux acceptance")
        binary = self.root / "fixture-bin"
        binary.mkdir()
        commands = {
            "sha256sum": "#!/bin/sh\nexit 0\n",
            "timeout": '#!/bin/sh\nshift\nexec "$@"\n',
            "cargo": f"#!{sys.executable}\n" + textwrap.dedent('''\
                import os
                from pathlib import Path
                if "AW_TEST_WORK_DIR" in os.environ:
                    work = Path(os.environ["AW_TEST_WORK_DIR"])
                    work.mkdir()
                    (work / "result.json").write_text("{}")
                '''),
        }
        for name, content in commands.items():
            path = binary / name
            path.write_text(content)
            path.chmod(0o755)
        environment = {
            **os.environ,
            "PATH": str(binary) + os.pathsep + os.environ["PATH"],
            "AW_PREVIEW_DIR": str(preview),
            "AW_PACKAGE_COMMANDS": str(capture),
            "GITHUB_WORKSPACE": str(self.root / "container workspace"),
        }
        helper = Path(environment["GITHUB_WORKSPACE"]) / "src/aw/scripts/compare_installed.py"
        helper.parent.mkdir(parents=True)
        shutil.copy2(AW / "scripts/compare_installed.py", helper)
        return script, environment, capture

    def test_package_acceptance_uses_each_bundles_aw_and_confirms_every_removal(self) -> None:
        script, environment, capture = self.prepare()
        subprocess.run(
            ["/bin/bash", "-eu", "-c", script], env=environment, cwd=self.root,
            check=True, capture_output=True, timeout=15,
        )
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        installs = [call for call in calls if call["args"][0] == "install"]
        self.assertEqual(len(installs), 6)
        self.assertEqual(
            {Path(call["program"]).parent.name for call in installs},
            {"aw-core-fixture", "aw-provider-sec-core-fixture", "aw-provider-tokenless-fixture", "aw-all-in-one-fixture"},
        )
        removals = [call for call in calls if call["args"][0] == "uninstall"]
        self.assertEqual(len(removals), 4)
        self.assertTrue(all("--yes" in call["args"] for call in removals))
        self.assertEqual(sum(call["args"][:2] == ["config", "init"] for call in calls), 2)
        self.assertEqual(sum(call["args"][:2] == ["config", "validate"] for call in calls), 2)
        self.assertEqual(sum(call["args"][:2] == ["list", "--installed"] for call in calls), 3)
        self.assertTrue(all(Path(call["program"]).name == "aw" for call in calls))

    def test_missing_provider_bundle_launcher_fails_before_installation(self) -> None:
        script, environment, capture = self.prepare("aw-provider-tokenless")
        result = subprocess.run(
            ["/bin/bash", "-eu", "-c", script], env=environment, cwd=self.root,
            capture_output=True, timeout=15,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(capture.exists())

    def test_ambiguous_bundle_versions_fail_before_installation(self) -> None:
        script, environment, capture = self.prepare()
        packages = Path(environment["AW_PREVIEW_DIR"]) / "packages"
        with tarfile.open(packages / "aw-core-other.tar.gz", "w:gz") as archive:
            archive.add(self.root / "source bundles/aw-core-fixture", arcname="aw-core-other")
        result = subprocess.run(
            ["/bin/bash", "-eu", "-c", script], env=environment, cwd=self.root,
            capture_output=True, timeout=15,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(capture.exists())
