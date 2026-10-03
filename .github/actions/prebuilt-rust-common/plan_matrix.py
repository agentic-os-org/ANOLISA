#!/usr/bin/env python3
"""Plan the target matrix for components with prebuilt release packages."""

from __future__ import annotations

import argparse
import json

from targets import TARGETS


def build_matrix(component: str) -> dict[str, list[dict[str, str]]]:
    """Return the matrix rows supported by the selected component."""
    return {
        "include": [
            {"component": component, **target}
            for target in TARGETS.get(component, ())
        ]
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--component", required=True)
    args = parser.parse_args()

    matrix = build_matrix(args.component)
    print(f"enabled={'true' if matrix['include'] else 'false'}")
    print(f"matrix={json.dumps(matrix, separators=(',', ':'), sort_keys=True)}")


if __name__ == "__main__":
    main()
