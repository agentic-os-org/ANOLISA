"""Deterministic optional preparation integrity and failure-recovery contracts."""

import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "optional_install", ROOT / "packaging/skill-score/optional/install.py"
)
assert SPEC and SPEC.loader
INSTALL = importlib.util.module_from_spec(SPEC)
sys.path.insert(0, str(ROOT / "packaging/skill-score/optional"))
try:
    SPEC.loader.exec_module(INSTALL)
finally:
    sys.path.pop(0)


class OptionalInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.cache = self.root / "cache"
        self.cache.mkdir()

    def image_manifest(self) -> dict:
        return {
            "images": [
                {
                    "role": role,
                    "id": "sha256:" + config * 64,
                    "manifest_id": "sha256:" + image * 64,
                    "platform": "linux/amd64",
                }
                for role, config, image in (
                    ("work", "a", "c"),
                    ("egress", "b", "d"),
                )
            ]
        }

    def test_image_resolution_accepts_only_the_bound_id_of_each_store(
        self,
    ) -> None:
        manifest = self.image_manifest()
        for store in ("id", "manifest_id"):
            with self.subTest(store=store):
                expected = {item["role"]: item[store] for item in manifest["images"]}

                def inspect(arguments: list[str], **kwargs: object) -> str:
                    self.assertEqual(arguments[:3], ["docker", "image", "inspect"])
                    self.assertEqual(kwargs, {"timeout": 15})
                    identity = arguments[-1]
                    if identity not in expected.values():
                        raise RuntimeError("not in this image store")
                    return json.dumps(
                        [
                            {
                                "Id": identity,
                                "Os": "linux",
                                "Architecture": "amd64",
                            }
                        ]
                    )

                with patch.object(INSTALL, "run", side_effect=inspect):
                    self.assertEqual(INSTALL.images_ready(manifest), expected)

    def test_image_resolution_rejects_unknown_identity_platform_and_missing_image(
        self,
    ) -> None:
        for failure in (
            "unknown-id",
            "wrong-role",
            "wrong-platform",
            "missing-egress",
        ):
            with self.subTest(failure=failure):
                manifest = self.image_manifest()

                def inspect(arguments: list[str], **_kwargs: object) -> str:
                    identity = arguments[-1]
                    if failure == "missing-egress" and identity in (
                        "sha256:" + "b" * 64,
                        "sha256:" + "d" * 64,
                    ):
                        raise RuntimeError("missing")
                    actual = (
                        "sha256:" + "e" * 64 if failure == "unknown-id" else identity
                    )
                    if failure == "wrong-role":
                        actual = "sha256:" + "b" * 64
                    return json.dumps(
                        [
                            {
                                "Id": actual,
                                "Os": "linux",
                                "Architecture": (
                                    "arm64" if failure == "wrong-platform" else "amd64"
                                ),
                            }
                        ]
                    )

                with patch.object(INSTALL, "run", side_effect=inspect):
                    self.assertIsNone(INSTALL.images_ready(manifest))

    def test_readiness_passes_resolved_ids_without_reverting_to_config_ids(
        self,
    ) -> None:
        environment = self.root / "environment"
        environment.mkdir()
        manifest = self.image_manifest()
        manifest["protocol_version"] = "agent-sec-skill-score/worker-v1"
        actual = {item["role"]: item["manifest_id"] for item in manifest["images"]}

        def worker(arguments: list[str]) -> None:
            request = json.loads(Path(arguments[-2]).read_text())
            self.assertEqual(request["image_ids"], actual)
            Path(arguments[-1]).write_text(
                json.dumps(
                    {
                        "schema_version": manifest["protocol_version"],
                        "request_id": request["request_id"],
                        "cleanup_status": "complete",
                        "result": {"tools": ["python"]},
                    }
                )
            )

        with patch.object(INSTALL, "run_readiness_worker", side_effect=worker):
            INSTALL.environment_check(
                environment, manifest, self.root / "recovery", actual
            )
        self.assertEqual(list((self.root / "recovery").iterdir()), [])

    def test_manifest_requires_both_immutable_identities(self) -> None:
        prefix = self.root / "prefix"
        contract = prefix / "share/anolisa/components/sec-core/component.toml"
        contract.parent.mkdir(parents=True)
        contract.write_text('[component]\nversion = "test"\n')
        for malformed in (None, "mutable:tag"):
            with self.subTest(manifest_id=malformed):
                manifest = self.image_manifest()
                manifest.update(
                    schema="agent-sec-skill-score/runtime-v1",
                    component="sec-core-skill-score",
                    protocol_version="agent-sec-skill-score/worker-v1",
                    platform="linux/amd64",
                    harbor_version="0.22.0",
                    version="test",
                    files=[],
                )
                if malformed is None:
                    del manifest["images"][0]["manifest_id"]
                else:
                    manifest["images"][0]["manifest_id"] = malformed
                with self.assertRaisesRegex(ValueError, "immutable"):
                    INSTALL.validate_manifest(manifest, self.root, prefix)

    def archive(self, name: str, link: str | None = None) -> Path:
        path = self.root / "archive.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            member = tarfile.TarInfo(name)
            if link is not None:
                member.type = tarfile.SYMTYPE
                member.linkname = link
            archive.addfile(member, io.BytesIO())
        return path

    def test_cache_and_local_source_are_always_hashed(self) -> None:
        expected = b"pinned upstream bytes"
        checksum = hashlib.sha256(expected).hexdigest()
        spec = {
            "file": "runtime.tar.gz",
            "sha256": checksum,
            "bytes": len(expected),
        }
        (self.cache / checksum).write_bytes(b"corrupt")
        (self.root / spec["file"]).write_bytes(expected)
        with patch.object(
            INSTALL.urllib.request,
            "urlopen",
            side_effect=AssertionError("network"),
        ):
            result = INSTALL.fetch(spec, self.cache, self.root, True)
        self.assertEqual(result.read_bytes(), expected)
        result.write_bytes(b"corrupt")
        (self.root / spec["file"]).write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            INSTALL.fetch(spec, self.cache, self.root, True)

    def test_offline_missing_dependency_does_not_contact_network(self) -> None:
        with patch.object(
            INSTALL.urllib.request,
            "urlopen",
            side_effect=AssertionError("network"),
        ):
            with self.assertRaisesRegex(ValueError, "offline dependency"):
                INSTALL.fetch(
                    {"file": "missing", "sha256": "a" * 64},
                    self.cache,
                    None,
                    True,
                )

    def test_download_identifies_client_and_still_verifies_bytes(self) -> None:
        expected = b"pinned upstream bytes"
        spec = {
            "file": "runtime.tar.gz",
            "url": "https://upstream.example/runtime.tar.gz",
            "sha256": hashlib.sha256(expected).hexdigest(),
            "bytes": len(expected),
        }
        for received in (expected, b"x" * len(expected)):
            with self.subTest(correct_bytes=received == expected):
                response = io.BytesIO(received)
                response.url = spec["url"]
                with patch.object(
                    INSTALL.urllib.request, "urlopen", return_value=response
                ) as fetch:
                    if received == expected:
                        result = INSTALL.fetch(spec, self.cache, None, False)
                        self.assertEqual(result.read_bytes(), expected)
                        result.unlink()
                    else:
                        with self.assertRaisesRegex(
                            ValueError, "checksum/size mismatch"
                        ):
                            INSTALL.fetch(spec, self.cache, None, False)
                request = fetch.call_args.args[0]
                self.assertEqual(request.full_url, spec["url"])
                self.assertEqual(request.get_header("User-agent"), "anolisa/0.3.20")
                self.assertEqual(fetch.call_args.kwargs, {"timeout": 60})
                self.assertEqual(list(self.cache.iterdir()), [])

    def test_http_error_reports_asset_and_origin_without_redirect_secrets(
        self,
    ) -> None:
        spec = {
            "file": "runtime.tar.gz",
            "url": "https://upstream.example/runtime.tar.gz?token=ORIGINAL_SECRET",
            "sha256": "a" * 64,
        }
        error = INSTALL.urllib.error.HTTPError(
            "https://redirect.example/blob?signature=REDIRECT_SECRET",
            403,
            "Forbidden",
            None,
            io.BytesIO(b"SENSITIVE_RESPONSE_BODY"),
        )
        with patch.object(INSTALL.urllib.request, "urlopen", side_effect=error):
            with self.assertRaises(ValueError) as caught:
                INSTALL.fetch(spec, self.cache, None, False)
        self.assertEqual(
            str(caught.exception),
            "dependency download failed for runtime.tar.gz from upstream.example: HTTP 403",
        )
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_archive_rejects_traversal_and_escaping_links(self) -> None:
        for name, link in (
            ("../escape", None),
            ("python/bin/python", "../../../escape"),
        ):
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    INSTALL.unpack(self.archive(name, link), self.root / "extract")
        self.assertFalse((self.root / "escape").exists())

    def test_archive_accepts_internal_library_link(self) -> None:
        INSTALL.unpack(
            self.archive("python/lib/libpython.so", "libpython.so.1"),
            self.root / "extract",
        )
        self.assertTrue((self.root / "extract/python/lib/libpython.so").is_symlink())

    def test_preparation_uses_the_same_system_docker_daemon_as_execution(
        self,
    ) -> None:
        with (
            patch.dict(
                os.environ,
                {
                    "DOCKER_HOST": "unix:///other.sock",
                    "DOCKER_CONTEXT": "other",
                },
            ),
            patch.object(
                INSTALL, "run", side_effect=["linux", "[]", "Docker Compose v2"]
            ),
        ):
            INSTALL.require_local_docker()
            self.assertEqual(os.environ["DOCKER_HOST"], "unix:///var/run/docker.sock")
            self.assertNotIn("DOCKER_CONTEXT", os.environ)

    def test_candidate_keeps_its_manifest_and_inventories_it_before_activation(
        self,
    ) -> None:
        payload = self.root / "payload"
        payload.mkdir()
        manifest = {
            "images": [],
            "version": "test",
            "source_commit": "a" * 40,
            "protocol_version": "agent-sec-skill-score/worker-v1",
            "worker_version": "0.10.0",
            "python": {"version": "3.12.11"},
            "harbor_version": "0.22.0",
        }
        manifest_path = payload / "runtime-manifest.json"
        manifest_path.write_text(json.dumps(manifest))
        identity = INSTALL.digest(manifest_path)

        def check_candidate(
            environment: Path,
            _manifest: dict,
            _recovery: Path,
            _image_ids: dict,
        ) -> None:
            self.assertEqual(environment.name, identity)
            self.assertTrue(environment.parent.name.startswith(".candidate-"))
            self.assertEqual(
                (environment / "runtime-manifest.json").read_bytes(),
                manifest_path.read_bytes(),
            )
            self.assertFalse((environment / "ready.json").exists())

        with (
            patch.object(INSTALL.platform, "system", return_value="Linux"),
            patch.object(INSTALL.platform, "machine", return_value="x86_64"),
            patch.object(INSTALL, "validate_manifest"),
            patch.object(INSTALL, "require_local_docker"),
            patch.object(INSTALL, "images_ready", return_value={}),
            patch.object(INSTALL, "ready_matches", return_value=False),
            patch.object(INSTALL, "build_environment"),
            patch.object(INSTALL, "network_check"),
            patch.object(INSTALL, "environment_check", side_effect=check_candidate),
        ):
            result = INSTALL.prepare(payload, self.root / "prefix", self.cache)
        inventory = json.loads((result / "installed-files.json").read_text())
        self.assertEqual(
            inventory,
            [
                {
                    "file": "runtime-manifest.json",
                    "bytes": manifest_path.stat().st_size,
                    "sha256": identity,
                }
            ],
        )
        self.assertEqual(list(result.parent.iterdir()), [result])

    def test_failed_candidate_keeps_prior_environment_and_cleans_partial(
        self,
    ) -> None:
        payload = self.root / "payload"
        payload.mkdir()
        manifest = {"images": [], "version": "test"}
        manifest_path = payload / "runtime-manifest.json"
        manifest_path.write_text(json.dumps(manifest))
        identity = INSTALL.digest(manifest_path)
        parent = self.root / "prefix/lib/anolisa/sec-core-skill-score/environments"
        prior = parent / identity
        prior.mkdir(parents=True)
        (prior / "ready.json").write_text("old environment")
        with (
            patch.object(INSTALL.platform, "system", return_value="Linux"),
            patch.object(INSTALL.platform, "machine", return_value="x86_64"),
            patch.object(INSTALL, "validate_manifest"),
            patch.object(INSTALL, "require_local_docker"),
            patch.object(INSTALL, "images_ready", return_value={}),
            patch.object(INSTALL, "ready_matches", return_value=False),
            patch.object(
                INSTALL,
                "build_environment",
                side_effect=ValueError("dependency failed"),
            ),
        ):
            with self.assertRaisesRegex(ValueError, "dependency failed"):
                INSTALL.prepare(payload, self.root / "prefix", self.cache)
        self.assertEqual((prior / "ready.json").read_text(), "old environment")
        self.assertEqual(list(parent.iterdir()), [prior])

    def test_readiness_timeout_signals_before_killing(self) -> None:
        for cooperative in (True, False):
            with self.subTest(cooperative=cooperative):
                process = Mock()
                process.poll.return_value = None
                process.wait.side_effect = (
                    [0] if cooperative else [subprocess.TimeoutExpired("worker", 15), 0]
                )
                with patch.object(INSTALL.subprocess, "Popen", return_value=process):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        INSTALL.run_readiness_worker(["worker"], timeout=0)
                process.terminate.assert_called_once()
                self.assertEqual(process.kill.call_count, int(not cooperative))

    def test_failed_readiness_preserves_recovery_coordinates_unless_cleanup_verified(
        self,
    ) -> None:
        environment = self.root / "environment"
        environment.mkdir()
        recovery = self.root / "recovery"
        manifest = {
            "protocol_version": "agent-sec-skill-score/worker-v1",
            "images": [],
        }
        for complete in (False, True):
            with self.subTest(complete=complete):
                with (
                    patch.object(
                        INSTALL,
                        "run_readiness_worker",
                        side_effect=subprocess.TimeoutExpired("worker", 100),
                    ),
                    patch.object(
                        INSTALL,
                        "cleanup_readiness_request",
                        return_value=complete,
                    ) as cleanup,
                ):
                    expected = subprocess.TimeoutExpired if complete else RuntimeError
                    with self.assertRaises(expected):
                        INSTALL.environment_check(environment, manifest, recovery, {})
                args = cleanup.call_args.args
                record = recovery / (args[1] + ".json")
                self.assertEqual(record.exists(), not complete)
                if not complete:
                    value = json.loads(record.read_text())
                    self.assertEqual(value["request_id"], args[1])
                    self.assertEqual(value["compose_project"], args[2])
                    self.assertEqual(record.stat().st_mode & 0o777, 0o600)
                self.assertEqual(list(environment.iterdir()), [])

    def test_ready_check_rejects_file_drift_before_import(self) -> None:
        environment = self.root / "environment"
        environment.mkdir()
        (environment / "worker.py").write_bytes(b"modified")
        inventory = environment / "installed-files.json"
        inventory.write_text(
            json.dumps(
                [
                    {
                        "file": "worker.py",
                        "sha256": hashlib.sha256(b"original").hexdigest(),
                    }
                ]
            )
        )
        (environment / "ready.json").write_text(
            json.dumps(
                {
                    "manifest_sha256": "a" * 64,
                    "images": [],
                    "installed_files_sha256": INSTALL.digest(inventory),
                }
            )
        )
        with patch.object(
            INSTALL,
            "python_check",
            side_effect=AssertionError("imported corrupt code"),
        ):
            self.assertFalse(
                INSTALL.ready_matches(environment, "a" * 64, {"images": []})
            )


if __name__ == "__main__":
    unittest.main()
