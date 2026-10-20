"""Validate the application SDK without adding it to the framework workspace."""

import json
from pathlib import Path
from typing import Callable

from check_inventory import collect

REQUIRED = {
    "aw-sdk/test/client": {
        "disabled_components_are_explicit",
        "tokenless_rejects_ambient_state_and_relative_rtk",
        "construction_does_not_connect_or_fall_back_to_a_local_scanner",
    },
    "aw-sdk/test/tokenless": {
        "real_runtime_matches_sdk_and_requires_visible_marker_for_recovery",
        "failed_and_rtk_owned_results_are_preserved",
        "pre_tool_uses_explicit_rtk_and_retains_host_adoption_choice",
    },
}


def layout(metadata: dict, root: Path) -> None:
    """Keep one public package and component-owned implementation dependencies."""
    packages = [p for p in metadata["packages"] if p["id"] in metadata["workspace_members"]]
    if len(packages) != 1 or packages[0]["name"] != "aw-sdk":
        raise ValueError("SDK workspace must contain exactly the aw-sdk package")
    package = packages[0]
    if Path(package["manifest_path"]).resolve() != (root / "Cargo.toml").resolve():
        raise ValueError("SDK package must stay in its independent workspace")
    repo = root.parents[3]
    local = {
        "asc-daemon-client": repo / "src/agent-sec-core/v2/crates/asc-daemon-client",
        "tokenless-runtime": repo / "src/tokenless/crates/tokenless-runtime",
        "tokenless-protocol": repo / "src/tokenless/crates/tokenless-protocol",
    }
    allowed = set(local) | {"thiserror", "serde_json", "tempfile"}
    if {d["name"] for d in package["dependencies"]} != allowed:
        raise ValueError("SDK dependencies changed; review the application boundary")
    for dependency in package["dependencies"]:
        name = dependency["name"]
        if name in local:
            if (
                dependency.get("source") is not None
                or not dependency.get("path")
                or Path(dependency["path"]).resolve() != local[name].resolve()
                or not dependency.get("optional")
                or not dependency["req"].startswith("=")
            ):
                raise ValueError(f"{name}: SDK needs an optional, fixed component dependency")
        elif dependency.get("path") or not dependency.get("source", "").startswith("registry+"):
            raise ValueError(f"{name}: SDK external dependencies must use the locked registry")
    for target in package["targets"]:
        if not any(
            Path(target["src_path"]).resolve().is_relative_to(root / kind)
            for kind in ("src", "tests", "examples")
        ):
            raise ValueError("SDK target escaped its audited source directory")


def isolation(tree: str, selected: str) -> None:
    """Reject component leakage and imports of AW runtime implementation."""
    names = {line.split()[0] for line in tree.splitlines() if line.strip()}
    if "aw-sdk" not in names:
        raise ValueError("SDK dependency report is missing the consumer")
    if any(name.startswith("aw-") and name != "aw-sdk" for name in names):
        raise ValueError("application SDK must not depend on AW framework or Providers")
    for feature, prefix in (("sec-core", "asc-"), ("tokenless", "tokenless-")):
        present = any(name.startswith(prefix) for name in names)
        if present != (selected == feature):
            raise ValueError(f"{selected or 'empty'} SDK selection has incorrect {feature} dependencies")


def required_cases(runnable: dict[str, set[str]]) -> None:
    """A successful zero-test build cannot replace SDK acceptance."""
    for target, cases in REQUIRED.items():
        missing = cases - runnable.get(target, set())
        if missing:
            raise ValueError(f"SDK acceptance missing runnable cases: {target}: {sorted(missing)}")


def check(aw: Path, run: Callable) -> None:
    """Compile, inventory and run default and isolated component selections."""
    root = aw / "sdk/rust"
    run(["cargo", "fmt", "--all", "--", "--check"], root)
    metadata = json.loads(run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        root, capture=True,
    ))
    layout(metadata, root)
    run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], root)
    required_cases(collect(root, metadata, run))
    run(["cargo", "test", "--workspace", "--locked"], root)
    for selected in ("sec-core", "tokenless", ""):
        features = ["--no-default-features"] + (["--features", selected] if selected else [])
        tree = run(
            ["cargo", "tree", "--locked", "--edges", "normal,build", "--prefix", "none",
             "--format", "{p}", *features],
            root, capture=True,
        )
        isolation(tree, selected)
        run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", *features,
             "--", "-D", "warnings"], root)
        run(["cargo", "test", "--workspace", "--locked", *features], root)
    run(["cargo", "doc", "--workspace", "--no-deps", "--locked"], root)
