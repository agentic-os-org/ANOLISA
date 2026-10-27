"""Render service assets for explicit install paths without editing RPM defaults."""

import argparse
import shlex
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bindir", required=True, help="Binary directory used by service units")
    worker = parser.add_mutually_exclusive_group()
    worker.add_argument("--binary", default="agentsight", help="Supervisor worker command")
    worker.add_argument(
        "--sibling-binary",
        action="store_true",
        help="Resolve the raw supervisor's worker beside the installed launcher",
    )
    parser.add_argument("--launcher", required=True, type=Path)
    parser.add_argument("--unitdir", required=True, type=Path)
    parser.add_argument(
        "--config", help="Explicit worker configuration file for source installation"
    )
    arguments = parser.parse_args()
    bindir = arguments.bindir.rstrip("/")
    if not arguments.bindir.startswith("/") and bindir != "{bindir}":
        parser.error("--bindir must be an absolute directory or the literal {bindir}")
    if arguments.config is not None and not Path(arguments.config).is_absolute():
        parser.error("--config must be an absolute file path")
    command = (
        '"$(dirname -- "$0")/agentsight"'
        if arguments.sibling_binary
        else shlex.quote(arguments.binary or "agentsight")
    )
    replacements = {
        "@BINDIR@": bindir,
        "@AGENTSIGHT_COMMAND@": command,
        "@CONFIG_ARGUMENT@": (
            " --config " + shlex.quote(arguments.config) if arguments.config is not None else ""
        ),
    }
    templates = Path(__file__).resolve().parent
    outputs = (
        ("agentsight-start.sh.in", arguments.launcher, 0o755),
        ("agentsight.service.in", arguments.unitdir / "agentsight.service", 0o644),
        (
            "agentsight-enforcer.service.in",
            arguments.unitdir / "agentsight-enforcer.service",
            0o644,
        ),
    )
    rendered = []
    for name, destination, mode in outputs:
        text = (templates / name).read_text()
        for placeholder, value in replacements.items():
            text = text.replace(placeholder, value)
        rendered.append((destination, mode, text))
    for destination, mode, text in rendered:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)
        destination.chmod(mode)


if __name__ == "__main__":
    main()
