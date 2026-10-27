"""Saved images bind their classic and containerd identities without using tags at run time."""

import hashlib
import importlib.util
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "image_build", ROOT / "skill-score-runtime/image/build.py"
)
assert SPEC and SPEC.loader
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)
NAMES = ("agent-sec-skill-score:test", "agent-sec-skill-score-egress:test")
OCI = "application/vnd.oci.image."


class WorkBaseImageTests(unittest.TestCase):
    def test_buildkit_keeps_the_base_when_temporary_tag_is_its_only_tag(self):
        identity = "sha256:" + "a" * 64
        tag = "agent-sec-skill-score-base:sha256-" + "a" * 64
        base = {"reference": identity, "image_id": identity, "platform": "linux/amd64"}
        inspected = self.inspect(tag, identity)
        details = json.loads(inspected.stdout)
        details[0]["RepoTags"] = [tag]
        details[0]["RepoDigests"] = ["business@" + identity]
        inspected.stdout = json.dumps(details)
        output = io.StringIO()
        with (
            patch.object(
                BUILD.subprocess,
                "run",
                side_effect=[
                    subprocess.CompletedProcess([], 1, "", "missing"),
                    subprocess.CompletedProcess([], 0, "", ""),
                    inspected,
                    inspected,
                ],
            ) as run,
            patch.object(BUILD.sys, "stderr", output),
        ):
            with BUILD.build_base_reference(base) as reference:
                self.assertEqual(reference, tag)
        self.assertFalse(
            any(call.args[0][1:3] == ["image", "rm"] for call in run.call_args_list)
        )
        self.assertIn(
            "removing its only tag could delete the base image", output.getvalue()
        )

    def test_buildkit_local_tag_is_identity_checked_and_only_new_tags_are_removed(self):
        identity = "sha256:" + "a" * 64
        base = {"reference": identity, "image_id": identity, "platform": "linux/amd64"}
        for existed in (False, True):
            for fails in (False, True):
                with self.subTest(existed=existed, fails=fails):
                    current = [identity if existed else None]
                    calls = []

                    def run(command, **kwargs):
                        calls.append(command)
                        if command[1:3] == ["image", "inspect"]:
                            return (
                                self.inspect(command[-1], current[0])
                                if current[0]
                                else subprocess.CompletedProcess(
                                    command, 1, "", "missing"
                                )
                            )
                        if command[1] == "tag":
                            self.assertEqual(command[2], identity)
                            current[0] = identity
                        elif command[1:3] == ["image", "rm"]:
                            self.assertTrue(
                                command[-1].startswith("agent-sec-skill-score-base:")
                            )
                            current[0] = None
                        else:
                            self.fail(f"unexpected Docker operation: {command}")
                        return subprocess.CompletedProcess(command, 0, "", "")

                    with patch.object(BUILD.subprocess, "run", side_effect=run):
                        try:
                            with BUILD.build_base_reference(base) as reference:
                                self.assertEqual(
                                    reference,
                                    "agent-sec-skill-score-base:sha256-" + "a" * 64,
                                )
                                if fails:
                                    raise RuntimeError("build failed")
                        except RuntimeError:
                            self.assertTrue(fails)
                    self.assertEqual(current[0], identity if existed else None)
                    self.assertEqual(
                        sum(call[1:3] == ["image", "rm"] for call in calls),
                        int(not existed),
                    )

    def test_buildkit_rejects_changed_tag_and_preserves_registry_digest_reference(self):
        identity = "sha256:" + "a" * 64
        base = {"reference": identity, "image_id": identity, "platform": "linux/amd64"}
        for changed_after in (False, True):
            wrong = self.inspect("local", "sha256:" + "b" * 64)
            right = self.inspect("local", identity)
            results = [right, right, wrong] if changed_after else [wrong, wrong]
            with patch.object(BUILD.subprocess, "run", side_effect=results) as run:
                with self.assertRaisesRegex(ValueError, "local build base tag differs"):
                    with BUILD.build_base_reference(base):
                        self.assertTrue(changed_after)
                self.assertTrue(
                    all(
                        call.args[0][1:3] == ["image", "inspect"]
                        for call in run.call_args_list
                    )
                )
        base["reference"] = BUILD.DEFAULT_BASE_IMAGE
        with patch.object(BUILD.subprocess, "run") as run:
            with BUILD.build_base_reference(base) as reference:
                self.assertEqual(reference, BUILD.DEFAULT_BASE_IMAGE)
            run.assert_not_called()

    def test_wrapper_observes_execution_but_preserves_harbor_version_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real = root / "claude-real"
            real.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            real.chmod(0o755)
            trace = root / "strace"
            trace.write_text(
                '#!/bin/sh\nprintf "%s\\n" "$@" > "$ASC_TRACE_ARGUMENTS"\n'
            )
            trace.chmod(0o755)
            wrapper = root / "claude"
            wrapper.write_text(
                (ROOT / "skill-score-runtime/image/claude-observed")
                .read_text()
                .replace("/opt/asc/claude/node_modules/.bin/claude", str(real))
                .replace("/logs/artifacts/poc", str(root / "logs"))
            )
            arguments = root / "trace-args"
            environment = os.environ | {
                "PATH": str(root) + os.pathsep + os.environ["PATH"],
                "ASC_TRACE_ARGUMENTS": str(arguments),
            }
            result = subprocess.run(
                ["sh", str(wrapper), "--version"],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(result.stdout, "--version\n")
            self.assertFalse(arguments.exists())
            subprocess.run(
                ["sh", str(wrapper), "-p", "argument with spaces"],
                check=True,
                env=environment,
            )
            self.assertEqual(
                arguments.read_text().splitlines()[-3:],
                [str(real), "-p", "argument with spaces"],
            )

    def inspect(self, reference: str, identity: str, architecture: str = "amd64"):
        return subprocess.CompletedProcess(
            [],
            0,
            json.dumps(
                [
                    {
                        "Id": identity,
                        "Os": "linux",
                        "Architecture": architecture,
                        "RepoDigests": [reference] if "@sha256:" in reference else [],
                        "RepoTags": ["business:prepared"],
                    }
                ]
            ),
            "",
        )

    def test_registry_base_is_bound_to_inspected_platform_and_image_id(self):
        identity = "sha256:" + "a" * 64
        inspected = self.inspect(
            BUILD.DEFAULT_BASE_IMAGE.replace(":lite@", "@"), identity
        )
        with patch.object(BUILD.subprocess, "run", return_value=inspected):
            self.assertEqual(
                BUILD.work_base_image(BUILD.DEFAULT_BASE_IMAGE),
                {
                    "reference": BUILD.DEFAULT_BASE_IMAGE,
                    "image_id": identity,
                    "platform": "linux/amd64",
                },
            )
        for reference in (
            "business:latest",
            "sha256:abcd",
            "example.invalid/work@sha256:abcd",
        ):
            with (
                self.subTest(reference=reference),
                patch.object(BUILD.subprocess, "run") as run,
            ):
                with self.assertRaises(ValueError):
                    BUILD.inspect_base(reference)
                run.assert_not_called()

    def test_local_base_requires_recipe_and_verifies_upstream_identity(self):
        local = "sha256:" + "a" * 64
        upstream = {
            "reference": "example.invalid/base@sha256:" + "b" * 64,
            "image_id": "sha256:" + "c" * 64,
            "platform": "linux/amd64",
        }
        recipe = {
            "source_repository": "https://example.invalid/business",
            "source_commit": "d" * 40,
            "dockerfile_path": "deploy/Dockerfile",
            "dockerfile_sha256": "e" * 64,
            "upstream_base_image": upstream,
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "recipe.json"
            path.write_text(json.dumps(recipe))

            def run(command, **kwargs):
                reference = command[-1]
                return self.inspect(
                    reference, local if reference == local else upstream["image_id"]
                )

            with patch.object(BUILD.subprocess, "run", side_effect=run):
                with self.assertRaisesRegex(ValueError, "requires --base-image-recipe"):
                    BUILD.work_base_image(local)
                result = BUILD.work_base_image(local, path)
                self.assertEqual(result["reference"], result["image_id"])
                self.assertEqual(result["recipe"], recipe)
                for field, value in (
                    ("source_commit", "main"),
                    ("dockerfile_path", "../Dockerfile"),
                    ("dockerfile_sha256", "wrong"),
                ):
                    path.write_text(json.dumps(recipe | {field: value}))
                    with self.assertRaises(ValueError):
                        BUILD.work_base_image(local, path)
                path.write_text(
                    json.dumps(
                        recipe | {"upstream_base_image": upstream | {"image_id": local}}
                    )
                )
                with self.assertRaisesRegex(ValueError, "upstream identity"):
                    BUILD.work_base_image(local, path)

    def test_base_identity_and_platform_cannot_silently_change(self):
        identity = "sha256:" + "a" * 64
        for result in (
            self.inspect(identity, "sha256:" + "b" * 64),
            self.inspect(identity, identity, "arm64"),
        ):
            with patch.object(BUILD.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(ValueError, "exact linux/amd64"):
                    BUILD.inspect_base(identity)
        result = self.inspect(BUILD.DEFAULT_BASE_IMAGE, identity)
        data = json.loads(result.stdout)
        data[0]["RepoDigests"] = []
        result.stdout = json.dumps(data)
        with patch.object(BUILD.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(ValueError, "registry digest"):
                BUILD.inspect_base(BUILD.DEFAULT_BASE_IMAGE)

    def test_custom_base_version_isolated_from_default_release(self):
        base = {"reference": "sha256:" + "a" * 64, "image_id": "sha256:" + "a" * 64}
        BUILD.validate_base_version(
            "1.1.0-skillscore.0123456789ab.base.aaaaaaaaaaaa", base
        )
        for version in (
            "1.1.0-skillscore.0123456789ab",
            "1.1.0-skillscore.0123456789ab.base.bbbbbbbbbbbb",
        ):
            with self.assertRaisesRegex(ValueError, "custom base version"):
                BUILD.validate_base_version(version, base)
        base["reference"] = BUILD.DEFAULT_BASE_IMAGE
        BUILD.validate_base_version("1.1.0-skillscore.0123456789ab", base)
        with self.assertRaisesRegex(ValueError, "default base version"):
            BUILD.validate_base_version(
                "1.1.0-skillscore.0123456789ab.base.aaaaaaaaaaaa", base
            )


class ImageArchiveTests(unittest.TestCase):
    def archive(
        self,
        root: Path,
        failure: str = "",
        manifest_type: str = OCI + "manifest.v1+json",
    ) -> tuple[Path, list[dict]]:
        members = {}

        def store(data, media_type):
            raw = json.dumps(data).encode() if not isinstance(data, bytes) else data
            identity = hashlib.sha256(raw).hexdigest()
            members["blobs/sha256/" + identity] = raw
            return {
                "mediaType": media_type,
                "digest": "sha256:" + identity,
                "size": len(raw),
            }

        index = {"schemaVersion": 2, "manifests": []}
        legacy = []
        expected = []
        for name in NAMES:
            config = store(
                {
                    "os": "linux",
                    "architecture": "arm64" if failure == "platform" else "amd64",
                    "fixture": name,
                },
                OCI + "config.v1+json",
            )
            layer = store(b"fixture layer", OCI + "layer.v1.tar")
            manifest = store(
                {"schemaVersion": 2, "config": config, "layers": [layer]},
                manifest_type,
            )
            manifest["annotations"] = {
                "io.containerd.image.name": "docker.io/library/" + name
            }
            index["manifests"].append(manifest)
            legacy.append(
                {
                    "RepoTags": [name],
                    "Config": "blobs/sha256/" + config["digest"][7:],
                    "Layers": ["blobs/sha256/" + layer["digest"][7:]],
                }
            )
            expected.append(
                {
                    "name": name,
                    "id": config["digest"],
                    "manifest_id": manifest["digest"],
                    "platform": "linux/amd64",
                }
            )
        if failure == "tag":
            index["manifests"][0]["annotations"][
                "io.containerd.image.name"
            ] = "unbound:tag"
        elif failure == "ambiguous":
            index["manifests"][1]["annotations"] = index["manifests"][0]["annotations"]
        elif failure == "legacy-config":
            legacy[0]["Config"] = legacy[1]["Config"]
        elif failure == "legacy-layers":
            legacy[0]["Layers"] = []
        elif failure == "nested-index":
            index["manifests"][0]["mediaType"] = OCI + "index.v1+json"
        elif failure == "manifest-size":
            index["manifests"][0]["size"] += 1
        elif failure in ("manifest-sha", "config-sha"):
            item = expected[0]["id" if failure == "config-sha" else "manifest_id"]
            path = "blobs/sha256/" + item[7:]
            members[path] = b" " + members[path][1:]
        elif failure == "missing-layer":
            del members[legacy[0]["Layers"][0]]
        members["index.json"] = json.dumps(index).encode()
        members["manifest.json"] = json.dumps(legacy).encode()
        target = root / "images.tar"
        with tarfile.open(target, "w") as archive:
            for name, data in members.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
            if failure == "duplicate-member":
                archive.addfile(member, io.BytesIO(data))
        return target, expected

    def test_archive_binds_config_manifest_tag_and_platform(self):
        for manifest_type in (
            OCI + "manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        ):
            with (
                self.subTest(manifest_type=manifest_type),
                tempfile.TemporaryDirectory() as directory,
            ):
                archive, expected = self.archive(
                    Path(directory), manifest_type=manifest_type
                )
                self.assertEqual(
                    BUILD.archive_image_identities(archive, NAMES), expected
                )

    def test_archive_rejects_tampered_or_ambiguous_identity_chain(self):
        for failure in (
            "platform",
            "tag",
            "ambiguous",
            "legacy-config",
            "legacy-layers",
            "nested-index",
            "manifest-size",
            "manifest-sha",
            "config-sha",
            "missing-layer",
            "duplicate-member",
        ):
            with (
                self.subTest(failure=failure),
                tempfile.TemporaryDirectory() as directory,
            ):
                archive, _ = self.archive(Path(directory), failure)
                with self.assertRaises(ValueError):
                    BUILD.archive_image_identities(archive, NAMES)


if __name__ == "__main__":
    unittest.main()
