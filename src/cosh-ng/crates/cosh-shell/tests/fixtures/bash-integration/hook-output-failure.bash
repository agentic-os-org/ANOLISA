#!/bin/bash
builtin printf '%s\n' printf-attempt >>"$COSH_BASH_TEST_WORKDIR/output.calls"
builtin exit 73
