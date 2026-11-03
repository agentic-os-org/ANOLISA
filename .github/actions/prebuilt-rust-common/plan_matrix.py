#!/usr/bin/env python3
"""Plan the target matrix for components with prebuilt release packages."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from targets import component_targets


def build_matrix(
    component: str, tokenless_source_root: Path | None = None
) -> dict[str, list[dict[str, str]]]:
    """Return the matrix rows supported by the selected component."""
    return {
        "include": [
            {"component": component, **target}
            for target in component_targets(component, tokenless_source_root)
        ]
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--component", required=True)
    parser.add_argument("--tokenless-source-root", type=Path)
    args = parser.parse_args()

    matrix = build_matrix(args.component, args.tokenless_source_root)
    print(f"enabled={'true' if matrix['include'] else 'false'}")
    print(f"matrix={json.dumps(matrix, separators=(',', ':'), sort_keys=True)}")


if __name__ == "__main__":
    main()
