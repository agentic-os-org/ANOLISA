#!/usr/bin/env python3
"""Build external backend programs for integration, without assembling AW packages."""

from __future__ import annotations

import argparse
import json
import platform
import sys
import tempfile
from pathlib import Path

from build import SOURCE, compile_target, recheck_source, source_commit
from common import Cancellation, digest, run

RTK_SHARED_SOURCES = ("home.rs", "path_policy.rs", "rtk_outcome.rs")


def backends(selection: list[str] | tuple[str, ...]) -> tuple[str, ...]:
    """Select explicit backend capabilities without adding package targets."""
    selected = tuple(word for value in selection for word in value.split())
    if (
        not selected
        or len(selected) != len(set(selected))
        or any(v not in {"sec-core", "tokenless"} for v in selected)
    ):
        raise ValueError("backends must be a nonempty unique list of sec-core, tokenless")
    return selected


def prepare_tokenless(repo: Path, source: str, root: Path, cancel: Cancellation) -> Path:
    """Export committed Tokenless sources and fetch a fresh pinned RTK inside them.

    Tokenless links RTK through third_party/rtk, so building the export keeps a
    cached RTK checkout, its local edits and other ignored files out of the CLI.
    """
    archive = root / "tokenless.tar"
    run(
        ["git", "archive", "--format=tar", f"--output={archive}", f"{source}:src/tokenless"],
        repo, cancel, 60,
    )
    tree = root / "tokenless"
    tree.mkdir()
    run(["tar", "-xf", str(archive), "-C", str(tree)], root, cancel, 60)
    archive.unlink()
    run(["bash", "packaging/build/setup-rtk.sh"], tree, cancel, 180)
    return tree


def rtk_provenance(
    tree: Path, source: str, binary: Path, cancel: Cancellation
) -> dict[str, object]:
    """Receipt the exported RTK build inputs against the CLI that links them."""
    rtk = tree / "third_party/rtk"
    inputs = [
        ("LICENSE", rtk / "LICENSE", "upstream:LICENSE"),
        ("source-commit", rtk / ".anolisa-rtk-commit", "setup-rtk.sh:.anolisa-rtk-commit"),
        *(
            (
                f"tokenless-stats/{name}", tree / "crates/tokenless-stats/src" / name,
                f"src/tokenless/crates/tokenless-stats/src/{name}",
            )
            for name in RTK_SHARED_SOURCES
        ),
        (
            "setup-rtk.sh", tree / "packaging/build/setup-rtk.sh",
            "src/tokenless/packaging/build/setup-rtk.sh",
        ),
        *(
            (patch.name, patch, f"src/tokenless/third_party/patches/{patch.name}")
            for patch in sorted((tree / "third_party/patches").glob("*.patch"))
        ),
    ]
    return {
        "upstream": {
            "repository": "https://github.com/rtk-ai/rtk.git", "release": "v0.49.0",
            "commit": (rtk / ".anolisa-rtk-commit").read_text().strip(),
        },
        "anolisa_source_commit": source,
        "shared_sources_reason": (
            "The Tokenless patch includes home.rs, path_policy.rs and rtk_outcome.rs "
            "by relative path to share home, state-path and call-scoped receipt rules."
        ),
        "inputs": [
            {"source": origin, "input": name, "sha256": digest(path, cancel)}
            for name, path, origin in inputs
        ],
        "binary": {"name": "tokenless", "sha256": digest(binary, cancel)},
    }


def build(repo: Path, selection: list[str] | tuple[str, ...], cancel: Cancellation) -> None:
    """Prepare selected backends; framework and bridge builds remain separate."""
    selected = backends(selection)
    if platform.system() != "Linux" or platform.machine() not in {"x86_64", "aarch64"}:
        raise ValueError("backend integration builds require native Linux x86_64 or aarch64")
    source = source_commit(repo, cancel)
    provenance = None
    if "sec-core" in selected:
        compile_target(repo / "src/agent-sec-core/v2", ("asc-cli", "asc-daemon"), cancel)
    if "tokenless" in selected:
        cli = repo / "src/tokenless/target/release/tokenless"
        with tempfile.TemporaryDirectory(prefix="aw-integration-tokenless-") as temporary:
            tree = prepare_tokenless(repo, source, Path(temporary), cancel)
            # The export is temporary; keep the CLI where consumers expect it.
            compile_target(tree, ("tokenless-cli",), cancel, repo / "src/tokenless/target")
            provenance = rtk_provenance(tree, source, cli, cancel)
    recheck_source(repo, source, cancel)
    if provenance is not None:
        cancel.check()
        receipt = repo / "src/aw/target/rtk-provenance.json"
        receipt.parent.mkdir(parents=True, exist_ok=True)
        receipt.write_text(json.dumps(provenance, indent=2) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backends", nargs="+", default=["sec-core", "tokenless"])
    args = parser.parse_args()
    cancel = Cancellation()
    cancel.register()
    try:
        build(SOURCE, args.backends, cancel)
    except (OSError, RuntimeError, ValueError) as error:
        print(f"aw-integration-backends: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
