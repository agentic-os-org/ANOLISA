"""Validate the pinned Claude TLS entry against an isolated local model server."""

import argparse
import json
import os
from pathlib import Path
import resource
import socket

from sdk_transport_cases import one
from static_tls import PROFILES
from transport_cases import build


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, choices=(1, 2, 3), default=3)
    options = parser.parse_args()
    if {name for _, name in socket.if_nameindex()} != {"lo"} or os.geteuid() == 0:
        parser.error("run_as_nonroot_in_loopback_only_network_namespace")
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    os.umask(0o077)
    output = options.output.resolve()
    output.mkdir(mode=0o700)
    results = []
    for index in range(options.runs):
        case = output / f"run-{index + 1}"
        assets = build(case)
        result = one("claude", case, assets, Path(__file__), static_profile=next(iter(PROFILES)))
        senders = {p["pid"] for p in result["processes"] if p.get("model_sender")}
        verified = result["status"] == "OBSERVED" and any(
            source["pid"] in senders
            and source["server_wire_digest_matches"] > 0
            and source["summary"]["accounting_complete"]
            and source["summary"]["ring_full"] == 0
            for source in result.get("sources", [])
        )
        result["static_entry_receiver_verified"] = verified
        results.append(result)
        (output / "results.json").write_text(json.dumps(results, indent=2))
        print(json.dumps({"run": index + 1, "receiver_verified": verified}), flush=True)
        if not verified:
            raise SystemExit(1)


if __name__ == "__main__":
    main()
