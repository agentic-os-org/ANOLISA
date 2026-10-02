"""Load the Hermes plugin under a collision-resistant test package name."""

from pathlib import Path

from standalone_hook_test_loader import load_package_from_path

_HERMES_PLUGIN_DIR = Path(__file__).resolve().parents[3] / "hermes-plugin"
load_package_from_path("hermes_plugin_src", _HERMES_PLUGIN_DIR / "src")
