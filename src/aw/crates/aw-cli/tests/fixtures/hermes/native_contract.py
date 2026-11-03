"""Exercise the pinned Hermes dispatcher with an owned profile."""

import argparse
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True

from native_guard import GuardTests
from native_hooks import HooksTests
from native_reload import ReloadTests
from native_support import NativeFixture

# Export the test classes for unittest discovery through this stable entrypoint.
__all__ = ["GuardTests", "HooksTests", "ReloadTests"]

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--plugin", type=Path, required=True)
    args = parser.parse_args()
    NativeFixture.plugin_path = args.plugin
    sys.path.insert(0, str(args.source))
    unittest.main(argv=[sys.argv[0]])
