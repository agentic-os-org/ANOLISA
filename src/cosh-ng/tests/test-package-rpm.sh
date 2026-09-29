#!/usr/bin/env bash
# Exercise the RPM spec scriptlets without building the RPM: the %post
# /etc/shells registration through the real RPM Lua interpreter and the
# %preun erase guard through fixture-backed bash runs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC="$ROOT/cosh-ng.spec.in"
TMP="$(mktemp -d /tmp/cosh-ng-rpm-scriptlet-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# --- structural anchors: the lifecycle sections must stay in the spec ---
grep -q '^%preun$' "$SPEC"
grep -q '^%define cosh_replacement_ready ' "$SPEC"
grep -q '^Requires(preun): ' "$SPEC"
grep -Fxq \
    'Requires(preun):  (%{_bindir}/systemctl if systemd) /usr/bin/getent /usr/bin/awk' \
    "$SPEC"
grep -Fq "systemctl stop 'cosh-gateway@*.service' 'cosh-gateway-acp@*.service'" "$SPEC"
grep -Fxq '%systemd_preun cosh-gateway@.service' "$SPEC"
grep -Fq \
    'skills/manage-task-checkpoints/SKILL.md' \
    "$SPEC"
grep -Fxq \
    '%{_datadir}/anolisa/skills/manage-task-checkpoints/' \
    "$SPEC"
if grep -Fxq '%systemd_preun cosh-gateway-acp@.service' "$SPEC"; then
    echo "ERROR: removed legacy ACP unit still has a lifecycle macro" >&2
    exit 1
fi
grep -q '^%post -p <lua>$' "$SPEC"
# the extraction below slices on section boundaries, so the sections must
# keep their order: %preun, then %post, then %postun
awk '
    /^%preun$/ { a = NR }
    /^%post -p <lua>$/ { b = NR }
    /^%postun/ { c = NR }
    END { exit !(a && b && c && a < b && b < c) }
' "$SPEC"

# --- %postun must be a swap-safe, metadata-preserving bash scriptlet ---
# It removes only cosh's own /etc/shells line, and only on final erase when
# /usr/bin/cosh is gone (a replacement provider keeps it -> the line stays);
# the rewrite uses a temp copy + atomic rename and preserves mode/ownership/
# xattrs. The guard tests the file directly (fail-safe) and must NOT run rpm
# inside the scriptlet -- a nested query can fail under the transaction lock.
grep -q '^%postun$' "$SPEC"
if grep -q '^%postun -p <lua>$' "$SPEC"; then
    echo "ERROR: %postun must be a bash scriptlet, not lua" >&2
    exit 1
fi
grep -Fq '[ ! -x "%{_bindir}/cosh" ]' "$SPEC"
if awk '/^%postun$/{f=1;next} /^%[a-z]/{f=0} f' "$SPEC" | grep -v '^[[:space:]]*#' | grep -qwE 'rpm|cosh_replacement_ready'; then
    echo "ERROR: %postun must not run rpm (nested rpm in a scriptlet is unsafe under the tx lock)" >&2
    exit 1
fi
grep -Fq 'cp --attributes-only --preserve=mode,ownership,xattr' "$SPEC"
grep -Fxq \
    'Requires(postun): /usr/bin/awk /usr/bin/cp /usr/bin/mktemp /usr/bin/mv /usr/bin/readlink /usr/bin/rm' \
    "$SPEC"
if grep -q '^Requires(postun): lua$' "$SPEC"; then
    echo "ERROR: %postun no longer needs the lua interpreter" >&2
    exit 1
fi

# --- %post registration matrix through the real RPM Lua interpreter ---
SHELLS="$TMP/shells"
COSH="$TMP/cosh"

post_script() {
    awk '/^%post -p <lua>$/{f=1;next} /^%/{f=0} f' "$SPEC" |
        sed -e "s|/etc/shells|$SHELLS|g" -e "s|%{_bindir}/cosh|$COSH|g"
}

POST_SCRIPT="$(post_script)"
# the substitutions must have taken effect: a silent sed no-op would make
# the scriptlet below run against the real /etc/shells of this machine
case "$POST_SCRIPT" in
    *"$SHELLS"*) : ;;
    *)
        echo "ERROR: shells path substitution missed the %post scriptlet" >&2
        exit 1
        ;;
esac
case "$POST_SCRIPT" in
    *"$COSH"*) : ;;
    *)
        echo "ERROR: registration path substitution missed the %post scriptlet" >&2
        exit 1
        ;;
esac
case "$POST_SCRIPT" in
    *'/etc/shells'* | *'%{_bindir}/cosh'*)
        echo "ERROR: %post scriptlet still references packaged paths" >&2
        exit 1
        ;;
esac

run_post() {
    rpm --eval "%{lua:$POST_SCRIPT}" >/dev/null
}

expect_shells() {
    local name="$1"
    printf '%s' "$2" > "$TMP/expected"
    if ! cmp -s "$TMP/expected" "$SHELLS"; then
        echo "ERROR: %post case '$name' produced unexpected bytes:" >&2
        od -c "$SHELLS" >&2
        exit 1
    fi
}

run_post_case() {
    local name="$1"
    local initial="$2"
    local expected="$3"

    if [ "$initial" = "<missing>" ]; then
        rm -f "$SHELLS"
    else
        printf '%s' "$initial" > "$SHELLS"
    fi
    run_post
    expect_shells "$name (install)" "$expected"
    run_post
    expect_shells "$name (reinstall)" "$expected"
}

if command -v rpm >/dev/null 2>&1 && rpm --eval '%{lua:print("ok")}' >/dev/null 2>&1; then
    # the shared predicate must survive macro expansion with a queryformat
    # that emits a real newline (%%{NAME} folds to %{NAME}, \\n folds to \n)
    predicate_line="$(sed -n 's/^%define cosh_replacement_ready //p' "$SPEC")"
    expanded="$(rpm --define "cosh_replacement_ready $predicate_line" \
        --eval '%{cosh_replacement_ready}')"
    case "$expanded" in
        *"--qf '%{NAME}\n' -f"*) : ;;
        *)
            echo "ERROR: cosh_replacement_ready expanded unexpectedly: $expanded" >&2
            exit 1
            ;;
    esac

    run_post_case "missing file" "<missing>" "$COSH"$'\n'
    run_post_case "empty file" "" "$COSH"$'\n'
    run_post_case "missing trailing newline" \
        $'/bin/sh\n/bin/bash' \
        $'/bin/sh\n/bin/bash\n'"$COSH"$'\n'
    run_post_case "existing trailing newline" \
        $'/usr/bin/bash\n' \
        $'/usr/bin/bash\n'"$COSH"$'\n'
    run_post_case "existing exact registration" \
        $'/usr/bin/bash\n'"$COSH"$'\n/usr/bin/zsh\n' \
        $'/usr/bin/bash\n'"$COSH"$'\n/usr/bin/zsh\n'
    run_post_case "duplicate registrations preserved" \
        $'/usr/bin/bash\n'"$COSH"$'\n'"$COSH"$'\n' \
        $'/usr/bin/bash\n'"$COSH"$'\n'"$COSH"$'\n'
    run_post_case "substring is not a registration" \
        $'/usr/bin/bash\n'"$COSH"$'-backup\n' \
        $'/usr/bin/bash\n'"$COSH"$'-backup\n'"$COSH"$'\n'

    # registration stays fail-open when the shells file cannot be opened,
    # but the failure must be observable (not silent).
    rm -f "$SHELLS"
    rpm --eval "%{lua:io.open = function() return nil, 'Read-only file system' end
$(post_script)}" >/dev/null 2>"$TMP/post.warn"
    if [ -e "$SHELLS" ]; then
        echo "ERROR: fail-open %post unexpectedly touched the shells file" >&2
        exit 1
    fi
    if ! grep -Fq 'could not register' "$TMP/post.warn"; then
        echo "ERROR: fail-open %post did not emit an observable warning" >&2
        exit 1
    fi
else
    echo "SKIP: rpm lua interpreter unavailable; %post matrix not exercised" >&2
fi

# --- %preun erase guard matrix through fixture-backed bash runs ---
STUB="$TMP/stub-bin"
install -d -m 0755 "$STUB"
GUARD_COSH="$STUB/cosh"
SYSTEMD_RUNTIME="$TMP/run/systemd/system"
SYSTEMCTL_LOG="$TMP/systemctl.log"
install -d -m 0755 "$SYSTEMD_RUNTIME"

PREDICATE="$(sed -n 's/^%define cosh_replacement_ready //p' "$SPEC")"
PREUN_RAW="$(awk '/^%preun$/{f=1;next} /^%post/{f=0} f' "$SPEC" |
    sed -e '/^%systemd_preun cosh-gateway@\.service$/d')"
# Bash 5.2 enables patsub_replacement by default, which would expand every
# unquoted '&' in the substituted predicate to the matched pattern text;
# keep the replacement strings verbatim.
shopt -u patsub_replacement 2>/dev/null || :
PREUN="${PREUN_RAW//'%{cosh_replacement_ready}'/$PREDICATE}"
PREUN="${PREUN//'%{_bindir}'/$STUB}"
PREUN="${PREUN//\/run\/systemd\/system/$SYSTEMD_RUNTIME}"
PREUN="${PREUN//%%/%}"
case "$PREUN" in
    *'%{cosh_replacement_ready}'*)
        echo "ERROR: %preun still references the unexpanded predicate macro" >&2
        exit 1
        ;;
esac
expected_predicate="${PREDICATE//'%{_bindir}'/$STUB}"
expected_predicate="${expected_predicate//%%/%}"
case "$PREUN" in
    *"$expected_predicate"*) : ;;
    *)
        echo "ERROR: predicate was not spliced verbatim into %preun" >&2
        exit 1
        ;;
esac

write_stub() {
    printf '%s\n' "#!/usr/bin/env bash" "$2" > "$STUB/$1"
    chmod 0755 "$STUB/$1"
}

run_preun() {
    local action="$1"
    PATH="$STUB:/usr/bin:/bin" bash -c "$PREUN" cosh-preun "$action"
}

expect_preun() {
    local name="$1"
    local action="$2"
    local expected_status="$3"
    local status=0

    run_preun "$action" >"$TMP/preun.out" 2>"$TMP/preun.err" || status=$?
    if [ "$status" -ne "$expected_status" ]; then
        echo "ERROR: %preun case '$name' exited $status, expected $expected_status:" >&2
        cat "$TMP/preun.err" >&2
        exit 1
    fi
}

write_stub getent "printf '%s\n' 'coshuser:x:1000:1000::/home/coshuser:$GUARD_COSH'"
write_stub rpm "printf '%s\n' cosh-ng"
write_stub cosh ":"
write_stub systemctl "printf '%s\n' \"\$*\" >> '$SYSTEMCTL_LOG'"

: > "$SYSTEMCTL_LOG"
expect_preun "erase with cosh login-shell user" 0 1
grep -Fq coshuser "$TMP/preun.err"
grep -Fq "$GUARD_COSH" "$TMP/preun.err"
test ! -s "$SYSTEMCTL_LOG"

expect_preun "upgrade never blocks" 1 0
test ! -s "$SYSTEMCTL_LOG"

write_stub getent "printf '%s\n' 'root:x:0:0:root:/root:/usr/bin/bash'"
expect_preun "erase without cosh users" 0 0
grep -Fxq 'stop cosh-gateway@*.service cosh-gateway-acp@*.service' "$SYSTEMCTL_LOG"

write_stub systemctl "printf '%s\n' \"\$*\" >> '$SYSTEMCTL_LOG'; exit 4"
expect_preun "failed Gateway instance stop" 0 1
grep -Fq 'failed to stop running Gateway instances' "$TMP/preun.err"

rmdir "$SYSTEMD_RUNTIME"
expect_preun "erase without a systemd manager" 0 0
install -d -m 0755 "$SYSTEMD_RUNTIME"
write_stub systemctl "printf '%s\n' \"\$*\" >> '$SYSTEMCTL_LOG'"

write_stub getent "exit 2"
expect_preun "failed passwd enumeration" 0 1
expect_preun "upgrade with broken enumeration" 1 0

write_stub getent "exit 0"
expect_preun "empty passwd enumeration" 0 1

write_stub getent "printf '%s\n' 'root:x:0:0:root:/root:/usr/bin/bash'"
write_stub awk "exit 3"
expect_preun "failed passwd filter" 0 1
rm -f "$STUB/awk"

write_stub getent "printf '%s\n' 'coshuser:x:1000:1000::/home/coshuser:$GUARD_COSH'"
write_stub rpm "exit 1"
expect_preun "failed replacement lookup" 0 1

write_stub rpm "printf '%s\n' cosh-ng unexpected-shell"
expect_preun "unexpected replacement owner" 0 1

write_stub rpm "printf '%s\n' cosh-ng copilot-shell"
chmod 0644 "$GUARD_COSH"
expect_preun "non-executable replacement" 0 1

chmod 0755 "$GUARD_COSH"
expect_preun "atomic provider swap" 0 0

rm -f "$GUARD_COSH"
expect_preun "upgrade without launcher" 1 0

# --- %postun /etc/shells removal matrix through fixture-backed bash runs ---
# %postun uses GNU coreutils (cp --attributes-only); skip the behavioral matrix
# on non-GNU hosts (e.g. macOS dev) like the %post lua matrix skips without rpm.
# The structural anchors above already ran on every host.
if cp --version 2>/dev/null | grep -q 'GNU coreutils'; then
    POSTUN_ETC="$TMP/postun-etc"
    install -d -m 0755 "$POSTUN_ETC"
    POSTUN_SHELLS="$POSTUN_ETC/shells"
    POSTUN_RAW="$(awk '/^%postun$/{f=1;next} /^%/{f=0} f' "$SPEC")"
    POSTUN="${POSTUN_RAW//'%{_bindir}'/$STUB}"
    POSTUN="${POSTUN//'%{_sysconfdir}'/$POSTUN_ETC}"
    POSTUN="${POSTUN//%%/%}"
    case "$POSTUN" in
        *'%{_sysconfdir}'* | *'%{_bindir}'*)
            echo "ERROR: %postun still references an unexpanded macro" >&2
            exit 1
            ;;
    esac

    run_postun() { PATH="$STUB:/usr/bin:/bin" bash -c "$POSTUN" cosh-postun "$1"; }

    expect_postun_shells() {
        local name="$1"
        if ! cmp -s "$TMP/postun.expected" "$POSTUN_SHELLS"; then
            echo "ERROR: %postun case '$name' produced unexpected bytes:" >&2
            od -c "$POSTUN_SHELLS" >&2
            exit 1
        fi
    }

    # final erase, no replacement provider: drop only cosh's own line
    rm -f "$GUARD_COSH"
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh '# admin comment' > "$POSTUN_SHELLS"
    chmod 0640 "$POSTUN_SHELLS"
    run_postun 0
    printf '%s\n' /bin/sh /usr/bin/zsh '# admin comment' > "$TMP/postun.expected"
    expect_postun_shells "erase drops only cosh line"
    if [ "$(stat -c '%a' "$POSTUN_SHELLS")" != 640 ]; then
        echo "ERROR: %postun did not preserve /etc/shells mode" >&2
        exit 1
    fi

    # upgrade ($1=1): never touch the shared table
    printf '%s\n' /bin/sh "$STUB/cosh" > "$POSTUN_SHELLS"
    run_postun 1
    printf '%s\n' /bin/sh "$STUB/cosh" > "$TMP/postun.expected"
    expect_postun_shells "upgrade leaves shells untouched"

    # replacement provider still owns an executable /usr/bin/cosh: keep the line
    write_stub cosh ":"
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$POSTUN_SHELLS"
    run_postun 0
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$TMP/postun.expected"
    expect_postun_shells "replacement present keeps registration"

    # admin edited the cosh line (no longer an exact match): keep it
    rm -f "$GUARD_COSH"
    printf '%s\n' /bin/sh "$STUB/cosh --restricted" > "$POSTUN_SHELLS"
    run_postun 0
    printf '%s\n' /bin/sh "$STUB/cosh --restricted" > "$TMP/postun.expected"
    expect_postun_shells "admin-modified line preserved"

    # cp failure mid-rewrite: nonzero exit, /etc/shells untouched, temp cleaned
    rm -f "$GUARD_COSH"
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$POSTUN_SHELLS"
    cp "$POSTUN_SHELLS" "$TMP/postun.before-cpfail"
    write_stub cp 'exit 1'
    st=0
    run_postun 0 >/dev/null 2>&1 || st=$?
    rm -f "$STUB/cp"
    if [ "$st" -eq 0 ]; then
        echo "ERROR: %postun cp-failure must exit nonzero" >&2
        exit 1
    fi
    if ! cmp -s "$TMP/postun.before-cpfail" "$POSTUN_SHELLS"; then
        echo "ERROR: %postun cp-failure must leave /etc/shells untouched" >&2
        exit 1
    fi
    if ls "$POSTUN_ETC"/shells.cosh-ng.* >/dev/null 2>&1; then
        echo "ERROR: %postun cp-failure left a temp file (trap cleanup failed)" >&2
        exit 1
    fi
    echo "PASS: %postun cp-failure exits nonzero, /etc/shells untouched, temp cleaned"

    # signal (TERM) mid-rewrite must NOT empty /etc/shells: the handler must
    # exit, not clean-and-resume into cp/mv. awk wrapper TERMs our shell first.
    rm -f "$GUARD_COSH"
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$POSTUN_SHELLS"
    cp "$POSTUN_SHELLS" "$TMP/postun.before-sig"
    # shellcheck disable=SC2016  # $PPID/$@ must stay literal in the written wrapper
    printf '%s\n' '#!/usr/bin/env bash' 'kill -TERM "$PPID"; exec /usr/bin/awk "$@"' > "$STUB/awk"
    chmod 0755 "$STUB/awk"
    st=0
    run_postun 0 >/dev/null 2>&1 || st=$?
    rm -f "$STUB/awk"
    if [ "$st" -eq 0 ]; then
        echo "ERROR: %postun interrupted by TERM must exit nonzero" >&2
        exit 1
    fi
    if ! cmp -s "$TMP/postun.before-sig" "$POSTUN_SHELLS"; then
        echo "ERROR: %postun TERM-interrupt emptied/altered /etc/shells" >&2
        od -c "$POSTUN_SHELLS" >&2
        exit 1
    fi
    if ls "$POSTUN_ETC"/shells.cosh-ng.* >/dev/null 2>&1; then
        echo "ERROR: %postun TERM-interrupt left a temp file" >&2
        exit 1
    fi
    echo "PASS: %postun TERM-interrupt leaves /etc/shells intact, exits nonzero, temp cleaned"

    # /etc/shells as an admin-managed symlink: keep the link, rewrite its target
    rm -f "$GUARD_COSH" "$POSTUN_SHELLS"
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$POSTUN_ETC/managed-shells"
    ln -s "$POSTUN_ETC/managed-shells" "$POSTUN_SHELLS"
    run_postun 0
    if [ ! -L "$POSTUN_SHELLS" ]; then
        echo "ERROR: %postun replaced the /etc/shells symlink with a regular file" >&2
        exit 1
    fi
    printf '%s\n' /bin/sh /usr/bin/zsh > "$TMP/postun.expected"
    if ! cmp -s "$TMP/postun.expected" "$POSTUN_ETC/managed-shells"; then
        echo "ERROR: %postun did not rewrite the symlink target" >&2
        od -c "$POSTUN_ETC/managed-shells" >&2
        exit 1
    fi
    echo "PASS: %postun preserves an admin /etc/shells symlink and rewrites its target"
    rm -f "$POSTUN_SHELLS" "$POSTUN_ETC/managed-shells"

    # missing /etc/shells is a no-op, not a crash or recreation
    rm -f "$POSTUN_SHELLS" "$GUARD_COSH"
    run_postun 0
    if [ -e "$POSTUN_SHELLS" ]; then
        echo "ERROR: %postun recreated a missing /etc/shells" >&2
        exit 1
    fi

    echo "cosh-ng %postun matrix passed"
else
    echo "SKIP: GNU coreutils unavailable; %postun matrix not exercised" >&2
fi

echo "cosh-ng rpm scriptlet tests passed"
