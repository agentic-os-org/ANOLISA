"""SDK gate failures must remain visible without relying on Rust compilation."""

import copy
import importlib.util
import unittest
import sys
from pathlib import Path

AW = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AW / "scripts"))
SPEC = importlib.util.spec_from_file_location("sdk_gate", AW / "scripts/check_sdk.py")
sdk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sdk)


class SdkGateTests(unittest.TestCase):
    def test_component_selection_rejects_leakage_and_missing_reports(self) -> None:
        sdk.isolation("aw-sdk v0.1.0\nasc-daemon-client v1.0.1\nserde v1", "sec-core")
        sdk.isolation("aw-sdk v0.1.0\ntokenless-runtime v0.8.7", "tokenless")
        sdk.isolation("aw-sdk v0.1.0\nthiserror v2", "")
        for tree, feature in [
            ("", "sec-core"),
            ("aw-sdk v0.1.0", "sec-core"),
            ("aw-sdk v0.1.0\nasc-daemon-client v1", "tokenless"),
            ("aw-sdk v0.1.0\ntokenless-runtime v1", ""),
            ("aw-sdk v0.1.0\nasc-daemon-client v1\ntokenless-runtime v1", "sec-core"),
            ("aw-sdk v0.1.0\naw-service v1", ""),
            ("aw-sdk v0.1.0\naw-provider-sec-core v1", ""),
        ]:
            with self.subTest(tree=tree, feature=feature), self.assertRaises(ValueError):
                sdk.isolation(tree, feature)

    def test_absent_or_ignored_required_cases_fail(self) -> None:
        sdk.required_cases(sdk.REQUIRED)
        for target, cases in sdk.REQUIRED.items():
            for case in cases:
                missing = copy.deepcopy(sdk.REQUIRED)
                missing[target].remove(case)
                with self.subTest(case=case), self.assertRaises(ValueError):
                    sdk.required_cases(missing)
        with self.assertRaises(ValueError):
            sdk.required_cases({})

    def test_component_dependency_paths_and_versions_are_audited(self) -> None:
        root = AW / "sdk/rust"
        repo = AW.parents[1]
        dependencies = [
            {"name": name, "path": str(repo / path), "source": None, "req": "=1.0.1", "optional": True}
            for name, path in [
                ("asc-daemon-client", "src/agent-sec-core/v2/crates/asc-daemon-client"),
                ("tokenless-runtime", "src/tokenless/crates/tokenless-runtime"),
                ("tokenless-protocol", "src/tokenless/crates/tokenless-protocol"),
            ]
        ] + [
            {"name": name, "source": "registry+fixture", "path": None}
            for name in ("thiserror", "serde_json", "tempfile")
        ]
        metadata = {
            "workspace_members": ["sdk"],
            "packages": [{
                "name": "aw-sdk", "id": "sdk", "manifest_path": str(root / "Cargo.toml"),
                "dependencies": dependencies, "targets": [{"src_path": str(root / "src/lib.rs")}],
            }],
        }
        sdk.layout(metadata, root)
        for key, value in [
            ("optional", False), ("req", "*"), ("path", str(repo / "private")),
            ("source", "git+unfixed"), ("name", "aw-service"),
        ]:
            invalid = copy.deepcopy(metadata)
            invalid["packages"][0]["dependencies"][0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                sdk.layout(invalid, root)
