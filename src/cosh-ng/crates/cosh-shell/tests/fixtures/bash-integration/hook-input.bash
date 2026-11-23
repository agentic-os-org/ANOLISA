builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-live.bash"
false
builtin printf 'OBS_FALSE:%s:%s\n' "$?" "${PIPESTATUS[*]}"
builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-live.bash" before-first
cargo build
builtin printf 'OBS_CARGO:%s:%s\n' "$?" "${PIPESTATUS[*]}"
builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-live.bash" after-first
builtin printf '%s\n' NO_EVENT
true && cargo build
alias cb='cargo build'
cb
printf x | cargo build
f() { cargo build; }; f
(cargo build)
builtin printf '%s\n' MULTI\
LINE
builtin read -e -p 'READ>' answer
readline-answer
builtin printf 'READ_RESULT:%s\n' "$answer"
if [[ $COSH_BASH_TEST_CASE != hook_live_skip_* ]]; then builtin : > "$COSH_BASH_TEST_WORKDIR/runtime/disabled"; fi
cargo test
builtin printf 'OBS_TEST:%s:%s\n' "$?" "${PIPESTATUS[*]}"
builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-live.bash" after-final
false
builtin : > "$COSH_BASH_TEST_WORKDIR/runtime/disabled"
builtin printf '%s\n' DONE
builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-live.bash" finish
exit
