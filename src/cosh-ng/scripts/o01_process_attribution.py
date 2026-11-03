from __future__ import annotations

import copy
import os
import stat
from pathlib import Path, PurePosixPath
from typing import Any

SHELL_EXE = "/usr/libexec/anolisa/cosh-ng/cosh-shell"
PROBE_EXE = "/usr/bin/python3.11"
PROBE_ARGV = ("/usr/bin/python3", "/usr/bin/dnf", "check-update", "cosh-ng")
IDENTITY_FIELDS = ("pid", "start_ticks", "cgroup", "cgroup_device", "cgroup_inode")


def _positive_integer(value: object) -> bool:
    return type(value) is int and value > 0


def _cgroup_path(value: str) -> str:
    if not value.startswith("0::/") or len(value.splitlines()) != 1:
        raise ValueError("a single cgroup-v2 membership is required")
    path = value[3:]
    if ".." in PurePosixPath(path).parts or str(PurePosixPath(path)) != path:
        raise ValueError("noncanonical cgroup path")
    return path


def _read_state(directory: Path) -> dict[str, Any]:
    prefix, fields = (directory / "stat").read_text().rsplit(")", 1)
    pid, name = prefix.split("(", 1)
    values = fields.split()
    result = {
        "pid": int(pid.strip()),
        "name": name,
        "ppid": int(values[1]),
        "start_ticks": int(values[19]),
        "cgroup": (directory / "cgroup").read_bytes().decode("utf-8").removesuffix("\n"),
    }
    if result["pid"] != int(directory.name) or not _positive_integer(result["start_ticks"]):
        raise ValueError("invalid process identity")
    _cgroup_path(result["cgroup"])
    return result


def _group_identity(path: Path) -> dict[str, int]:
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode):
        raise ValueError("cgroup is not a directory")
    return {"cgroup_device": metadata.st_dev, "cgroup_inode": metadata.st_ino}


def _command_identity(directory: Path) -> dict[str, Any]:
    executable = os.readlink(directory / "exe")
    command_line = (directory / "cmdline").read_bytes()
    if not command_line.endswith(b"\0") or command_line == b"\0":
        raise ValueError("incomplete command line")
    return {
        "exe": executable,
        "argv": [value.decode("utf-8") for value in command_line[:-1].split(b"\0")],
    }


def observe_process(
    pid: int, *, proc_root: Path = Path("/proc"), cgroup_root: Path = Path("/sys/fs/cgroup")
) -> dict[str, Any]:
    result: dict[str, Any] = {"stable": False}
    try:
        if not _positive_integer(pid):
            raise ValueError("invalid PID")
        directory = proc_root / str(pid)
        before = _read_state(directory)
        result["before"] = before
        group = cgroup_root / _cgroup_path(before["cgroup"]).lstrip("/")
        group_before = _group_identity(group)
        result["cgroup_identity_before"] = group_before
        command_before = _command_identity(directory)
        result.update(command_before=command_before, **command_before)
        command_after = _command_identity(directory)
        after = _read_state(directory)
        group_after = _group_identity(group)
        result.update(
            after=after,
            command_after=command_after,
            cgroup_identity_after=group_after,
            **group_before,
            stable=(
                before == after and group_before == group_after and command_before == command_after
            ),
        )
    except (OSError, ValueError, IndexError) as error:
        result["error"] = str(error)
    return result


def _stable(proof: dict[str, Any]) -> bool:
    before = proof.get("before")
    after = proof.get("after")
    group = {key: proof.get(key) for key in ("cgroup_device", "cgroup_inode")}
    return bool(
        proof.get("stable") is True
        and isinstance(before, dict)
        and before == after
        and all(_positive_integer(before.get(key)) for key in ("pid", "start_ticks", "ppid"))
        and all(_positive_integer(value) for value in group.values())
        and proof.get("cgroup_identity_before") == group == proof.get("cgroup_identity_after")
        and proof.get("command_before")
        == {"exe": proof.get("exe"), "argv": proof.get("argv")}
        == proof.get("command_after")
        and isinstance(proof.get("argv"), list)
        and isinstance(proof.get("exe"), str)
    )


def _identity(proof: dict[str, Any]) -> dict[str, Any]:
    return {
        **{key: proof["before"][key] for key in ("pid", "start_ticks", "cgroup")},
        **{key: proof[key] for key in ("cgroup_device", "cgroup_inode")},
    }


def _candidate_root(proof: dict[str, Any]) -> bool:
    return _stable(proof) and proof["exe"] == SHELL_EXE and proof["argv"] == ["-cosh"]


def capture_held_root(
    pid: int,
    cgroup_path: Path,
    *,
    proc_root: Path = Path("/proc"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
) -> dict[str, Any] | None:
    try:
        relative = cgroup_path.relative_to(cgroup_root)
        if str(relative) == "." or ".." in relative.parts:
            return None
        expected_group = "0::/" + relative.as_posix()
        proof = observe_process(pid, proc_root=proc_root, cgroup_root=cgroup_root)
        if not _candidate_root(proof) or proof["before"]["cgroup"] != expected_group:
            return None
        return _identity(proof)
    except (OSError, ValueError):
        return None


def attribute_busy_processes(
    busy_processes: list[dict[str, Any]],
    held_roots: list[dict[str, Any]],
    *,
    proc_root: Path = Path("/proc"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
) -> dict[str, Any]:
    observations = []
    blocking_processes = []
    for process in busy_processes:
        row: dict[str, Any] = {
            "process": copy.deepcopy(process),
            "classification": "foreign-or-unknown",
            "blocking": True,
            "evidence": {},
        }
        if process.get("name") == "dnf" and all(
            _positive_integer(process.get(key)) for key in ("pid", "start_ticks")
        ):
            child = observe_process(process["pid"], proc_root=proc_root, cgroup_root=cgroup_root)
            row["evidence"]["child"] = child
            exact_probe = (
                _stable(child)
                and child["before"]["start_ticks"] == process["start_ticks"]
                and child["before"]["name"] == "dnf"
                and child["exe"] == PROBE_EXE
                and child["argv"] == list(PROBE_ARGV)
            )
            for expected in held_roots if exact_probe else []:
                if not isinstance(expected, dict) or not all(
                    key in expected for key in IDENTITY_FIELDS
                ):
                    continue
                if (
                    not all(
                        _positive_integer(expected[key])
                        for key in IDENTITY_FIELDS
                        if key != "cgroup"
                    )
                    or expected["pid"] != child["before"]["ppid"]
                ):
                    continue
                root = observe_process(
                    expected["pid"], proc_root=proc_root, cgroup_root=cgroup_root
                )
                row["evidence"].update(root=root, held_identity=copy.deepcopy(expected))
                if not _candidate_root(root) or _identity(root) != expected:
                    continue
                if (
                    child["before"]["ppid"] == expected["pid"]
                    and child["before"]["cgroup"] == expected["cgroup"]
                    and child["cgroup_device"] == expected["cgroup_device"]
                    and child["cgroup_inode"] == expected["cgroup_inode"]
                ):
                    row.update(classification="candidate-update-probe", blocking=False)
                    row["evidence"].update(root=root, held_identity=copy.deepcopy(expected))
                    break
        observations.append(row)
        if row["blocking"]:
            blocking_processes.append(copy.deepcopy(process))
    return {"observations": observations, "blocking_processes": blocking_processes}
