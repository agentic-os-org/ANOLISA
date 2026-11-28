"""One command: authorized TLS writes, real AW ownership, metadata and cleanup."""

import argparse
import json
import os
from pathlib import Path
import resource

from aw_fixture import AwFixture
from run_case import prepare, run_case, wait_for


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args()
    directory = options.output.resolve()
    if directory.exists():
        parser.error("output must be a new directory")
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    assets = prepare(directory)
    daemon = AwFixture(options.aw.resolve(), directory / "aw-daemon")
    try:
        result = run_case(directory, assets, "registered", threads=4, aw=daemon.aw, socket=daemon.socket)
        wait_for(lambda: daemon.status()["bindings"] == 0)
        unknown = run_case(directory, assets, "unregistered")
        result = {"registered": result, "unregistered": unknown,
                  "aw_bindings_after_owner_exit": daemon.status()["bindings"],
                  "production_rewrite": "no-go: tracer SIGKILL can terminate target with SIGTRAP"}
        (directory / "demo.json").write_text(json.dumps(result, indent=2))
        print(json.dumps({"registered_events": result["registered"]["events"],
                          "association": result["registered"]["association"],
                          "unregistered_events": result["unregistered"]["events"],
                          "aw_bindings_after_owner_exit": result["aw_bindings_after_owner_exit"],
                          "output": str(directory)}, indent=2))
    finally:
        daemon.close()


if __name__ == "__main__":
    main()
