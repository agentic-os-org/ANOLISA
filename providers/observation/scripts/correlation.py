"""Read-only fixture importer, separate from the Sidecar and without AW RPC access."""

import copy
import json
import os
from pathlib import Path
import time


def process_ref(pid):
    base = Path(f"/proc/{pid}")
    fields = (base / "stat").read_text().rsplit(") ", 1)[1].split()
    if fields[0] in ("Z", "X"):
        raise ValueError("process_exited")
    executable = (base / "exe").stat()
    return {"pid": pid, "start_ticks": fields[19], "uid": base.stat().st_uid,
            "pid_namespace": os.readlink(base / "ns/pid"),
            "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
            "exe": os.readlink(base / "exe"), "exe_device": executable.st_dev,
            "exe_inode": executable.st_ino}


def import_registration(path, process):
    """Trust only the test owner's private capture of actual CLI replies, for 60 seconds.

    This file is a same-UID fixture trust boundary, not a cryptographic AW attestation.
    Production use needs a scoped, read-only identity export and revocation contract.
    """
    path = Path(path)
    stat = path.lstat()
    if not path.is_file() or path.is_symlink() or stat.st_uid != os.geteuid() or stat.st_mode & 0o077 or stat.st_size > 65536:
        raise ValueError("invalid_registration_capture")
    source = json.loads(path.read_text())
    if source["schema_version"] != "aw.ptrace-fixture-registration/v0":
        raise ValueError("unsupported_registration_capture")
    if process_ref(process["pid"]) != process:
        raise ValueError("stale_process_identity")
    if source["owner"] != {"pid": process["pid"], "start_ticks": process["start_ticks"]}:
        raise ValueError("fixture_owner_mismatch")
    entries = []
    if len(source["registrations"]) > 16:
        raise ValueError("registration_count_limit")
    for record in source["registrations"]:
        prepared = record["registration"]
        ready = record["ready"]
        scope = record["scope"]
        handle = prepared["result"]["handle"]
        root_id = handle["binding"]["instance_id"]
        scopes = {item["instance_id"]: item for item in source["status"]["instances"]}
        if (prepared["error"] is not None or ready["error"] is not None
                or ready["result"]["state"] != "ready" or ready["result"]["handle"] != handle
                or handle["key"]["owner"] != source["owner"]
                or handle["binding"]["identity"] != source["identity"]
                or scope["identity"] != source["identity"]
                or scope["target"] != handle["binding"]["target"]
                or scopes.get(scope["instance_id"], {}).get("parent_instance_id") != root_id
                or not scopes.get(root_id, {}).get("scope_validity", {}).get("native_alive")
                or not scopes.get(scope["instance_id"], {}).get("scope_validity", {}).get("native_alive")
                or not record["acknowledged"]["result"]["acknowledged"]):
            raise ValueError("invalid_registration_relationship")
        entries.append({"aw_instance_id": handle["binding"]["instance_id"], "aw_scope_id": scope["instance_id"],
                        "service_identity": source["identity"], "process": process,
                        "valid_from_ns": source["valid_from_ns"],
                        "valid_until_ns": str(int(source["valid_from_ns"]) + 60_000_000_000)})
    return entries


def correlate(event, mappings, current=None):
    result = copy.deepcopy(event)
    result["aw_instance_id"] = result["aw_scope_id"] = None
    result["association"] = {"status": "unassociated", "process_state": "observed"}
    if event.get("schema_version") != "aw.transport-observation-poc/v0":
        raise ValueError("unsupported_event_schema")
    try:
        current = current or process_ref(event["process"]["pid"])
        if current != event["process"]:
            raise ValueError("stale")
    except (OSError, ValueError):
        result["association"]["status"] = "stale_process_identity"
        return result
    observed = int(event["observed_at_ns"])
    now = time.monotonic_ns()
    matches = [m for m in mappings if m["process"] == current
               and int(m["valid_from_ns"]) <= observed <= now < int(m["valid_until_ns"])]
    if len(matches) == 1:
        result.update(aw_instance_id=matches[0]["aw_instance_id"], aw_scope_id=matches[0]["aw_scope_id"])
        result["association"] = {"status": "verified_fixture_owner", "process_state": "observed",
                                 "basis": "private_aw_cli_fixture_capture", "service_identity": matches[0]["service_identity"]}
    elif matches:
        result["association"]["status"] = "ambiguous_process_owner"
    return result
