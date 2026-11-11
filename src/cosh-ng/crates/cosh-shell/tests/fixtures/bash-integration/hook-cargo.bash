#!/bin/bash
builtin printf '%s\n' "$*" >>"$COSH_BASH_TEST_WORKDIR/tool.calls"
builtin printf '%s\n' TOOL_OUTPUT
exit 101
