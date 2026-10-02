"""Load standalone hook scripts without leaking their sibling modules."""

import importlib.util
import pkgutil
import sys
from pathlib import Path
from types import ModuleType

_MISSING = object()


def load_module_from_path(module_name: str, module_path: Path) -> ModuleType:
    """Load one file under an explicit, caller-owned module name."""
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    assert spec is not None
    assert spec.loader is not None

    module = importlib.util.module_from_spec(spec)
    previous = sys.modules.get(module_name, _MISSING)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        if previous is _MISSING:
            sys.modules.pop(module_name, None)
        else:
            sys.modules[module_name] = previous
        raise
    return module


def load_package_from_path(package_name: str, package_path: Path) -> ModuleType:
    """Load one package directory under an explicit, unique package name."""
    init_path = package_path / "__init__.py"
    spec = importlib.util.spec_from_file_location(
        package_name,
        init_path,
        submodule_search_locations=[str(package_path)],
    )
    assert spec is not None
    assert spec.loader is not None

    package = importlib.util.module_from_spec(spec)
    previous = sys.modules.get(package_name, _MISSING)
    sys.modules[package_name] = package
    try:
        spec.loader.exec_module(package)
    except BaseException:
        if previous is _MISSING:
            sys.modules.pop(package_name, None)
        else:
            sys.modules[package_name] = previous
        raise
    return package


def _importable_sibling_names(directory: Path) -> set[str]:
    """Return every importable top-level module or package beside a hook."""
    return {
        module.name
        for module in pkgutil.iter_modules([str(directory)])
        if module.name.isidentifier()
    }


def _matching_module_keys(sibling_names: set[str]) -> set[str]:
    """Return loaded module keys rooted at one of the sibling names."""
    return {
        module_name
        for module_name in sys.modules
        if module_name.partition(".")[0] in sibling_names
    }


def load_standalone_hook(module_name: str, hook_path: Path) -> ModuleType:
    """Load a hook with an isolated namespace for all eager sibling imports.

    Standalone hooks use imports such as ``from trace_context import ...``
    because production launches each script in its own interpreter. Unit tests
    load several plugins into one interpreter, so every importable sibling is
    temporarily removed from the process-wide module cache while the hook is
    executed. Existing modules and ``sys.path`` are restored afterward.
    """
    sibling_names = _importable_sibling_names(hook_path.parent)
    if module_name.partition(".")[0] in sibling_names:
        raise ValueError("module_name must be unique outside the hook directory")

    previous_path = sys.path.copy()
    previous_modules = {
        key: sys.modules.pop(key) for key in _matching_module_keys(sibling_names)
    }
    sys.path.insert(0, str(hook_path.parent))
    try:
        return load_module_from_path(module_name, hook_path)
    finally:
        sys.path[:] = previous_path
        for key in _matching_module_keys(sibling_names):
            sys.modules.pop(key, None)
        sys.modules.update(previous_modules)
