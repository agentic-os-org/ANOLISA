#!/bin/bash
builtin printf '%s\n' "$*" >>"${COSH_BASH_TEST_WORKDIR:?}/hook.calls"
[[ $1 == shell-integration && ( $2 == compat || $2 == advise ) ]] || exit 64
__fixture_fields=()
while IFS= builtin read -r -d '' __fixture_field; do __fixture_fields+=("$__fixture_field"); done
[[ ${__fixture_fields[0]} == 2 ]] || exit 65
if [[ $2 == compat ]]; then [[ ${#__fixture_fields[@]} == 6 ]] || exit 65; else [[ ${#__fixture_fields[@]} == 15 ]] || exit 65; fi
/usr/bin/mkdir -p -m 700 "${COSH_BASH_TEST_WORKDIR}/runtime" || exit 66
__fixture_digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
if [[ $2 == advise ]]; then
 if [[ ${COSH_BASH_TEST_CASE-} == hook_live_skip_candidate && ${__fixture_fields[6]} == 'cargo build' ]]; then
  builtin printf '%s\n' 'SKIP:cargo build' >>"${COSH_BASH_TEST_WORKDIR}/hook.advise"
  builtin printf '%s\0' 2 SKIP suppressed '' '' '' '' ''
 else
  builtin printf 'SHOW:%s\n' "${__fixture_fields[6]}" >>"${COSH_BASH_TEST_WORKDIR}/hook.advise"
  builtin printf '%s\0' 2 SHOW suggest_only "$__fixture_digest" '' '' "${COSH_BASH_TEST_WORKDIR}/runtime" FRAGMENT_INVITATION
 fi
 exit
fi
builtin printf '%s\0' 2 SHOW compatible "$__fixture_digest" "$__fixture_digest" '' "${COSH_BASH_TEST_WORKDIR}/runtime" ''
if [[ ${COSH_BASH_TEST_CASE-} == hook_bad_tail ]]; then builtin printf x; fi
