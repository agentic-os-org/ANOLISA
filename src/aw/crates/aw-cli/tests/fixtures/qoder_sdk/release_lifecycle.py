#!/usr/bin/env python3
"""Pin published SDK/Worker bytes and locate the post-Hook result rewrite."""

import argparse
import hashlib
import json
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--sdk-version", required=True)
    parser.add_argument("--worker-version", required=True)
    parser.add_argument("--sdk-sha256", required=True)
    parser.add_argument("--worker-target", choices=["linux-arm64", "linux-x64"], required=True)
    parser.add_argument("--worker-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    sdk = args.package / "dist/index.js"
    worker = args.package / "dist/_worker/qoder-worker-runtime.obf.mjs"
    metadata = args.package / "dist/_worker/runtime-info.json"
    assert json.loads((args.package / "package.json").read_text())["version"] == args.sdk_version
    runtime_info = json.loads(metadata.read_text())
    assert runtime_info["version"] == args.worker_version
    sdk_digest = hashlib.sha256(sdk.read_bytes()).hexdigest()
    worker_bytes = worker.read_bytes()
    worker_digest = hashlib.sha256(worker_bytes).hexdigest()
    assert sdk_digest == args.sdk_sha256
    assert runtime_info["target"] == args.worker_target
    assert worker_digest == args.worker_sha256
    post = worker_bytes.find(b"firePostToolUseEvent")
    updated = worker_bytes.find(b"getUpdatedToolOutput", post)
    success = worker_bytes.find(b"createSuccessResult", updated)
    assert 0 <= post < updated < success

    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "result.json").write_text(json.dumps({
        "status": "partial_unsupported",
        "sdk_version": args.sdk_version,
        "worker_version": args.worker_version,
        "sdk_sha256": sdk_digest,
        "worker_sha256": worker_digest,
        "worker_target": runtime_info["target"],
        "post_precedes_worker_rewrite": True,
        "model_calls": 0,
        "final_gate": "unsupported",
        "reason": "No captured model-visible Worker request or final sink",
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
