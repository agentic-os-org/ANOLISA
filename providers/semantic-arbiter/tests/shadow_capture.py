#!/usr/bin/env python3
"""Transparent protocol capture and an observation-only native completion recorder.

The receipt Provider never judges, calls a model, or supplies tool effects. It
records real AW tool.after requests; it is not a substitute semantic Provider.
"""

import argparse
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Any
import uuid

CREDENTIAL_NAMES = ("QODER_PERSONAL_ACCESS_TOKEN", "DASHSCOPE_API_KEY")
MAX_BYTES = 16 * 1024 * 1024


def safe_path(path: Path) -> Path:
    """Reject links, ambiguous paths, and sensitive configuration filenames."""
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("An absolute non-symlink path is required")
    if any(p.name in {"credentials.env", "model.json"} for p in (path, *path.parents)):
        raise ValueError("Credential configuration files are outside harness scope")
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise ValueError("Symlink paths are outside harness scope")
    return path


def read_text(path: Path, limit: int = MAX_BYTES) -> str:
    """Read bounded regular artifacts, never devices or symlinks."""
    safe_path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("Expected a regular artifact")
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError("Artifact exceeds read limit")
    return data.decode("utf-8")


def write_text(path: Path, text: str) -> None:
    """Create private files exclusively: old benchmark artifacts are never replaced."""
    safe_path(path)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        stream.write(text)


def write_json(path: Path, value: Any) -> None:
    write_text(path, json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def redact(text: str, secrets: tuple[str, ...]) -> str:
    """Scrub supplied secrets before output, including common escaped encodings."""
    from urllib.parse import quote
    for secret in secrets:
        if secret:
            for form in (secret, quote(secret, safe=""), json.dumps(secret)[1:-1]):
                text = text.replace(form, "[REDACTED]")
    text = re.sub(r"(?i)(bearer\s+)[^\s\"']+", r"\1[REDACTED]", text)
    return text


def capture() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--provider-bin", type=Path)
    parser.add_argument("--receipt", action="store_true")
    args = parser.parse_args()
    if bool(args.provider_bin) == args.receipt:
        raise ValueError("Select exactly one capture role")
    directory = safe_path(args.directory)
    raw = sys.stdin.buffer.read(2 * 1024 * 1024 + 1)
    if len(raw) > 2 * 1024 * 1024:
        raise ValueError("Protocol input exceeds capture limit")
    request = json.loads(raw)
    method = request.get("method")
    if method not in {"describe", "validate_config", "invoke"}:
        raise ValueError("Unsupported protocol method")
    # AW validation is offline; only explicit harness --run permits invocation.
    if method == "invoke" and os.environ.get("SHADOW_ACCEPTANCE_RUN") != "1":
        raise ValueError("Runtime invocation requires explicit harness --run")
    secrets = tuple(os.environ.get(name, "") for name in CREDENTIAL_NAMES) if method == "invoke" else ()
    status = 0
    if args.receipt:
        response = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
        if method == "describe":
            response["operations"] = [{"name": "record_completion", "events": ["tool.after"], "effects": ["observe"]}]
        elif method == "invoke":
            if request.get("event", {}).get("name") != "tool.after":
                raise ValueError("Receipt recorder only accepts tool.after")
            response.update(input_digest=request["input_digest"], effects=[])
        out, err = (json.dumps(response) + "\n").encode(), b""
    else:
        binary = safe_path(args.provider_bin)
        process = subprocess.run([str(binary)], input=raw, capture_output=True, timeout=52)
        out, err, status = process.stdout, process.stderr, process.returncode
    # Preserve real output bytes on the protocol pipe, but redact all saved copies.
    # A semantic Provider failure is not replaced with a successful fabricated reply.
    if method == "invoke":
        try:
            decoded = json.loads(out)
        except (ValueError, UnicodeDecodeError):
            decoded = None
        artifact = {"role": "native_receipt" if args.receipt else "semantic_transport",
                    "request": request, "response": decoded, "returncode": status,
                    "stdout": out.decode(errors="replace"), "stderr": err.decode(errors="replace")}
        write_text(directory / (uuid.uuid4().hex + ".json"), redact(json.dumps(artifact) + "\n", secrets))
    sys.stdout.buffer.write(out)
    sys.stderr.write(redact(err.decode(errors="replace"), secrets))
    return status


if __name__ == "__main__":
    try:
        raise SystemExit(capture())
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        print("shadow capture failed; no synthetic semantic response supplied", file=sys.stderr)
        raise SystemExit(1)
