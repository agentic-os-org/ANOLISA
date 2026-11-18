"""Bounded, once-only local RPC with authenticated Linux peers."""

import asyncio
import errno
import json
import os
from pathlib import Path
import socket
import stat
import struct
import time
from typing import Any

SERVICE = "aw-service/v1alpha7"
SDK = "aw-sdk/v1"
MAX_FRAME = 9 * 1024 * 1024
UNAVAILABLE = {errno.ENOENT, errno.ECONNREFUSED, errno.ECONNRESET, errno.EPIPE, errno.ETIMEDOUT}


class AwError(RuntimeError):
    """A protocol/policy error, distinct from locally observed transport loss."""

    def __init__(self, code: str, *, unavailable: bool = False) -> None:
        super().__init__(code)
        self.code = code
        self.unavailable = unavailable


def same_identity(left: dict | None, right: dict | None) -> bool:
    return bool(left and right and left == right)


async def rpc(
    endpoint: str, identity: dict | None, operation: dict, timeout_ms: int
) -> dict[str, Any]:
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
        raise AwError("invalid_timeout")
    path = Path(endpoint)
    if not path.is_absolute():
        raise AwError("endpoint_must_be_absolute")
    writer = None
    deadline = time.monotonic_ns() + timeout_ms * 1_000_000
    try:
        async with asyncio.timeout(timeout_ms / 1000):
            parent = path.parent.lstat()
            metadata = path.lstat()
            if (
                not stat.S_ISDIR(parent.st_mode)
                or parent.st_uid != os.geteuid()
                or parent.st_mode & 0o077
                or not stat.S_ISSOCK(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or metadata.st_mode & 0o077
            ):
                raise AwError("endpoint_identity")
            reader, writer = await asyncio.open_unix_connection(endpoint, limit=MAX_FRAME + 4)
            peer = writer.get_extra_info("socket").getsockopt(
                socket.SOL_SOCKET, socket.SO_PEERCRED, 12
            )
            if struct.unpack("3i", peer)[1] != os.geteuid():
                raise AwError("endpoint_identity")
            request = json.dumps(
                {
                    "api_version": SERVICE,
                    "identity": identity,
                    "deadline_ns": deadline,
                    "operation": operation,
                },
                allow_nan=False,
            ).encode()
            if len(request) > MAX_FRAME:
                raise AwError("frame_limit")
            writer.write(struct.pack("!I", len(request)) + request)
            await writer.drain()
            (length,) = struct.unpack("!I", await reader.readexactly(4))
            if not 0 < length <= MAX_FRAME:
                raise AwError("frame_limit")
            try:
                response = json.loads(await reader.readexactly(length))
            except (ValueError, UnicodeError) as error:
                raise AwError("invalid_response") from error
            if not isinstance(response, dict) or response.get("api_version") != SERVICE:
                raise AwError("protocol_version")
            received = response.get("identity")
            if not isinstance(received, dict) or not all(
                received.get(key) for key in ("generation", "config_revision")
            ):
                raise AwError("invalid_response")
            if identity is not None and not same_identity(identity, received):
                raise AwError("ECONNRESET", unavailable=True)
            if response.get("error") is not None:
                if not isinstance(response["error"], str) or response.get("result") is not None:
                    raise AwError("invalid_response")
                raise AwError(response["error"])
            if not isinstance(response.get("result"), dict):
                raise AwError("invalid_response")
            if time.monotonic_ns() >= deadline:
                raise AwError("ETIMEDOUT", unavailable=True)
            return response
    except TimeoutError as error:
        raise AwError("ETIMEDOUT", unavailable=True) from error
    except asyncio.IncompleteReadError as error:
        raise AwError("ECONNRESET", unavailable=True) from error
    except OSError as error:
        raise AwError(
            errno.errorcode.get(error.errno, "transport_error"),
            unavailable=error.errno in UNAVAILABLE,
        ) from error
    finally:
        if writer is not None:
            # No further payload is written, and cancellation never waits on the peer.
            writer.close()
