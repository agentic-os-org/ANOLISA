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
from common import Cancellation, copy_file, digest, run

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


def prepare_rtk(repo: Path, root: Path, cancel: Cancellation) -> Path:
    """Use owned pinned sources without modifying a local RTK checkout."""
    shared = root / "crates/tokenless-stats/src"
    for name in RTK_SHARED_SOURCES:
        copy_file(
            repo / "src/tokenless/crates/tokenless-stats/src" / name, shared / name, cancel, 0o644
        )
    destination = root / "third_party/rtk"
    run(["bash", "src/tokenless/packaging/build/setup-rtk.sh", str(destination)], repo, cancel, 180)
    return destination


def rtk_provenance(
    repo: Path, rtk: Path, source: str, binary: Path, cancel: Cancellation
) -> dict[str, object]:
    """Receipt the actual isolated RTK inputs without adding backend package payloads."""
    shared = rtk.parent.parent / "crates/tokenless-stats/src"
    inputs = [
        ("LICENSE", rtk / "LICENSE", "upstream:LICENSE"),
        ("source-commit", rtk / ".anolisa-rtk-commit", "setup-rtk.sh:.anolisa-rtk-commit"),
        *(
            (
                f"tokenless-stats/{name}", shared / name,
                f"src/tokenless/crates/tokenless-stats/src/{name}",
            )
            for name in RTK_SHARED_SOURCES
        ),
        (
            "setup-rtk.sh", repo / "src/tokenless/packaging/build/setup-rtk.sh",
            "src/tokenless/packaging/build/setup-rtk.sh",
        ),
        *(
            (name, repo / f"src/tokenless/third_party/patches/{name}",
             f"src/tokenless/third_party/patches/{name}")
            for name in ("rtk-tokenless-stats.patch", "rtk-pytest-error-report.patch")
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
        "binary": {"name": "rtk", "sha256": digest(binary, cancel)},
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
        compile_target(repo / "src/tokenless", ("tokenless-cli",), cancel)
        with tempfile.TemporaryDirectory(prefix="aw-integration-rtk-") as temporary:
            rtk = prepare_rtk(repo, Path(temporary), cancel)
            compile_target(rtk, ("rtk",), cancel, repo / "src/aw/target")
            provenance = rtk_provenance(
                repo, rtk, source, repo / "src/aw/target/release/rtk", cancel
            )
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
