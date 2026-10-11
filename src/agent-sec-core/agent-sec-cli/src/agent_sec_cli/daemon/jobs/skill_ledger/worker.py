"""Persistent child-process entry point for Skill Ledger processing."""

import os
import sys
import traceback
from typing import BinaryIO

from agent_sec_cli.daemon.jobs.skill_ledger.processor import (
    process_skill_change,
)
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import (
    MAX_WORKER_FRAME_BYTES,
    WorkerProtocolError,
    WorkerResultTooLargeError,
    error_worker_response,
    parse_worker_request,
    serialize_worker_response,
    success_worker_response,
)


def _main() -> int:
    # Keep the protocol pipe private from scanner and native-library output.
    protocol_fd = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    with os.fdopen(protocol_fd, "wb") as protocol_stdout:
        return _run(protocol_stdout)


def _run(protocol_stdout: BinaryIO) -> int:
    while True:
        line = sys.stdin.buffer.readline(MAX_WORKER_FRAME_BYTES + 1)
        if not line:
            return 0

        try:
            request = parse_worker_request(line)
        except WorkerProtocolError as exc:
            print(f"invalid Skill Ledger worker request: {exc}", file=sys.stderr)
            return 2

        try:
            result = process_skill_change(request.change)
            response = success_worker_response(request.request_id, result)
        except Exception as exc:
            traceback.print_exc(file=sys.stderr)
            response = error_worker_response(request.request_id, exc)

        try:
            frame = serialize_worker_response(response)
        except WorkerResultTooLargeError:
            # The result (e.g. a change echoing ~150k accumulated paths)
            # cannot be delivered within the frame cap. Dying here would read
            # as a transport failure: the client kills + respawns the worker
            # and reprocesses the same change (a full re-scan) before failing
            # identically. Answer with a bounded error frame instead — a
            # permanent payload error, no restart, no double processing.
            traceback.print_exc(file=sys.stderr)
            bounded = error_worker_response(
                request.request_id,
                WorkerResultTooLargeError(
                    f"worker result exceeds {MAX_WORKER_FRAME_BYTES} bytes"
                ),
            )
            frame = serialize_worker_response(bounded)
        except WorkerProtocolError as exc:
            print(f"invalid Skill Ledger worker response: {exc}", file=sys.stderr)
            return 2
        protocol_stdout.write(frame)
        protocol_stdout.flush()


if __name__ == "__main__":
    raise SystemExit(_main())
