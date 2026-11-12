"""Verify the canonical execution copy against the controller's private source snapshot."""

import hashlib
import json
import os
import posixpath
import re
import shlex
import stat
import sys
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SUBJECT_ROOT = "/skills/subject"
# These are the existing Rust Snapshot limits and source-tree-v2 digest encoding.
FILE_LIMIT = 32 * 1024 * 1024
TREE_LIMIT = 128 * 1024 * 1024
ENTRY_LIMIT = 8192
DEPTH_LIMIT = 64


@dataclass
class SubjectInstallation:
    """Keep controller-generated identity separate from the public verification receipt."""

    binding: dict[str, Any]
    manifest: dict[str, Any]


def _directory(path: Path) -> int:
    descriptor = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            if part in {".", ".."}:
                raise ValueError("source directory is not canonical")
            child = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=descriptor,
            )
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def source_identity(source: Path) -> dict[str, Any]:
    """Hash regular files and executable bits without following links or unbounded trees."""
    source = source.absolute()
    digest = hashlib.sha256(b"agent-sec-skill-score/source-tree-v2\0")
    members: list[dict[str, Any]] = []
    consumed = 0

    def visit(directory: int, parent: str, depth: int) -> None:
        nonlocal consumed
        before_directory = os.fstat(directory)
        names = []
        with os.scandir(directory) as entries:
            for entry in entries:
                names.append(entry.name)
                if len(names) + len(members) > ENTRY_LIMIT:
                    raise ValueError("source entry limit exceeded")
        for name in sorted(names):
            relative = f"{parent}/{name}" if parent else name
            if depth > DEPTH_LIMIT or len(members) >= ENTRY_LIMIT:
                raise ValueError("source entry/depth limit exceeded")
            descriptor = os.open(
                name,
                os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                dir_fd=directory,
            )
            try:
                before = os.fstat(descriptor)
                is_directory = stat.S_ISDIR(before.st_mode)
                if not is_directory and (
                    not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
                ):
                    raise ValueError("source contains a link or special file")
                mode = (before.st_mode & 0o111) | (0o100 if is_directory else 0)
                kind = "d" if is_directory else "f"
                label = relative.encode("utf-8", errors="strict")
                digest.update(len(label).to_bytes(8, "big"))
                digest.update(label)
                digest.update(kind.encode())
                digest.update(mode.to_bytes(4, "big"))
                member: dict[str, Any] = {
                    "path": relative,
                    "kind": kind,
                    "exec": mode,
                }
                members.append(member)
                if is_directory:
                    visit(descriptor, relative, depth + 1)
                else:
                    if (
                        before.st_size > FILE_LIMIT
                        or consumed + before.st_size > TREE_LIMIT
                    ):
                        raise ValueError("source byte limit exceeded")
                    digest.update(before.st_size.to_bytes(8, "big"))
                    file_digest = hashlib.sha256()
                    size = 0
                    while chunk := os.read(
                        descriptor, min(65536, FILE_LIMIT + 1 - size)
                    ):
                        size += len(chunk)
                        if size > FILE_LIMIT:
                            raise ValueError("source file limit exceeded")
                        digest.update(chunk)
                        file_digest.update(chunk)
                    consumed += size
                    member.update(size=size, sha256=file_digest.hexdigest())
                after = os.fstat(descriptor)
                if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                    after.st_size,
                    after.st_mtime_ns,
                    after.st_ctime_ns,
                ) or (not is_directory and size != before.st_size):
                    raise ValueError("source changed while hashing")
            finally:
                os.close(descriptor)
        after_directory = os.fstat(directory)
        if (before_directory.st_mtime_ns, before_directory.st_ctime_ns) != (
            after_directory.st_mtime_ns,
            after_directory.st_ctime_ns,
        ):
            raise ValueError("source directory changed while hashing")

    descriptor = _directory(source)
    try:
        visit(descriptor, "", 1)
    finally:
        os.close(descriptor)
    if not any(
        member["path"] == "SKILL.md" and member["kind"] == "f" for member in members
    ):
        raise ValueError("source SKILL.md is missing")
    return {
        "schema": "agent-sec-skill-score/source-binding-v1",
        "root": SUBJECT_ROOT,
        "content_hash": digest.hexdigest(),
        "members": members,
    }


def verify_source(
    source: Path, expected: dict[str, Any], *, exact: bool = True
) -> dict[str, Any]:
    """Require exact uploaded paths, bytes and executable bits before starting the Agent."""
    actual = source_identity(source)
    actual_members = {member["path"]: member for member in actual["members"]}
    if (exact and actual != expected) or any(
        actual_members.get(member["path"]) != member for member in expected["members"]
    ):
        raise ValueError("uploaded source does not match the private snapshot")
    encoded = json.dumps(expected, sort_keys=True, separators=(",", ":")).encode()
    original_paths = {member["path"] for member in expected["members"]}
    added = [
        member["path"]
        for member in actual["members"]
        if member["path"] not in original_paths
    ]
    return {
        "schema": expected["schema"],
        "root": SUBJECT_ROOT,
        "content_hash": expected["content_hash"],
        "manifest_sha256": hashlib.sha256(encoded).hexdigest(),
        "entries": len(expected["members"]),
        "verified": True,
        "exact_tree": exact,
        "observed_tree_hash": actual["content_hash"],
        "additional_entries": len(added),
        "additional_paths": added[:256],
        "additional_paths_truncated": len(added) > 256,
    }


async def install_subject(environment: Any, source: Path) -> SubjectInstallation:
    """Use Harbor's public environment API; never infer a source path from Agent output."""
    from agent_sec_cli.skill_score.contracts import (  # noqa: PLC0415 - standalone verifier
        ContractError,
    )

    expected = source_identity(source)
    scratch = (
        "/tmp/asc-score-subject-" + uuid.uuid4().hex
    )  # noqa: S108 - disposable container
    prepare = "\n".join(
        [
            "import os",
            "root = os.open('/', os.O_RDONLY | os.O_DIRECTORY)",
            "try: os.mkdir('skills', 0o755, dir_fd=root)",
            "except FileExistsError: pass",
            "skills = os.open('skills', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root)",
            "os.mkdir('subject', 0o700, dir_fd=skills)",
            "tmp = os.open('tmp', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root)",
            f"os.mkdir({Path(scratch).name!r}, 0o700, dir_fd=tmp)",
        ]
    )
    result = await environment.exec("python -c " + shlex.quote(prepare), timeout_sec=30)
    if result.return_code != 0:
        raise ContractError("canonical source directory initialization failed")
    with tempfile.TemporaryDirectory(prefix="asc-score-source-manifest-") as temporary:
        manifest = Path(temporary) / "manifest.json"
        manifest.write_text(json.dumps(expected))
        await environment.upload_file(Path(__file__), scratch + "/verify.py")
        await environment.upload_file(manifest, scratch + "/manifest.json")
        await environment.upload_dir(source, SUBJECT_ROOT)
        result = await environment.exec(
            f"python {scratch}/verify.py {scratch}/manifest.json",
            timeout_sec=40,
        )
    if result.return_code != 0:
        raise ContractError("canonical source verification failed")
    try:
        binding = json.loads(result.stdout)
        if binding != verify_source(source, expected):
            raise ValueError("identity mismatch")
    except (ValueError, TypeError) as exc:
        raise ContractError("canonical source verification receipt is invalid") from exc
    return SubjectInstallation(binding, expected)


async def recheck_subject(
    environment: Any, installed: SubjectInstallation
) -> dict[str, Any]:
    """Re-upload trusted verifier inputs after the Agent; check all original members."""
    from agent_sec_cli.skill_score.contracts import (  # noqa: PLC0415 - standalone verifier
        ContractError,
    )

    scratch = "/tmp/asc-score-source-check-" + uuid.uuid4().hex  # noqa: S108
    prepare = (
        "import os; "
        "root=os.open('/',os.O_RDONLY|os.O_DIRECTORY); "
        "tmp=os.open('tmp',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=root); "
        f"os.mkdir({Path(scratch).name!r},0o700,dir_fd=tmp)"
    )
    result = await environment.exec("python -c " + shlex.quote(prepare), timeout_sec=30)
    if result.return_code != 0:
        raise ContractError("canonical source recheck initialization failed")
    with tempfile.TemporaryDirectory(prefix="asc-score-source-recheck-") as temporary:
        manifest = Path(temporary) / "manifest.json"
        manifest.write_text(json.dumps(installed.manifest))
        await environment.upload_file(Path(__file__), scratch + "/verify.py")
        await environment.upload_file(manifest, scratch + "/manifest.json")
        result = await environment.exec(
            f"python {scratch}/verify.py {scratch}/manifest.json --after",
            timeout_sec=40,
        )
    if result.return_code != 0:
        raise ContractError("canonical source was modified or could not be verified")
    try:
        binding = json.loads(result.stdout)
        for field in (
            "schema",
            "root",
            "content_hash",
            "manifest_sha256",
            "entries",
        ):
            if binding[field] != installed.binding[field]:
                raise ValueError("identity mismatch")
        if binding["verified"] is not True or binding["exact_tree"] is not False:
            raise ValueError("verification missing")
    except (ValueError, TypeError, KeyError) as exc:
        raise ContractError("canonical source recheck receipt is invalid") from exc
    return binding


def source_write_refs(
    events: list[dict[str, Any]], manifest: dict[str, Any]
) -> list[str]:
    """Flag original-member writes and unresolved aliases known to involve the source tree."""
    original = [SUBJECT_ROOT + "/" + member["path"] for member in manifest["members"]]
    aliases = []
    for event in events:
        if (
            event.get("syscall") in {"symlink", "symlinkat"}
            and event.get("outcome") == "succeeded"
        ):
            first = re.search(r'"(?:[^"\\]|\\.)*"', event.get("args", ""))
            try:
                origin = json.loads(first[0]) if first else ""
            except ValueError:
                continue
            if origin and (
                origin == SUBJECT_ROOT
                or origin.startswith(SUBJECT_ROOT + "/")
                or SUBJECT_ROOT.startswith(origin.rstrip("/") + "/")
            ):
                aliases.append(event.get("target", ""))
    refs = []
    observed_cwd = {}
    for event in sorted(events, key=lambda item: item.get("time", 0)):
        pid = event.get("pid")
        cwd = observed_cwd.get(pid)
        if event.get("syscall") == "chdir" and event.get("outcome") == "succeeded":
            target = event.get("target", "")
            observed_cwd[pid] = (
                target
                if target.startswith("/")
                else posixpath.join(cwd, target) if cwd else None
            )
        if not (
            event.get("kind") == "file"
            and event.get("write")
            and event.get("outcome") == "succeeded"
        ):
            continue
        paths = [event.get("target", "")]
        source_write = False
        resolved_open = False
        syscall = event.get("syscall", "")
        if (
            cwd
            and paths[0]
            and not paths[0].startswith("/")
            and syscall in {"rename", "link"}
        ):
            paths[0] = posixpath.join(cwd, paths[0])
        if syscall in {"open", "openat", "openat2"}:
            # strace -yy resolves a successful open's fd even through a symlink alias.
            resolved = re.fullmatch(r"\d+<(/[^>]+)>", event.get("result", ""))
            if resolved:
                paths = [resolved[1]]
                resolved_open = True
        if syscall in {"rename", "renameat", "renameat2", "link", "linkat"}:
            # The collector's target is the destination; moving an original away also matters.
            arguments = event.get("args", "")
            first = re.search(r'"(?:[^"\\]|\\.)*"', arguments)
            try:
                source = json.loads(first[0]) if first else ""
            except ValueError:
                source = ""
            if (
                source
                and not source.startswith("/")
                and syscall.endswith(("at", "at2"))
            ):
                directory = re.match(r"(?:AT_FDCWD|\d+)<(/[^>]+)>,", arguments)
                if directory:
                    source = posixpath.join(directory[1], source)
                elif cwd and arguments.startswith("AT_FDCWD,"):
                    source = posixpath.join(cwd, source)
                else:
                    source = ""
            elif cwd and source and not source.startswith("/"):
                source = posixpath.join(cwd, source)
            if (
                cwd
                and paths[0]
                and not paths[0].startswith("/")
                and syscall.endswith(("at", "at2"))
            ):
                names = list(re.finditer(r'"(?:[^"\\]|\\.)*"', arguments))
                if len(names) >= 2 and re.search(
                    r",\s*AT_FDCWD,\s*$", arguments[: names[1].start()]
                ):
                    paths[0] = posixpath.join(cwd, paths[0])
            paths.append(source)
        for path in paths:
            if not isinstance(path, str) or not path.startswith("/"):
                continue
            if not resolved_open and any(
                path.startswith(alias.rstrip("/") + "/") for alias in aliases if alias
            ):
                source_write = True
            if (
                not resolved_open
                and (event.get("path_resolution") or ".." in Path(path).parts)
                and (path == SUBJECT_ROOT or path.startswith(SUBJECT_ROOT + "/"))
            ):
                source_write = True
            path = posixpath.normpath(path)
            if any(
                member == path or member.startswith(path.rstrip("/") + "/")
                for member in original
            ):
                source_write = True
        if source_write:
            refs.append(event["id"])
    return refs


def invalidate_source_attribution(evidence: dict[str, Any]) -> None:
    """Keep observations while withholding claims that effects came from the original source."""
    evidence["complete"] = False
    evidence["unverified_entry_path_started"] = evidence.get("entry_started", False)
    evidence["entry_started"] = False
    evidence["entry_activated"] = False
    evidence["critical_canary"] = False
    for key in (
        "positive_refs",
        "attempt_refs",
        "critical_refs",
        "matched_finding_ids",
    ):
        evidence[key] = []
    for event in evidence.get("events", []):
        event["attributed"] = False
        if event.get("id") in evidence.get("catalog", {}):
            evidence["catalog"][event["id"]] = json.dumps(event, ensure_ascii=False)
    evidence["unverified_reason"] = (
        "canonical source changed or its post-execution identity is unverified"
    )


if __name__ == "__main__":
    try:
        expected_manifest = json.loads(Path(sys.argv[1]).read_text())
        print(
            json.dumps(
                verify_source(
                    Path(SUBJECT_ROOT),
                    expected_manifest,
                    exact="--after" not in sys.argv[2:],
                )
            )
        )
    except Exception:
        print("canonical source verification failed", file=sys.stderr)
        raise SystemExit(1) from None
