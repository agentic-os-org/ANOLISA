"""Tests for isolated loading of standalone plugin hook modules."""

import sys
from types import ModuleType

from standalone_hook_test_loader import (
    load_package_from_path,
    load_standalone_hook,
)


def test_load_standalone_hook_isolates_all_sibling_modules(tmp_path, monkeypatch):
    helper_name = "isolated_loader_collision_helper"
    package_name = "isolated_loader_collision_package"
    hook_path = tmp_path / "hook.py"
    (tmp_path / f"{helper_name}.py").write_text("VALUE = 'local-helper'\n")
    package_dir = tmp_path / package_name
    package_dir.mkdir()
    (package_dir / "__init__.py").write_text("VALUE = 'local-package'\n")
    hook_path.write_text(
        f"from {helper_name} import VALUE as helper_value\n"
        f"from {package_name} import VALUE as package_value\n"
    )

    foreign_helper = ModuleType(helper_name)
    foreign_helper.VALUE = "foreign-helper"
    foreign_package = ModuleType(package_name)
    foreign_package.VALUE = "foreign-package"
    monkeypatch.setitem(sys.modules, helper_name, foreign_helper)
    monkeypatch.setitem(sys.modules, package_name, foreign_package)
    previous_path = sys.path.copy()

    module_name = "isolated_standalone_hook_probe"
    try:
        hook = load_standalone_hook(module_name, hook_path)

        assert hook.helper_value == "local-helper"
        assert hook.package_value == "local-package"
        assert sys.modules[helper_name] is foreign_helper
        assert sys.modules[package_name] is foreign_package
        assert sys.path == previous_path
    finally:
        sys.modules.pop(module_name, None)


def test_load_package_from_path_assigns_unique_plugin_names(tmp_path):
    first_package = tmp_path / "first" / "src"
    second_package = tmp_path / "second" / "src"
    first_package.mkdir(parents=True)
    second_package.mkdir(parents=True)
    (first_package / "__init__.py").write_text("PLUGIN = 'first'\n")
    (second_package / "__init__.py").write_text("PLUGIN = 'second'\n")

    module_names = ("isolated_first_plugin_src", "isolated_second_plugin_src")
    try:
        first = load_package_from_path(module_names[0], first_package)
        second = load_package_from_path(module_names[1], second_package)

        assert first.PLUGIN == "first"
        assert second.PLUGIN == "second"
        assert first is not second
    finally:
        for module_name in module_names:
            sys.modules.pop(module_name, None)
