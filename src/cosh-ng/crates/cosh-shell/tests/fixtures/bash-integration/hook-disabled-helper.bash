#!/bin/bash
builtin printf '%s\n' "$*" >>"${COSH_BASH_TEST_WORKDIR:?}/hook.calls"
[[ $1 == shell-integration && $2 == compat ]] || exit 64
builtin printf '%s\0' 2 DISABLE guidance_off '' '' '' '' ''
