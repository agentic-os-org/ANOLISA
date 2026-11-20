#!/bin/sh
# Run with the installed CLI on a kernel with a known Landlock configuration.
set -eu
cli=${1:?supply an absolute agent-sec-cli path}
expected=${2:?supply enabled, disabled (EOPNOTSUPP) or no-syscall (ENOSYS)}
case "$expected" in
    enabled) reason='' ;;
    disabled) reason='LSM disabled' ;;
    no-syscall) reason='system call unavailable' ;;
    *) exit 2 ;;
esac
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir "$root/work" "$root/work/.git"
echo original > "$root/outside"
echo protected > "$root/work/.git/config"
cd "$root/work"
cat > "$root/permissions.json" <<EOF
{"version":1,"network":{"mode":"off"},"execution":{"deny_paths":[]},
 "filesystem":{"default":"deny","rules":[
  {"effect":"allow","path":"/","rights":["read"]},
  {"effect":"allow","path":"$root/work","rights":["write"]},
  {"effect":"deny","path":"$root/work/.git","rights":["write"]}]}}
EOF
for mode in default off auto required; do
    set --
    if [ "$mode" != default ]; then set -- --landlock "$mode"; fi
    # The first target operation is the startup marker. A failing target after
    # exec cannot masquerade as a refused launch.
    if "$cli" sandbox run "$@" --permissions "$root/permissions.json" -- /bin/sh -ec '
        echo started > "$1"
        (echo forbidden > .git/config) && exit 21
        (echo forbidden > "$2") && exit 22
        /bin/sh -ec "echo child > child; echo ok > /dev/null"
    ' sh "$mode" "$root/outside" 2>"$root/log"; then
        if [ "$mode" = required ] && [ "$expected" != enabled ]; then exit 1; fi
        test -f "$mode"
        test "$(cat child)" = child
        test "$(cat .git/config)" = protected
        test "$(cat "$root/outside")" = original
        case "$mode/$expected" in
            auto/disabled|auto/no-syscall) grep -Fq "Landlock skipped: $reason" "$root/log" ;;
            auto/enabled|required/enabled) grep -q 'Landlock enforced' "$root/log" ;;
            *) if grep -q Landlock "$root/log"; then exit 1; fi ;;
        esac
    else
        if [ "$expected" = enabled ] || [ "$mode" != required ]; then cat "$root/log" >&2; exit 1; fi
        grep -Fq "Landlock write protection: $reason" "$root/log"
        test ! -e "$mode"
    fi
    echo "PASS Landlock $expected / $mode"
done
