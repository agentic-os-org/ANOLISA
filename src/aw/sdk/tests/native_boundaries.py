"""Native input and tool-before adoption checks for the configured plugin."""

import argparse
import json
from pathlib import Path

from native_app_config import ready, request


def verify(
    args: argparse.Namespace,
    root: Path,
    ports: list[int],
    payload: dict,
    data: bytes,
    preserved: dict,
    first_reply_seconds: float,
) -> None:
    """Verify admission ordering and denial against actual model/tool side effects."""
    assert b"SYNTHETIC_RAW_SECRET" in data, data.decode()
    if args.long:
        assert first_reply_seconds >= 34
        (root / "model-delay-seconds").unlink()
    requests = root / "model-requests.jsonl"
    models_before = requests.read_text().splitlines()
    marker = root / "work/must-not-exist"
    (root / "tool-call.json").write_text(
        json.dumps(
            {
                "name": "execute_shell_command",
                "arguments": {"command": f"touch {marker}", "timeout": 5},
            }
        )
    )
    (root / "tool-call-id.txt").write_text("denied")
    (root / "deny").touch()
    payload["session_id"] = "denied"
    denied = request(ports[0], "console/chat", payload)
    (args.output / "denied.sse").write_bytes(denied)
    assert not marker.exists() and ready(ports[0])
    if args.boundary == "input":
        assert requests.read_text().splitlines() == models_before
    else:
        assert len(requests.read_text().splitlines()) > len(models_before)
    (root / "deny").unlink()
    payload["session_id"] = "allowed"
    (root / "tool-call-id.txt").write_text("allowed")
    allowed = request(ports[0], "console/chat", payload)
    (args.output / "allowed.sse").write_bytes(allowed)
    assert marker.exists()
    assert all(path.read_bytes() == contents for path, contents in preserved.items())
    (args.output / "model-requests.jsonl").write_bytes(requests.read_bytes())
    names = ["configured-boundary", "denied-no-side-effect", "allowed-native-tool"]
    if args.long:
        names.append("long-model")
    (args.output / "results.json").write_text(
        json.dumps(
            {
                "passed": True,
                "boundary": args.boundary,
                "cases": [{"name": name, "passed": True} for name in names],
                "input_before_model": args.boundary == "input",
                "model_before_aw": args.boundary == "before",
            },
            indent=2,
        )
    )
