"""The AW gate covers private storage, runtime leases and distributor tooling."""

import json
from pathlib import Path
import tomllib
from unittest.mock import patch

from test_ci_checks import AW, GateFixture, gate


class StoreBoundaryGateTests(GateFixture):
    def test_store_and_runtime_remain_inside_framework_crates(self) -> None:
        repo = AW.parents[1]
        manifest = tomllib.loads((AW / "Cargo.toml").read_text())
        dependencies = manifest["workspace"]["dependencies"]
        self.assertNotIn("local-package", dependencies)
        self.assertNotIn("local-package-runtime", dependencies)
        for crate in ("aw-package", "aw-exec"):
            manifest = tomllib.loads((AW / "crates" / crate / "Cargo.toml").read_text())
            self.assertNotIn("local-package", manifest.get("dependencies", {}))
            self.assertNotIn("local-package-runtime", manifest.get("dependencies", {}))
        source = (AW / "crates/aw-package/src/lib.rs").read_text()
        self.assertIn("mod store;", source)
        self.assertNotIn("pub mod store;", source)
        for workflow in ("aw-ci.yml", "aw-integration.yml"):
            text = (repo / ".github/workflows" / workflow).read_text()
            self.assertNotIn("distribution/crates/", text)
            self.assertIn("providers/agent-sec-core", text)
            self.assertIn("providers/tokenless", text)

    def test_three_workspaces_have_full_checks_and_compiled_inventory(self) -> None:
        repo = self.root
        aw = repo / "src/aw"
        providers = [repo / "providers" / name for name in ("agent-sec-core", "tokenless")]
        calls = []

        def run(command: list[str], cwd: Path, **kwargs: object) -> str:
            calls.append((command, cwd))
            if command[:2] == ["cargo", "metadata"]:
                return json.dumps(
                    {"workspace_root": str(cwd), "packages": [], "workspace_members": []}
                )
            return ""

        with patch.dict(gate.os.environ), patch.object(gate, "REPO", repo), patch.object(
            gate, "AW", aw
        ), patch.object(gate, "candidate", return_value="a" * 40), patch.object(
            gate, "selftest"
        ), patch.object(
            gate, "run", side_effect=run
        ), patch.object(
            gate, "structure"
        ), patch.object(
            gate, "package_permissions"
        ), patch.object(
            gate, "check_inventory"
        ) as inventory, patch(
            "check_native.load"
        ):
            gate.check()
        for root in [aw, *providers]:
            for command in ("fmt", "clippy", "test", "doc"):
                self.assertTrue(
                    any(args[:2] == ["cargo", command] and cwd == root for args, cwd in calls),
                    f"missing {command} for {root}",
                )
        metadata = inventory.call_args.args[0]
        self.assertNotIn("shared_workspaces", metadata)
        self.assertEqual([Path(row["workspace_root"]) for row in metadata["provider_workspaces"]], providers)
        cargo_roots = {cwd for args, cwd in calls if args[0] == "cargo"}
        self.assertEqual(cargo_roots, {aw, *providers})

    def test_delivery_scripts_are_checked_without_a_framework_binary(self) -> None:
        source = (AW / "scripts/check.py").read_text()
        self.assertIn('"distribution/packaging/aw/tests"', source)
        manifest = tomllib.loads((AW / "crates/aw-package/Cargo.toml").read_text())
        self.assertNotIn("bin", manifest)
        self.assertFalse((AW / "crates/aw-package/src/bin").exists())
