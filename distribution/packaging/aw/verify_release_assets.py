"""Verify the entire AW asset set before publication or candidate promotion."""

import json
from pathlib import Path
import sys
import tarfile

from common import Cancellation, digest
from package import COMPONENTS
from sdk_candidate import sdk_records


def verify(directory: Path, candidate: str, version: str) -> None:
    receipt = directory / "sdk-candidate.json"
    records = sdk_records(candidate, receipt)
    cancel = Cancellation()
    sums = {}
    for component in COMPONENTS.values():
        archive = f"{component}-{version}-linux-x86_64"
        name = f"{archive}.tar.gz"
        record = "share/doc/aw/source.json" if component == "aw-core" else "package.json"
        with tarfile.open(directory / name, "r:gz") as tar:
            member = tar.getmember(f"{archive}/{record}")
            if not member.isfile():
                raise ValueError(f"invalid native receipt in {name}")
            source = json.load(tar.extractfile(member))
        if (source["package_id"], source["version"], source["source_commit"]) != (
            component, version, candidate,
        ):
            raise ValueError(f"native receipt mismatch in {name}")
        sums[name] = digest(directory / name, cancel)
    for row in records:
        name = row["file"]
        suffix = ".tgz" if row["kind"] == "npm" else ".whl"
        if Path(name).name != name or not name.endswith(suffix) or name in sums:
            raise ValueError(f"invalid SDK artifact name: {name}")
        if digest(directory / name, cancel) != row["sha256"]:
            raise ValueError(f"SDK checksum mismatch: {name}")
        sums[name] = row["sha256"]
    sums[receipt.name] = digest(receipt, cancel)
    if {p.name for p in directory.iterdir()} != {*sums, "SHA256SUMS"}:
        raise ValueError("unexpected release asset set")
    # The manifest also binds the native archives verified by the install job.
    expected = sorted(f"{value}  {name}" for name, value in sums.items())
    if sorted((directory / "SHA256SUMS").read_text().splitlines()) != expected:
        raise ValueError("release asset checksums differ from SHA256SUMS")


if __name__ == "__main__":
    verify(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
