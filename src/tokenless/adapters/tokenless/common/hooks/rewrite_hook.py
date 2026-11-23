#!/usr/bin/env python3
"""Compatibility entrypoint for the pre_tool hook."""

import sys

from hook_entry import main

if __name__ == "__main__":
    main("pre_tool_recovery" if "--recovery-only" in sys.argv[1:] else "pre_tool")
