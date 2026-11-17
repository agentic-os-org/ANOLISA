"""Shared malformed descriptors and actual offline CLI output exercise SDK loading."""

from copy import deepcopy
import errno
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from aw_sdk import AwError
from aw_sdk.configuration import load_configuration
from aw_sdk.deployment import LIMIT, check_filesystems, load_descriptor, read_file

FIXTURE = json.loads(
    (Path(__file__).resolve().parents[2] / "tests/fixtures/deployment.json").read_text()
)
BINARY = os.environ["AW_TEST_BINARY"]
QWENPAW_PROFILE = {
    "adapter": "qwenpaw",
    "version": "installed-native-hooks/v1",
    "entrypoint": "app",
}


class Deployment(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="aw-sdk-descriptor-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.config = self.root / "config.json"
        self.descriptor = self.root / "descriptor.json"
        self.config.write_text(json.dumps(FIXTURE["configuration"]))
        self.config.chmod(0o600)
        self.value = deepcopy(FIXTURE["descriptor"])
        self.value["configuration_sha256"] = hashlib.sha256(self.config.read_bytes()).hexdigest()
        self.profile = deepcopy(self.value["profile"])
        self.write()

    def write(self) -> None:
        self.descriptor.write_text(json.dumps(self.value))
        self.descriptor.chmod(0o600)

    def load(self) -> dict:
        return load_descriptor(str(self.config), str(self.descriptor), self.profile, "app")

    def test_shared_invalid_documents(self) -> None:
        for case in FIXTURE["invalid"]:
            with self.subTest(case["name"]):
                value = deepcopy(self.value)
                item = value
                for part in case["path"][:-1]:
                    item = item[part]
                if case.get("remove"):
                    del item[case["path"][-1]]
                else:
                    item[case["path"][-1]] = case["value"]
                self.descriptor.write_text(json.dumps(value))
                with self.assertRaises(AwError) as raised:
                    self.load()
                self.assertEqual(raised.exception.code, case["error"])
                self.assertNotIn("secret", str(raised.exception))

    def test_shared_unicode_profile_fields(self) -> None:
        for case in FIXTURE["unicode_profile"]:
            for field in self.profile:
                with self.subTest(case=case["name"], field=field):
                    value = deepcopy(self.value)
                    value["profile"][field] = case["value"]
                    self.descriptor.write_text(json.dumps(value))
                    if case["valid"]:
                        load_descriptor(
                            str(self.config), str(self.descriptor), value["profile"], "app"
                        )
                    else:
                        with self.assertRaisesRegex(AwError, "sdk_descriptor_invalid"):
                            load_descriptor(
                                str(self.config), str(self.descriptor), value["profile"], "app"
                            )

    def test_fixed_descriptors_require_supported_filesystems(self) -> None:
        metadata = {
            "/proc/self/fdinfo/10": "mnt_id:\t7\n",
            "/proc/self/fdinfo/11": "mnt_id:\t8\n",
        }
        for unsupported in (None, "nfs", "cifs", "ntfs3", "fuse.sshfs", "unknown"):
            for bad_id in (7, 8):
                mounts = "".join(
                    f"{mount_id} 1 0:1 / /test rw - "
                    f"{unsupported if mount_id == bad_id and unsupported else 'ext4'} none rw\n"
                    for mount_id in (7, 8)
                )
                records = {**metadata, "/proc/self/mountinfo": mounts}
                with self.subTest(filesystem=unsupported, mount_id=bad_id), patch(
                    "aw_sdk.deployment.open",
                    side_effect=lambda path, **_options: io.StringIO(records[path]),
                ) as opened:
                    if unsupported is None:
                        check_filesystems(10, 11)
                    else:
                        with self.assertRaises(ValueError):
                            check_filesystems(10, 11)
                    self.assertEqual(
                        sum(call.args == ("/proc/self/mountinfo",) for call in opened.call_args_list),
                        1,
                    )
        for fdinfo, mountinfo in (("pos:\t0\n", ""), ("mnt_id:\t7\n", "")):
            with patch(
                "aw_sdk.deployment.open",
                side_effect=lambda path, **_options: io.StringIO(
                    mountinfo if path.endswith("mountinfo") else fdinfo
                ),
            ), self.assertRaises(ValueError):
                check_filesystems(10)
        with patch("aw_sdk.deployment.check_filesystems", side_effect=ValueError()):
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                read_file(str(self.descriptor))

    def test_unrelated_non_utf8_mount_paths_do_not_reject_local_files(self) -> None:
        metadata = {
            "/proc/self/fdinfo/10": b"mnt_id:\t7\n",
            "/proc/self/mountinfo": (
                b"6 1 0:2 / /unrelated-\xff rw - nfs server:/share rw\n"
                b"7 1 0:1 / /deployment rw - ext4 /dev/example rw\n"
            ),
        }
        with patch(
            "aw_sdk.deployment.open",
            side_effect=lambda path, **options: io.TextIOWrapper(
                io.BytesIO(metadata[path]), **options
            ),
        ):
            check_filesystems(10)

    def test_changed_parent_identity_is_rejected_before_file_open(self) -> None:
        parent = os.stat(self.root)
        for device, inode in ((parent.st_dev + 1, parent.st_ino), (parent.st_dev, parent.st_ino + 1)):
            with patch(
                "aw_sdk.deployment.os.stat",
                return_value=SimpleNamespace(st_dev=device, st_ino=inode),
            ), patch("aw_sdk.deployment.os.open", wraps=os.open) as opened:
                with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                    read_file(str(self.descriptor))
                self.assertNotIn(self.descriptor.name, [call.args[0] for call in opened.call_args_list])

    def test_native_acl_metadata_on_parent_and_file_is_rejected(self) -> None:
        for directory in (False, True):
            def getxattr(path, attribute):
                is_directory = Path(path).is_dir()
                if is_directory == directory and attribute == "system.nfs4_acl":
                    return b"native-acl"
                raise OSError(errno.ENODATA, "no attribute")

            with patch("aw_sdk.deployment.os.getxattr", side_effect=getxattr):
                with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                    read_file(str(self.descriptor))

    def test_duplicates_and_trailing_data_are_rejected(self) -> None:
        encoded = json.dumps(self.value, separators=(",", ":"))
        for case in FIXTURE["duplicate"]:
            self.descriptor.write_text(
                encoded.replace('"' + case["key"] + '":', case["replacement"])
            )
            with self.assertRaisesRegex(AwError, "sdk_descriptor_invalid"):
                self.load()
        for text in (
            encoded + "false",
            "\ufeff" + encoded,
            "[]",
            encoded.replace('"endpoint":', '"endpoint":NaN,"wrong":'),
        ):
            self.descriptor.write_text(text)
            with self.assertRaisesRegex(AwError, "sdk_descriptor_invalid"):
                self.load()

    def test_unicode_and_raw_configuration_digest(self) -> None:
        self.value["endpoint"] = "/tmp/猫😀.sock"
        self.write()
        self.assertEqual(self.load()["endpoint"], self.value["endpoint"])
        self.value["endpoint"] = "/tmp/\ud800.sock"
        self.write()
        with self.assertRaisesRegex(AwError, "sdk_descriptor_invalid"):
            self.load()
        self.value["endpoint"] = "/tmp/aw.sock"
        self.write()
        self.config.write_bytes(self.config.read_bytes() + b"\n")
        with self.assertRaisesRegex(AwError, "sdk_descriptor_stale"):
            self.load()

    def test_files_are_bounded_regular_and_nonblocking(self) -> None:
        for path in (self.descriptor, self.config):
            original = path.read_bytes()
            path.unlink()
            os.mkfifo(path)
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                self.load()
            path.unlink()
            path.write_bytes(b"x" * (LIMIT + 1))
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                self.load()
            path.write_bytes(original)
            path.chmod(0o600)
        self.descriptor.unlink()
        with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
            self.load()

    def test_file_trust_matches_legacy_reader(self) -> None:
        for path in (self.config, self.descriptor):
            for mode in (0o620, 0o602, 0o4644):
                path.chmod(mode)
                with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                    self.load()
                path.chmod(0o600)
            alias = self.root / "alias"
            os.link(path, alias)
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                self.load()
            alias.unlink()
            path.rename(alias)
            path.symlink_to(alias)
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                self.load()
            path.unlink()
            alias.rename(path)
        for mode in (0o720, 0o702):
            self.root.chmod(mode)
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                self.load()
            self.root.chmod(0o700)
        alias = self.root / "directory-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        for path in (
            alias / "descriptor.json",
            self.root / ".." / self.root.name / "descriptor.json",
        ):
            with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                load_descriptor(str(self.config), str(path), self.profile, "app")
        self.assertEqual(self.load()["target"], "app")

    def test_extended_access_acl_is_rejected(self) -> None:
        for path in (self.config, self.descriptor):
            for mask in (4, 6):
                acl = struct.pack("<I", 2) + b"".join(
                    struct.pack("<HHI", tag, permissions, uid)
                    for tag, permissions, uid in (
                        (1, 6, 0xFFFFFFFF),
                        (2, mask, os.geteuid() + 1),
                        (4, 0, 0xFFFFFFFF),
                        (16, mask, 0xFFFFFFFF),
                        (32, 0, 0xFFFFFFFF),
                    )
                )
                os.setxattr(path, "system.posix_acl_access", acl)
                with self.assertRaisesRegex(AwError, "sdk_descriptor_file_invalid"):
                    self.load()
                os.removexattr(path, "system.posix_acl_access")
                path.chmod(0o600)
        self.assertEqual(self.load()["target"], "app")

    def test_qwenpaw_requires_service_and_uses_cli_generated_hints(self) -> None:
        document = deepcopy(FIXTURE["configuration"])
        document["spec"]["agents"]["app"]["adapter"] = "qwenpaw"
        for enabled in (False, True):
            document["spec"]["events"]["input.submit"]["enabled"] = enabled
            document["spec"]["events"]["input.submit"]["required"] = enabled
            self.config.write_text(json.dumps(document))
            result = subprocess.run(
                [
                    BINARY,
                    "config",
                    "descriptor",
                    "--config",
                    str(self.config),
                    "--agent",
                    "app",
                    "--profile",
                    "qwenpaw-installed-native-hooks",
                ],
                check=True,
                capture_output=True,
                timeout=10,
            )
            self.descriptor.write_bytes(result.stdout)
            with patch(
                "aw_sdk.configuration.subprocess.Popen",
                side_effect=AssertionError("cold-start subprocess"),
            ):
                wiring = load_configuration(
                    str(self.config),
                    QWENPAW_PROFILE,
                    None,
                    "/missing/aw",
                    descriptor_file=str(self.descriptor),
                    requires_service=True,
                )
            self.assertEqual(wiring["input_enabled"], enabled)
            self.assertEqual(wiring["availability"], "required")
            value = json.loads(result.stdout)
            value["availability"] = "native_fallback"
            self.descriptor.write_text(json.dumps(value))
            with self.assertRaisesRegex(AwError, "sdk_descriptor_profile_mismatch"):
                load_configuration(
                    str(self.config),
                    QWENPAW_PROFILE,
                    None,
                    "/missing/aw",
                    descriptor_file=str(self.descriptor),
                    requires_service=True,
                )

    def test_legacy_parser_keeps_target_selection(self) -> None:
        document = deepcopy(FIXTURE["configuration"])
        document["spec"]["agents"]["app"]["adapter"] = "qwenpaw"
        self.config.write_text(json.dumps(document))
        wiring = load_configuration(
            str(self.config), QWENPAW_PROFILE, None, BINARY, requires_service=True
        )
        self.assertEqual(wiring["target"], "app")
        self.assertTrue(wiring["input_enabled"])
        self.assertEqual(wiring["availability"], "required")

    def test_caller_profile_and_service_constraint_apply_to_both_paths(self) -> None:
        # A contract fixture, not a claim that this native entrypoint exists.
        profile = {"adapter": "qoder-sdk", "version": "fixture-v3", "entrypoint": "fixture-worker"}
        document = deepcopy(FIXTURE["configuration"])
        for enabled in (False, True):
            for required in (False, True):
                document["spec"]["events"]["input.submit"].update(enabled=enabled, required=required)
                self.config.write_text(json.dumps(document))
                if required and not enabled:
                    with self.assertRaises(subprocess.CalledProcessError):
                        load_configuration(str(self.config), profile, None, BINARY)
                    continue
                self.value.update(
                    profile=profile,
                    configuration_sha256=hashlib.sha256(self.config.read_bytes()).hexdigest(),
                    wait_for_input_submit=enabled,
                    availability="required" if enabled and required else "native_fallback",
                )
                self.write()
                for requires_service in (False, True):
                    with self.subTest(enabled=enabled, required=required, service=requires_service):
                        parsed = load_configuration(
                            str(self.config),
                            profile,
                            None,
                            BINARY,
                            requires_service=requires_service,
                        )
                        self.assertEqual(parsed["target"], "app")
                        self.assertEqual(parsed["input_enabled"], enabled)
                        self.assertEqual(
                            parsed["availability"],
                            "required"
                            if requires_service or (enabled and required)
                            else "native_fallback",
                        )
                        with patch(
                            "aw_sdk.configuration.subprocess.Popen",
                            side_effect=AssertionError("descriptor invoked parser"),
                        ):
                            if requires_service and not (enabled and required):
                                with self.assertRaisesRegex(
                                    AwError, "sdk_descriptor_profile_mismatch"
                                ):
                                    load_configuration(
                                        str(self.config),
                                        profile,
                                        None,
                                        "/missing/aw",
                                        descriptor_file=str(self.descriptor),
                                        requires_service=True,
                                    )
                            else:
                                self.assertEqual(
                                    load_configuration(
                                        str(self.config),
                                        profile,
                                        None,
                                        "/missing/aw",
                                        descriptor_file=str(self.descriptor),
                                        requires_service=requires_service,
                                    ),
                                    parsed,
                                )

    def test_descriptor_accepts_a_caller_owned_profile_without_framework_names(self) -> None:
        profile = {"adapter": "contract-fixture", "version": "fixture-v3", "entrypoint": "worker"}
        document = deepcopy(FIXTURE["configuration"])
        document["spec"]["agents"]["app"]["adapter"] = profile["adapter"]
        self.config.write_text(json.dumps(document))
        self.value["configuration_sha256"] = hashlib.sha256(self.config.read_bytes()).hexdigest()
        self.value["profile"] = profile
        self.write()
        with patch(
            "aw_sdk.configuration.subprocess.Popen", side_effect=AssertionError("unexpected parser")
        ):
            wiring = load_configuration(
                str(self.config),
                profile,
                "app",
                "/missing/aw",
                descriptor_file=str(self.descriptor),
            )
            self.assertEqual(wiring["availability"], self.value["availability"])
            for field in profile:
                with self.subTest(field=field), self.assertRaisesRegex(
                    AwError, "sdk_descriptor_profile_mismatch"
                ):
                    load_configuration(
                        str(self.config),
                        {**profile, field: "mismatch"},
                        "app",
                        "/missing/aw",
                        descriptor_file=str(self.descriptor),
                    )


if __name__ == "__main__":
    unittest.main()
