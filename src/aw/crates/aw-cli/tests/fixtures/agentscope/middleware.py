"""Run all pinned AgentScope middleware contracts."""

import sys
import unittest

sys.dont_write_bytecode = True

from middleware_lifecycle import LifecycleTests
from middleware_permissions import PermissionDecisionTests
from middleware_results import ResultsTests, StrictWithholdingTests
from middleware_scope import ScopeTests
from middleware_transport import TransportTests

# Export the test classes for unittest discovery through this stable entrypoint.
__all__ = [
    "LifecycleTests",
    "PermissionDecisionTests",
    "ResultsTests",
    "ScopeTests",
    "StrictWithholdingTests",
    "TransportTests",
]

if __name__ == "__main__":
    unittest.main()
