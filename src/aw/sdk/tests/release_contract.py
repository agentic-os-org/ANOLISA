"""Exercise the verified release SDK artifacts against the matching packaged AW binary."""

import json
from pathlib import Path
import sys

import direct_contract
import python_package
import typescript_package


def main(binary: Path, receipt: Path) -> None:
    receipt = receipt.resolve()
    records = json.loads(receipt.read_text())["artifacts"]
    packages = {
        (row["kind"], row["component"]): receipt.parent / row["file"]
        for row in records
    }
    aw = str(binary.resolve(strict=True))
    direct_contract.run(aw)
    python_package.main(
        aw, (packages["wheel", "core"], packages["wheel", "claude"], packages["wheel", "qwenpaw"])
    )
    typescript_package.main(
        aw, (packages["npm", "core"], packages["npm", "qoder"], packages["npm", "pi"])
    )


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
