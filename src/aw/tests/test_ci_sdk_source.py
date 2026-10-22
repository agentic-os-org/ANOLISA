"""Public source export rejects incomplete and private dependency closures."""

import copy
import sys
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AW / "scripts"))
import sdk_source
from check_sdk_source import local_packages, locked_dependencies


class SourceFixture:
    def __init__(self) -> None:
        self.manifests = {
            "public/app": {
                "package": {"name": "app", "version": "1.0.0"},
                "workspace": {"dependencies": {"public": {"path": "../lib"}}},
                "dependencies": {"public": {"workspace": True}},
            },
            "public/lib": {
                "package": {"name": "public", "version": "1.0.0"},
                "workspace": {},
                "target": {"cfg(unix)": {"dev-dependencies": {"extra": {"path": "../extra"}}}},
            },
            "public/extra": {
                "package": {"name": "extra", "version": "1.0.0"},
                "workspace": {},
            },
        }

    def manifest(self, directory: str) -> dict:
        return self.manifests[directory]

    def workspace(self, directory: str) -> str:
        return directory


class SdkSourceTests(unittest.TestCase):
    def test_workspace_inheritance_and_target_test_dependencies_are_included(self) -> None:
        source = SourceFixture()
        public = set(source.manifests)
        with patch.object(sdk_source, "ROOTS", ["public/app"]), patch.object(sdk_source, "PUBLIC", public):
            packages, workspaces = sdk_source.closure(source)
        self.assertEqual(packages, public)
        self.assertEqual(workspaces, {name: {name} for name in public})

    def test_private_dependencies_cannot_enter_public_materials(self) -> None:
        source = SourceFixture()
        with patch.object(sdk_source, "ROOTS", ["public/app"]), patch.object(
            sdk_source, "PUBLIC", {"public/app", "public/lib"}
        ), self.assertRaisesRegex(ValueError, "not an admitted public library"):
            sdk_source.closure(source)

    def test_cargo_manifest_roundtrip_preserves_features_lints_and_targets(self) -> None:
        document = {
            "package": {"name": "sdk", "version": "0.1.0", "edition": "2024"},
            "workspace": {
                "members": ["."], "resolver": "3",
                "dependencies": {"shared": {"path": "../shared", "version": "=1.2.3"}},
                "lints": {"clippy": {"all": {"level": "deny", "priority": -1}}},
            },
            "features": {"default": ["sec-core"], "sec-core": ["dep:shared"]},
            "dependencies": {"shared": {"workspace": True, "optional": True}},
            "example": [{"name": "app", "required-features": ["sec-core"]}],
        }
        original = copy.deepcopy(document)
        self.assertEqual(tomllib.loads(sdk_source.manifest_bytes(document).decode()), original)
        self.assertEqual(document, original)
        with self.assertRaises(ValueError):
            sdk_source.value(0.5)

    def test_source_paths_cannot_escape_the_archive(self) -> None:
        self.assertEqual(sdk_source.joined("src/sdk", "../shared"), "src/shared")
        for path in ("/private/source", "../../../private"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                sdk_source.joined("src/sdk", path)

    def test_consumer_paths_and_lockfiles_cannot_escape_the_export(self) -> None:
        metadata = {"packages": [
            {"source": None, "manifest_path": "/tmp/export/src/Cargo.toml"},
            {"source": "registry+crates.io", "manifest_path": "/registry/Cargo.toml"},
        ]}
        local_packages(metadata, Path("/tmp/export"))
        with self.assertRaisesRegex(ValueError, "escaped"):
            local_packages(metadata, Path("/tmp/other"))
        locked = b'[[package]]\nname = "dep"\nversion = "1.0.0"\nchecksum = "a"\n'
        locked_dependencies(locked, locked + b'[[package]]\nname = "sdk-consumer"\nversion = "0.0.0"\n')
        for changed in (locked.replace(b"1.0.0", b"1.0.1"), locked.replace(b'"a"', b'"b"')):
            with self.assertRaisesRegex(ValueError, "locked dependency"):
                locked_dependencies(locked, changed)
