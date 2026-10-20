#!/usr/bin/env python3
"""Export committed public SDK sources without a complete OS checkout."""

import argparse
import json
from pathlib import Path

from sdk_source import Snapshot, export


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", default="HEAD", help="committed source revision")
    parser.add_argument("--output", type=Path, required=True, help="new .tar.gz archive")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[3]
    result = export(Snapshot(repo, args.revision), args.output.resolve())
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
