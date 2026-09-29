"""Dependency gates for shared lifecycle ownership across scan integrations."""

import tomllib
from pathlib import Path

V2 = Path(__file__).resolve().parents[2] / "v2"


def dependencies(relative):
    with (V2 / relative / "Cargo.toml").open("rb") as manifest:
        return tomllib.load(manifest).get("dependencies", {})


def test_handlers_and_core_cannot_depend_on_concrete_scanners_or_output_writers():
    for crate in ("crates/asc-daemon-handler", "crates/asc-daemon-core"):
        deps = dependencies(crate)
        assert not any(name.startswith("asc-capability-") for name in deps)
        assert not set(deps) & {
            "asc-event-sink",
            "asc-event-log",
            "asc-persistence-sqlite",
            "rusqlite",
        }
    for source in (V2 / "crates/asc-daemon-handler/src").glob("*.rs"):
        production = source.read_text().split("#[cfg(test)]")[0]
        assert "Finalizer" not in production
        assert "ActionRuntime" not in production


def test_capabilities_cannot_write_events_or_telemetry_directly():
    for crate in (V2 / "crates").glob("asc-capability-*"):
        if (crate / "Cargo.toml").is_file():
            deps = dependencies(crate.relative_to(V2))
            assert not set(deps) & {
                "asc-event-sink",
                "asc-event-log",
                "asc-telemetry",
                "asc-observability",
                "asc-persistence-sqlite",
                "rusqlite",
            }
