"""Build SDKs from a fixed Git commit and inspect matching RPM declarations."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
import tarfile
import tempfile
import zipfile

from common import Cancellation, digest, run
from package import publish

PACKAGES = {"aw", "aw-provider-sec-core", "aw-provider-tokenless"}


def commit_id(repo: Path, requested: str, cancel: Cancellation) -> str:
    """Require an immutable full commit, independent of checkout modifications."""
    if not re.fullmatch(r"[0-9a-f]{40}", requested):
        raise ValueError("candidate must be a full lowercase Git commit SHA")
    actual = run(
        ["git", "rev-parse", "--verify", requested + "^{commit}"], repo, cancel, 30
    )
    if actual.decode().strip() != requested:
        raise ValueError("candidate does not resolve to the requested commit")
    return requested


def sdk_payload(path: Path) -> dict[str, bytes]:
    """Read shipped files without extracting untrusted package paths."""
    result = {}
    if path.suffix == ".whl":
        with zipfile.ZipFile(path) as archive:
            for item in archive.infolist():
                if not item.is_dir():
                    if item.filename in result:
                        raise ValueError("duplicate wheel member: " + item.filename)
                    result[item.filename] = archive.read(item)
    else:
        with tarfile.open(path, "r:gz") as archive:
            for item in archive:
                if item.isdir():
                    continue
                if not item.isfile() or not item.name.startswith("package/"):
                    raise ValueError("unexpected npm member: " + item.name)
                if item.name in result:
                    raise ValueError("duplicate npm member: " + item.name)
                source = archive.extractfile(item)
                if source is None:
                    raise ValueError("unreadable npm member: " + item.name)
                result[item.name] = source.read()
    return result


def build(
    repo: Path, candidate: str, output: Path, python: str, cancel: Cancellation
) -> dict:
    """Build only archived candidate sources; publish a receipt after both succeed."""
    commit_id(repo, candidate, cancel)
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError("output directory must be empty")
    with tempfile.TemporaryDirectory(
        prefix="aw-sdk-candidate-", dir=output
    ) as directory:
        root = Path(directory)
        archive = root / "source.tar"
        run(
            [
                "git",
                "archive",
                "--output",
                str(archive),
                candidate,
                "LICENSE",
                "src/aw",
            ],
            repo,
            cancel,
            60,
        )
        source = root / "source"
        source.mkdir()
        run(["tar", "-xf", str(archive), "-C", str(source)], root, cancel, 60)
        packages = root / "packages"
        packages.mkdir()
        run(
            ["npm", "pack", "--offline", "--pack-destination", str(packages)],
            source / "src/aw/sdk/typescript",
            cancel,
            120,
        )
        run(
            [
                python,
                "-m",
                "pip",
                "wheel",
                "--no-build-isolation",
                "--no-deps",
                "--no-index",
                str(source / "src/aw/sdk/python"),
                "--wheel-dir",
                str(packages),
            ],
            source,
            cancel,
            120,
        )
        (npm,) = packages.glob("*.tgz")
        (wheel,) = packages.glob("*.whl")
        records = []
        for kind, path in (("npm", npm), ("wheel", wheel)):
            payload = sdk_payload(path)
            records.append(
                {
                    "kind": kind,
                    "file": path.name,
                    "sha256": digest(path, cancel),
                    "files": len(payload),
                }
            )
        python_tools = json.loads(
            run(
                [
                    python,
                    "-c",
                    "import importlib.metadata, json; "
                    "print(json.dumps({name: importlib.metadata.version(name) "
                    "for name in ('pip', 'setuptools', 'wheel')}))",
                ],
                root,
                cancel,
                10,
            )
        )
        receipt = {
            "schema": "aw-sdk-candidate/v1",
            "commit": candidate,
            "artifacts": records,
            "tools": {
                "node": run(["node", "--version"], root, cancel, 10).decode().strip(),
                "npm": run(["npm", "--version"], root, cancel, 10).decode().strip(),
                "python": run([python, "--version"], root, cancel, 10).decode().strip(),
                **python_tools,
            },
            "acceptance": "package-build-only",
        }
        receipt_path = packages / "sdk-candidate.json"
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
        publish([npm, wheel, receipt_path], output, cancel)
        return receipt


def verify_sdk(
    repo: Path, candidate: str, receipt_path: Path, python: str, cancel: Cancellation
) -> dict:
    """Compare shipped payloads against a fresh build, not a supplied SHA alone."""
    receipt = json.loads(receipt_path.read_text())
    if not isinstance(receipt, dict):
        raise ValueError("SDK receipt must be an object")
    if (
        receipt.get("schema") != "aw-sdk-candidate/v1"
        or receipt.get("commit") != candidate
    ):
        raise ValueError("SDK receipt candidate mismatch")
    records = receipt.get("artifacts", [])
    if (
        not isinstance(records, list)
        or len(records) != 2
        or any(
            not isinstance(row, dict)
            or any(
                not isinstance(row.get(key), str) for key in ("kind", "file", "sha256")
            )
            for row in records
        )
        or {row["kind"] for row in records} != {"npm", "wheel"}
    ):
        raise ValueError(
            "receipt must contain exactly one npm and one wheel with file and sha256"
        )
    with tempfile.TemporaryDirectory(prefix="aw-sdk-rebuild-") as directory:
        fresh = Path(directory)
        rebuilt = build(repo, candidate, fresh, python, cancel)
        expected = {row["kind"]: fresh / row["file"] for row in rebuilt["artifacts"]}
        for row in records:
            if Path(row["file"]).name != row["file"]:
                raise ValueError("SDK artifact must be next to its receipt")
            if row["file"] != expected[row["kind"]].name:
                raise ValueError(
                    "SDK artifact filename differs from candidate build: " + row["file"]
                )
            path = receipt_path.parent / row["file"]
            if digest(path, cancel) != row["sha256"]:
                raise ValueError("SDK artifact checksum mismatch: " + row["file"])
            if sdk_payload(path) != sdk_payload(expected[row["kind"]]):
                raise ValueError(
                    "SDK payload differs from candidate build: " + row["file"]
                )
    return {"commit": candidate, "sdk_payloads": "match-candidate-rebuild"}


def rpm_declaration(path: Path, cancel: Cancellation) -> dict:
    """Inspect actual RPM metadata and its embedded source record without install."""
    fields = (
        run(
            [
                "rpm",
                "-qp",
                "--qf",
                "%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}",
                str(path),
            ],
            path.parent,
            cancel,
            30,
        )
        .decode()
        .split("\t")
    )
    if len(fields) != 4 or fields[0] not in PACKAGES:
        raise ValueError("unexpected RPM package: " + path.name)
    name, version, release, arch = fields
    # Fixed shell text preserves pipeline failures; filenames stay positional data.
    raw = run(
        [
            "bash",
            "-o",
            "pipefail",
            "-c",
            'rpm2cpio "$1" | cpio --quiet -i --to-stdout "$2" "$3"',
            "aw-rpm-source",
            str(path),
            "./usr/share/doc/" + name + "/source.json",
            "usr/share/doc/" + name + "/source.json",
        ],
        path.parent,
        cancel,
        30,
    )
    source = json.loads(raw)
    if not isinstance(source, dict):
        raise ValueError("RPM source record must be an object: " + path.name)
    # Archive source records use source_commit; native RPM recipes also use commit.
    commit = source.get("source_commit", source.get("commit"))
    if "source_commit" in source and "commit" in source and source["commit"] != commit:
        raise ValueError("conflicting RPM source commits: " + path.name)
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError(
            "RPM source record requires a full source commit: " + path.name
        )
    return {
        "name": name,
        "version": version,
        "release": release,
        "arch": arch,
        "commit": commit,
        "file": path.name,
        "sha256": digest(path, cancel),
    }


def check_rpm_set(records: list[dict], candidate: str) -> None:
    """Reject incomplete, stale or mixed RPM combinations."""
    if len(records) != 3 or {row["name"] for row in records} != PACKAGES:
        raise ValueError(
            "require exactly aw, aw-provider-sec-core and aw-provider-tokenless"
        )
    if any(row["commit"] != candidate for row in records):
        raise ValueError("RPM declared source does not match candidate")
    if len({(row["version"], row["release"], row["arch"]) for row in records}) != 1:
        raise ValueError("RPM version, release and architecture must match")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("build", "verify"))
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--candidate", required=True)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--rpm", type=Path, action="append", default=[])
    args = parser.parse_args()
    cancel = Cancellation()
    cancel.register()
    try:
        repo = args.repo.resolve()
        commit_id(repo, args.candidate, cancel)
        if args.action == "build":
            if args.output is None or args.receipt or args.rpm:
                parser.error(
                    "build requires --output and does not accept --receipt or --rpm"
                )
            report = build(
                repo, args.candidate, args.output.resolve(), args.python, cancel
            )
        else:
            if args.receipt is None or args.output:
                parser.error("verify requires --receipt and does not accept --output")
            report = verify_sdk(
                repo, args.candidate, args.receipt.resolve(), args.python, cancel
            )
            if args.rpm:
                records = [rpm_declaration(path.resolve(), cancel) for path in args.rpm]
                check_rpm_set(records, args.candidate)
                report["rpms"] = records
                report["rpm_provenance"] = (
                    "declared-source-only; require controlled offline build evidence"
                )
            report["acceptance"] = (
                "package-identity-only; native and business acceptance pending"
            )
        print(json.dumps(report, indent=2))
    except (
        OSError,
        RuntimeError,
        ValueError,
        KeyError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        parser.exit(1, f"sdk candidate: {error}\n")


if __name__ == "__main__":
    main()
