"""Reject stale, mixed or leaky delivery artifacts before native acceptance."""

import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk_candidate as candidate
import package
from common import Cancellation, Cancelled, digest
from test_packages import Fixture

SHA = "a" * 40

# These tools exercise build orchestration offline; git, tar and publication stay real.
BUILD_TOOL = r"""#!PYTHON
import io, json, pathlib, sys, tarfile, zipfile
name = pathlib.Path(sys.argv[0]).name
if name in ('node', 'npm') and sys.argv[1:] == ['--version']:
    print('fixture-version')
elif name == 'npm':
    assert pathlib.Path('candidate-input').read_text() == 'archived'
    pkg = pathlib.Path.cwd().name
    output = pathlib.Path(sys.argv[sys.argv.index('--pack-destination') + 1])
    tar_name = (
        'anolisa-aw-sdk-0.1.1.tgz'
        if pkg == 'typescript'
        else f'anolisa-aw-sdk-{pkg}-0.1.1.tgz'
    )
    member = (
        'package/dist/client.mjs'
        if pkg == 'typescript'
        else f'package/dist/native/{pkg}/index.mjs'
    )
    with tarfile.open(output / tar_name, 'w:gz') as archive:
        item = tarfile.TarInfo(member)
        item.size = 7
        archive.addfile(item, io.BytesIO(b'current'))
elif sys.argv[1:] == ['--version']:
    print('Python fixture-version')
elif sys.argv[1] == '-c':
    import importlib.metadata
    importlib.metadata.version = lambda name: 'fixture-' + name
    exec(sys.argv[2])
else:
    assert sys.argv[1:4] == ['-m', 'pip', 'wheel']
    source = pathlib.Path(sys.argv[sys.argv.index('--no-index') + 1])
    assert (source / 'candidate-input').read_text() == 'archived'
    pkg = source.name
    output = pathlib.Path(sys.argv[sys.argv.index('--wheel-dir') + 1])
    whl_name = (
        'anolisa_aw_sdk-0.1.1-py3-none-any.whl'
        if pkg == 'python'
        else f'anolisa_aw_sdk_{pkg}-0.1.1-py3-none-any.whl'
    )
    member = (
        'aw_sdk/client.py'
        if pkg == 'python'
        else f'aw_sdk_{pkg}/_native/adapter.py'
    )
    with zipfile.ZipFile(output / whl_name, 'w') as archive:
        archive.writestr(member, b'current')
"""

FIXTURE_SPECS = (
    ("npm", "core", "sdk-core.tgz"),
    ("npm", "qoder", "sdk-qoder.tgz"),
    ("npm", "pi", "sdk-pi.tgz"),
    ("wheel", "core", "sdk-core.whl"),
    ("wheel", "claude", "sdk-claude.whl"),
    ("wheel", "qwenpaw", "sdk-qwenpaw.whl"),
)


def npm(path, content, member="package/dist/client.mjs"):
    with tarfile.open(path, "w:gz") as archive:
        item = tarfile.TarInfo(member)
        item.size = len(content)
        archive.addfile(item, io.BytesIO(content))


def wheel(path, content, member="aw_sdk/client.py"):
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(member, content)


def stage_fixture_artifacts(root, contents=None):
    contents = contents or {}
    cancel = Cancellation()
    records = []
    for kind, component, filename in FIXTURE_SPECS:
        data = contents.get((kind, component), b"current")
        if kind == "npm":
            npm(root / filename, data)
        else:
            wheel(root / filename, data)
        records.append(
            {
                "kind": kind,
                "component": component,
                "file": filename,
                "sha256": digest(root / filename, cancel),
            }
        )
    return records


class ReleaseVersionTests(unittest.TestCase):
    @unittest.skipUnless(sys.version_info >= (3, 11), "release version checks require Python 3.11")
    def test_release_requires_matching_binary_and_sdk_versions(self):
        from release_version import release_version

        repo = Path(__file__).resolve().parents[4]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifests = [
                *repo.glob("src/aw/crates/*/Cargo.toml"),
                repo / "providers/agent-sec-core/Cargo.toml",
                repo / "providers/tokenless/Cargo.toml",
            ]
            for kind, _, directory in candidate.SDK_PACKAGES:
                manifests.append(repo / directory / ("package.json" if kind == "npm" else "pyproject.toml"))
            for manifest in manifests:
                target = root / manifest.relative_to(repo)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(manifest, target)
            version = release_version(root)
            for manifest in manifests:
                target = root / manifest.relative_to(repo)
                with self.subTest(manifest=target.name):
                    target.write_text(target.read_text().replace(version, "999.0.0", 1))
                    with self.assertRaisesRegex(ValueError, "inconsistent AW release versions"):
                        release_version(root)
                    shutil.copyfile(manifest, target)


class CandidateTests(unittest.TestCase):
    def test_full_commit_required(self):
        for value in ("main", "a" * 39, "A" * 40):
            with self.assertRaises(ValueError):
                candidate.commit_id(Path.cwd(), value, Cancellation())

    def test_core_boundary_rejects_bundled_framework_bridges(self):
        candidate.check_core_boundary(
            "wheel", "core", {"aw_sdk/client.py": b"", "aw_sdk/sources.py": b""}
        )
        candidate.check_core_boundary(
            "npm",
            "core",
            {
                "package/dist/client.mjs": b"",
                "package/dist/native/shared/scope.mjs": b"",
            },
        )
        for member in ("aw_sdk/_native/qwenpaw/sdk.py", "aw_sdk/_native/claude/aw_claude.py"):
            with self.assertRaisesRegex(ValueError, "must not bundle framework adapters"):
                candidate.check_core_boundary("wheel", "core", {member: b""})
        for member in (
            "package/dist/native/qoder-sdk/sdk.mjs",
            "package/dist/native/pi/resident.mjs",
        ):
            with self.assertRaisesRegex(ValueError, "must not bundle framework adapters"):
                candidate.check_core_boundary("npm", "core", {member: b""})

    def test_rpm_set_rejects_stale_mixed_missing_and_duplicate(self):
        records = [
            {
                "name": name,
                "version": "0.1.1",
                "release": "6",
                "arch": "x86_64",
                "commit": SHA,
            }
            for name in candidate.PACKAGES
        ]
        candidate.check_rpm_set(records, SHA)
        variants = [
            records[:2],
            records + [records[0]],
            [dict(records[0], commit="b" * 40), *records[1:]],
            [dict(records[0], release="5"), *records[1:]],
            [dict(records[0], arch="aarch64"), *records[1:]],
        ]
        for variant in variants:
            with self.assertRaises(ValueError):
                candidate.check_rpm_set(variant, SHA)

    def test_npm_rejects_link_and_duplicate_members(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sdk.tgz"
            with tarfile.open(path, "w:gz") as archive:
                item = tarfile.TarInfo("package/escape")
                item.type = tarfile.SYMTYPE
                item.linkname = "/etc/passwd"
                archive.addfile(item)
            with self.assertRaises(ValueError):
                candidate.sdk_payload(path)
            with tarfile.open(path, "w:gz") as archive:
                item = tarfile.TarInfo("package/dist/client.mjs")
                archive.addfile(item, io.BytesIO())
                archive.addfile(item, io.BytesIO())
            with self.assertRaises(ValueError):
                candidate.sdk_payload(path)

    def test_forged_receipt_cannot_make_old_payload_match(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cancel = Cancellation()
            records = stage_fixture_artifacts(root, {("npm", "core"): b"old"})
            receipt = root / "sdk-candidate.json"
            receipt.write_text(
                json.dumps(
                    {
                        "schema": "aw-sdk-candidate/v2",
                        "commit": SHA,
                        "artifacts": records,
                    }
                )
            )

            def rebuild(repo, sha, output, python, cancellation):
                return {"artifacts": stage_fixture_artifacts(output)}

            with patch.object(candidate, "build", side_effect=rebuild):
                with self.assertRaisesRegex(ValueError, "payload differs"):
                    candidate.verify_sdk(root, SHA, receipt, sys.executable, cancel)
                npm(root / "sdk-core.tgz", b"current")
                records[0]["sha256"] = digest(root / "sdk-core.tgz", cancel)
                receipt.write_text(
                    json.dumps(
                        {
                            "schema": "aw-sdk-candidate/v2",
                            "commit": SHA,
                            "artifacts": records,
                        }
                    )
                )
                report = candidate.verify_sdk(
                    root, SHA, receipt, sys.executable, cancel
                )
                self.assertEqual(report["sdk_payloads"], "match-candidate-rebuild")
                (root / "sdk-core.tgz").write_bytes(b"changed")
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    candidate.verify_sdk(root, SHA, receipt, sys.executable, cancel)

    def test_relabelled_payload_is_rejected(self):
        for kind, component in (("npm", "core"), ("wheel", "claude")):
            with self.subTest(kind=kind, component=component), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                records = stage_fixture_artifacts(root)
                rebuilt_records = [dict(row) for row in records]
                row = next(
                    row
                    for row in records
                    if row["kind"] == kind and row["component"] == component
                )
                renamed = "old-0.0.1" + Path(row["file"]).suffix
                (root / row["file"]).rename(root / renamed)
                row["file"] = renamed
                receipt = root / "sdk-candidate.json"
                receipt.write_text(
                    json.dumps(
                        {
                            "schema": "aw-sdk-candidate/v2",
                            "commit": SHA,
                            "artifacts": records,
                        }
                    )
                )

                def rebuild(repo, sha, output, python, cancellation):
                    stage_fixture_artifacts(output)
                    return {"artifacts": rebuilt_records}

                with patch.object(candidate, "build", side_effect=rebuild):
                    with self.assertRaisesRegex(ValueError, "filename differs"):
                        candidate.verify_sdk(
                            root, SHA, receipt, sys.executable, Cancellation()
                        )

    def test_malformed_receipts_have_actionable_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / "sdk-candidate.json"
            invalid = [None, [], 1, "text"]
            invalid += [
                {"schema": "aw-sdk-candidate/v1", "commit": SHA, "artifacts": []},
            ]
            invalid += [
                {"schema": "aw-sdk-candidate/v2", "commit": SHA, "artifacts": rows}
                for rows in (
                    None,
                    1,
                    {},
                    "ab",
                    [None] * 6,
                    [{}] * 6,
                    [{"kind": "npm", "component": "core"}],
                )
            ]
            with patch.object(candidate, "build") as rebuild:
                for value in invalid:
                    with self.subTest(receipt=value):
                        receipt.write_text(json.dumps(value))
                        with self.assertRaises(ValueError):
                            candidate.verify_sdk(
                                root, SHA, receipt, sys.executable, Cancellation()
                            )
                rebuild.assert_not_called()


@unittest.skipUnless(
    shutil.which("git") and shutil.which("tar"), "requires git and tar"
)
class BuildTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        (self.repo / "LICENSE").write_text("fixture")
        for _, _, rel_dir in candidate.SDK_PACKAGES:
            directory = self.repo / rel_dir
            directory.mkdir(parents=True)
            (directory / "candidate-input").write_text("archived")
        for args in (
            ["init", "--quiet"],
            ["add", "."],
            [
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        ):
            subprocess.run(
                ["git", *args],
                cwd=self.repo,
                check=True,
                timeout=10,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        self.sha = (
            subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=self.repo, timeout=10
            )
            .decode()
            .strip()
        )
        # Dirty inputs must never reach the fake builders.
        for _, _, rel_dir in candidate.SDK_PACKAGES:
            (self.repo / rel_dir / "candidate-input").write_text("dirty")
        tools = self.root / "tools"
        tools.mkdir()
        for name in ("node", "npm", "python-builder"):
            path = tools / name
            path.write_text(BUILD_TOOL.replace("#!PYTHON", "#!" + sys.executable))
            path.chmod(0o755)
        self.python = str(tools / "python-builder")
        environment = patch.dict(
            os.environ, {"PATH": str(tools) + os.pathsep + os.environ["PATH"]}
        )
        environment.start()
        self.addCleanup(environment.stop)
        self.output = self.root / "output"

    def build(self, cancel=None):
        return candidate.build(
            self.repo, self.sha, self.output, self.python, cancel or Cancellation()
        )

    def test_build_and_verify_from_archive(self):
        receipt = self.build()
        self.assertEqual(len(receipt["artifacts"]), len(candidate.SDK_PACKAGES))
        self.assertEqual(
            set(receipt["tools"]),
            {"node", "npm", "python", "pip", "setuptools", "wheel"},
        )
        for name in ("pip", "setuptools", "wheel"):
            self.assertEqual(receipt["tools"][name], "fixture-" + name)
        self.assertEqual(
            {p.name for p in self.output.iterdir()},
            {"sdk-candidate.json", *(row["file"] for row in receipt["artifacts"])},
        )
        self.assertEqual(
            json.loads((self.output / "sdk-candidate.json").read_text()), receipt
        )
        report = candidate.verify_sdk(
            self.repo,
            self.sha,
            self.output / "sdk-candidate.json",
            self.python,
            Cancellation(),
        )
        self.assertEqual(report["sdk_payloads"], "match-candidate-rebuild")

    def test_late_cancellation_leaves_output_retryable(self):
        cancel = Cancellation()
        real_run = candidate.run

        def late_cancel(command, *args):
            result = real_run(command, *args)
            if command == [self.python, "--version"]:
                cancel.request()
            return result

        with patch.object(candidate, "run", side_effect=late_cancel):
            with self.assertRaises(Cancelled):
                self.build(cancel)
        self.assertEqual(list(self.output.iterdir()), [])
        self.build()

    def test_publication_failure_and_cancellation_roll_back(self):
        for failure in ("error", "cancel"):
            for position in (1, 4, 7):
                with self.subTest(failure=failure, position=position):
                    self.output = self.root / f"{failure}-{position}"
                    cancel = Cancellation()
                    linked = []

                    def link(source, target):
                        if failure == "error" and len(linked) + 1 == position:
                            raise OSError("injected publication failure")
                        os.link(source, target)
                        linked.append(target)
                        if failure == "cancel" and len(linked) == position:
                            cancel.request()

                    def publish(artifacts, output, cancellation):
                        return package.publish(
                            artifacts, output, cancellation, link=link
                        )

                    with patch.object(candidate, "publish", side_effect=publish):
                        with self.assertRaises((OSError, Cancelled)):
                            self.build(cancel)
                    self.assertEqual(list(self.output.iterdir()), [])
                    self.build()

    def test_receipt_write_failure_leaves_output_retryable(self):
        real_write = Path.write_text

        def write(path, text, *args, **kwargs):
            if path.name == "sdk-candidate.json":
                raise OSError("injected receipt write failure")
            return real_write(path, text, *args, **kwargs)

        with patch.object(Path, "write_text", new=write):
            with self.assertRaisesRegex(OSError, "receipt write failure"):
                self.build()
        self.assertEqual(list(self.output.iterdir()), [])
        self.build()

    def test_publication_preserves_concurrent_file(self):
        def link(source, target):
            if target.suffix == ".whl":
                target.write_bytes(b"concurrent")
            os.link(source, target)

        def publish(artifacts, output, cancellation):
            return package.publish(artifacts, output, cancellation, link=link)

        with patch.object(candidate, "publish", side_effect=publish):
            with self.assertRaises(FileExistsError):
                self.build()
        (remaining,) = self.output.iterdir()
        self.assertEqual(remaining.read_bytes(), b"concurrent")


@unittest.skipUnless(
    shutil.which("cpio") and shutil.which("bash"), "requires cpio and Bash"
)
class RpmTests(Fixture):
    def setUp(self):
        super().setUp()
        self.prepare("core sec-core tokenless")
        tools = self.repo / "tools"
        tools.mkdir()
        rpm = tools / "rpm"
        rpm.write_text(
            "#!" + sys.executable + "\nimport pathlib, sys\n"
            "print(pathlib.Path(sys.argv[-1]).stem + '\\t0.1.0\\t6\\tx86_64', end='')\n"
        )
        rpm.chmod(0o755)
        rpm2cpio = tools / "rpm2cpio"
        rpm2cpio.write_text('#!/bin/sh\ncat -- "$1"\n')
        rpm2cpio.chmod(0o755)
        environment = patch.dict(
            os.environ, {"PATH": str(tools) + os.pathsep + os.environ["PATH"]}
        )
        environment.start()
        self.addCleanup(environment.stop)

    def cpio(self, name, source, leading_dot=False):
        root = self.repo / (name + "-cpio")
        relative = "./usr/share/doc/" + name + "/source.json"
        record = root / relative
        record.parent.mkdir(parents=True, exist_ok=True)
        record.write_text(json.dumps(source))
        archive = subprocess.run(
            ["cpio", "--quiet", "-o", "-H", "newc"],
            cwd=root,
            input=(relative + "\n").encode(),
            capture_output=True,
            check=True,
            timeout=10,
        ).stdout
        if leading_dot:
            # GNU cpio strips ./ on creation; preserve it explicitly in the newc fixture.
            size = int(archive[94:102], 16)
            end = (110 + size + 3) // 4 * 4
            header = archive[:94] + f"{size + 2:08x}".encode() + archive[102:110]
            member = header + b"./" + archive[110 : 110 + size]
            archive = member + b"\0" * (-len(member) % 4) + archive[end:]
        path = self.repo / (name + ".rpm")
        path.write_bytes(archive)
        return path

    def test_actual_archive_source_records_through_cpio_pipeline(self):
        records = []
        for name in sorted(candidate.PACKAGES):
            root = self.repo / (name + "-stage")
            root.mkdir()
            if name == "aw":
                package._core(
                    self.repo, root, self.version, self.commit, "x86_64", self.cancel
                )
                source = root / "share/doc/aw/source.json"
            else:
                package._provider(
                    self.repo,
                    root,
                    name,
                    self.version,
                    self.commit,
                    "x86_64",
                    self.cancel,
                )
                source = root / "package.json"
            path = self.cpio(name, json.loads(source.read_text()))
            record = candidate.rpm_declaration(path, self.cancel)
            self.assertEqual(record["commit"], self.commit)
            self.assertEqual(record["sha256"], digest(path, self.cancel))
            records.append(record)
        candidate.check_rpm_set(records, self.commit)

    def test_native_rpm_commit_schema_and_invalid_records(self):
        for leading_dot in (False, True):
            path = self.cpio("aw", {"commit": self.commit}, leading_dot=leading_dot)
            self.assertEqual(
                candidate.rpm_declaration(path, self.cancel)["commit"], self.commit
            )
        for source in (
            {},
            {"source_commit": None},
            {"source_commit": "main"},
            [],
            {"source_commit": self.commit, "commit": "b" * 40},
        ):
            with self.subTest(source=source):
                path = self.cpio("aw", source)
                with self.assertRaises(ValueError):
                    candidate.rpm_declaration(path, self.cancel)

    def test_pipeline_failure_is_propagated(self):
        path = self.cpio("aw", {"commit": self.commit})
        (self.repo / "tools/rpm2cpio").write_text('#!/bin/sh\ncat -- "$1"\nexit 1\n')
        with self.assertRaisesRegex(RuntimeError, "bash exited with 1"):
            candidate.rpm_declaration(path, self.cancel)


if __name__ == "__main__":
    unittest.main()
