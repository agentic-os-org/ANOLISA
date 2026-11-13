"""Ask the Gateway to retire one session after the native CLI has exited."""

import json
from pathlib import Path
import sys

from hermes_cli.gateway_connection import GatewayConnection


def main() -> None:
    launch_path = Path(sys.argv[1])
    session_id = sys.argv[2]
    launch = json.loads(launch_path.read_text())
    connection = GatewayConnection()
    try:
        receipt = connection.call(
            "plugin.aw-native-hooks.retire", timeout=5,
            target_session_id=session_id, launch_path=str(launch_path),
            token=launch["token"])
    finally:
        connection.close()
    if receipt != {"retired": True}:
        raise SystemExit("AW Hermes Gateway session retirement is pending")


if __name__ == "__main__":
    main()
