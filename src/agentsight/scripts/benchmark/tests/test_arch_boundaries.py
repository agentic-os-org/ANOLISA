"""Architecture boundary checker regressions.

`extract_imports` matched only the simple `use crate::module;` form, so a
layer violation written as a brace group (`use crate::{a, b};`) produced no
imports at all and silently passed the CI gate.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SCRIPT = Path(__file__).parents[2] / "check-arch-boundaries.py"


def load_module():
    spec = importlib.util.spec_from_file_location("check_arch_boundaries", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


checker = load_module()


def targets(text: str) -> list:
    return [target for _, target in checker.extract_imports(text)]


def test_simple_import_still_yields_its_module():
    assert "storage" in targets("use crate::storage::db;\n")


def test_grouped_import_yields_every_module():
    assert targets("use crate::{storage, server};\n") == ["storage", "server"]


def test_multiline_grouped_import_yields_every_module():
    text = "use crate::{\n    storage,\n    server,\n};\n"
    assert targets(text) == ["storage", "server"]


def test_grouped_import_reports_a_layer_violation():
    # genai (L5) may not reach server (L7); before the fix the grouped import
    # produced no target and classify() was never called.
    assert targets("use crate::{storage, server};\n") == ["storage", "server"]
    verdict, _detail = checker.classify("genai", "server", Path("genai/builder.rs"))
    assert verdict == "violation"


def test_grouped_import_inside_cfg_test_block_is_skipped():
    text = "#[cfg(test)]\nmod tests {\n    use crate::{storage, server};\n}\n"
    assert targets(text) == []


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok {name}")
