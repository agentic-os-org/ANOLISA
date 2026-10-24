"""Report the container's AgentSight HTTP availability as bounded JSON evidence."""

import http.client
import json
import os
import signal
import sys


def check(required: str, port: str) -> tuple[dict[str, object], int]:
    """Probe loopback directly; optional outages remain visible as degraded."""
    if required not in ("0", "1") or len(port) > 5 or not port.isascii() or not port.isdecimal():
        return {"status": "blocked", "reason": "invalid healthcheck configuration"}, 1
    number = int(port)
    if not 1 <= number <= 65535:
        return {"status": "blocked", "reason": "invalid AgentSight port"}, 1
    result: dict[str, object] = {
        "schema_version": 1,
        "probe": "agentsight_http",
        "required": required == "1",
        "status": "ready",
    }
    connection = http.client.HTTPConnection("127.0.0.1", number, timeout=2)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        if response.status != 200:
            raise ValueError(f"HTTP {response.status}")
        # Bound both allocation and parsing of an unexpected endpoint response.
        payload = response.read(65537)
        if len(payload) > 65536:
            raise ValueError("health response exceeds 64 KiB")
        data = json.loads(payload)
        if not isinstance(data, dict) or data.get("status") != "ok":
            raise ValueError("health response must contain status=ok")
        result["version"] = data.get("version")
    except (OSError, http.client.HTTPException, ValueError) as error:
        result.update(status="degraded", reason=str(error))
    finally:
        connection.close()
    return result, int(required == "1" and result["status"] != "ready")


def main() -> int:
    """Print one health record and return the configured Docker health status."""

    def deadline(_signum: int, _frame: object) -> None:
        raise TimeoutError("health probe exceeded four seconds")

    signal.signal(signal.SIGALRM, deadline)
    signal.setitimer(signal.ITIMER_REAL, 4)
    try:
        result, code = check(
            os.environ.get("ANOLISA_REQUIRE_AGENTSIGHT", "0"),
            os.environ.get("ANOLISA_AGENTSIGHT_PORT", "7396"),
        )
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    print(json.dumps(result, sort_keys=True))
    return code


if __name__ == "__main__":
    sys.exit(main())
