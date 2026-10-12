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
# keep their order: %preun, then %post, then %postun, then %posttrans
awk '
    /^%preun$/ { a = NR }
    /^%post -p <lua>$/ { b = NR }
    /^%postun/ { c = NR }
    /^%posttrans$/ { d = NR }
    END { exit !(a && b && c && d && a < b && b < c && c < d) }
' "$SPEC"

# --- S2 anchors: /etc/shells stage directory + reclaim loop ---
# %postun must use a private 0700 stage directory, not a sibling temp file:
# this makes residues ownable (every stage carries an "owner" record with
# boot_id/pid/starttime), reclaimable (next transaction wipes dead stages),
# and refuses to touch files above a non-root-writable parent chain.
grep -Fq '.cosh-ng-shells.' "$SPEC"
grep -Fq 'cosh-ng-shells-stage v1' "$SPEC"
grep -Fq 'cosh_reg="%{_localstatedir}/lib/rpm-state/cosh-ng"' "$SPEC"
grep -Fxq \
    'Requires(postun): /usr/bin/awk /usr/bin/cat /usr/bin/cp /usr/bin/dirname /usr/bin/id /usr/bin/mkdir /usr/bin/mktemp /usr/bin/mv /usr/bin/readlink /usr/bin/rm /usr/bin/rmdir /usr/bin/stat' \
    "$SPEC"
grep -Fxq \
    'Requires(posttrans): /usr/bin/cat /usr/bin/dirname /usr/bin/id /usr/bin/rm /usr/bin/rmdir /usr/bin/stat' \
    "$SPEC"
# shellcheck disable=SC2016  # literal ${target} is the pre-S2 spec text we forbid
if grep -q 'mktemp "\${target}\.cosh-ng\.XXXXXX"' "$SPEC"; then
    echo "ERROR: %postun must not use a sibling random-name temp file" >&2
    echo "       (a SIGKILL would leave an unownable residue; use mkdir -p stage dir)" >&2
    exit 1
fi
# %postun and %posttrans carry the same guard+reclaim block; they must not drift
guard_block() {
    awk -v sec="$1" '
        $0 == sec { f = 1; next }
        /^%[a-z]/ { f = 0 }
        f && /# BEGIN cosh-ng shells guard\+reclaim/ { b = 1 }
        f && b { sub(/^[[:space:]]+/, ""); print }
        f && /# END cosh-ng shells guard\+reclaim/ { b = 0 }
    ' "$SPEC"
}
POSTUN_BLOCK="$(guard_block '%postun')"
POSTTRANS_BLOCK="$(guard_block '%posttrans')"
if [ -z "$POSTUN_BLOCK" ] || [ "$POSTUN_BLOCK" != "$POSTTRANS_BLOCK" ]; then
    echo "ERROR: %postun and %posttrans guard+reclaim blocks are missing or differ" >&2
    diff <(printf '%s\n' "$POSTUN_BLOCK") <(printf '%s\n' "$POSTTRANS_BLOCK") >&2 || :
    exit 1
fi
if awk '/^%posttrans$/{f=1;next} /^%[a-z]/{f=0} f' "$SPEC" |
    grep -v '^[[:space:]]*#' | grep -qE '(^|[;&|(`[:space:]])rpm([[:space:]]|$)|cosh_replacement_ready'; then
    echo "ERROR: %posttrans must not run rpm (nested rpm under tx lock is unsafe)" >&2
    exit 1
fi

# --- %postun must be a swap-safe, metadata-preserving bash scriptlet ---
# It removes only cosh's own /etc/shells line, and only on final erase when
# /usr/bin/cosh is gone (a replacement provider keeps it -> the line stays);
# the rewrite uses a private stage dir + atomic rename and preserves mode/
# ownership/xattrs. The guard tests the file directly (fail-safe) and must
# NOT run rpm inside the scriptlet -- a nested query can fail under the
# transaction lock. A non-root-writable parent chain disables the rewrite
# entirely to avoid root dereferencing a planted symlink.
grep -q '^%postun$' "$SPEC"
if grep -q '^%postun -p <lua>$' "$SPEC"; then
    echo "ERROR: %postun must be a bash scriptlet, not lua" >&2
    exit 1
fi
grep -Fq '[ ! -x "%{_bindir}/cosh" ]' "$SPEC"
if awk '/^%postun$/{f=1;next} /^%[a-z]/{f=0} f' "$SPEC" | grep -v '^[[:space:]]*#' |
    grep -qE '(^|[;&|(`[:space:]])rpm([[:space:]]|$)|cosh_replacement_ready'; then
    echo "ERROR: %postun must not run rpm (nested rpm in a scriptlet is unsafe under the tx lock)" >&2
    exit 1
fi
grep -Fq 'cp --attributes-only --preserve=mode,ownership,xattr' "$SPEC"
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
    # The scriptlet only rewrites below a parent chain owned by root or the
    # running uid with no group/other write bit. /tmp is world-writable (the
    # guard rejects it, correctly), so the simulated root must live under a
    # chain that passes the same predicate; CI runs unprivileged, so the
    # search starts from $HOME rather than a root-owned path.
    chain_safe() {
        local d="$1" uid meta
        uid="$(id -u)"
        while :; do
            meta="$(stat -c '%u %A' -- "$d" 2>/dev/null)" || return 1
            case "${meta#* }" in ?????w*|????????w*) return 1 ;; esac
            case "${meta%% *}" in 0|"$uid") : ;; *) return 1 ;; esac
            [ "$d" = / ] && return 0
            d="$(dirname -- "$d")"
        done
    }
    POSTUN_BASE=""
    for candidate in "${COSH_RPM_TEST_SAFE_DIR:-}" "${HOME:-}"; do
        if [ -z "$candidate" ] || [ ! -d "$candidate" ]; then continue; fi
        candidate="$(cd "$candidate" && pwd -P)"
        if chain_safe "$candidate"; then POSTUN_BASE="$candidate"; break; fi
    done
    if [ -z "$POSTUN_BASE" ]; then
        echo "ERROR: no directory whose parent chain is owned by root or uid $(id -u)" >&2
        echo "       without group/other write; set COSH_RPM_TEST_SAFE_DIR. Checked:" >&2
        for candidate in "${COSH_RPM_TEST_SAFE_DIR:-}" "${HOME:-}"; do
            [ -n "$candidate" ] || continue
            d="$candidate"
            while [ -n "$d" ]; do
                stat -c '  %u %A %n' -- "$d" >&2 || break
                [ "$d" = / ] && break
                d="$(dirname -- "$d")"
            done
        done
        exit 1
    fi
    POSTUN_ROOT="$(mktemp -d "$POSTUN_BASE/.cosh-ng-rpm-postun.XXXXXX")"
    chmod 0755 "$POSTUN_ROOT"
    zombie_parent=""
    cleanup_matrix() {
        if [ -n "$zombie_parent" ]; then
            kill "$zombie_parent" 2>/dev/null || :
            wait "$zombie_parent" 2>/dev/null || :
        fi
        rm -rf "$TMP" "$POSTUN_ROOT"
    }
    trap cleanup_matrix EXIT
    POSTUN_ETC="$POSTUN_ROOT/etc"
    install -d -m 0755 "$POSTUN_ETC" "$POSTUN_ROOT/var/lib/rpm-state"
    POSTUN_SHELLS="$POSTUN_ETC/shells"
    REG="$POSTUN_ROOT/var/lib/rpm-state/cosh-ng"
    POSTUN_RAW="$(awk '/^%postun$/{f=1;next} /^%/{f=0} f' "$SPEC")"
    POSTTRANS_RAW="$(awk '/^%posttrans$/{f=1;next} /^%[a-z]/{f=0} f' "$SPEC")"

    # expand <raw scriptlet> <simulated sysconfdir>: resolve the macros the
    # scriptlets use against the fixture tree
    expand() {
        local s="${1//'%{_bindir}'/$STUB}"
        s="${s//'%{_sysconfdir}'/$2}"
        s="${s//'%{_localstatedir}'/$POSTUN_ROOT/var}"
        s="${s//%%/%}"
        case "$s" in
            *'%{_sysconfdir}'* | *'%{_bindir}'* | *'%{_localstatedir}'*)
                echo "ERROR: scriptlet still references an unexpanded macro" >&2
                exit 1
                ;;
        esac
        printf '%s\n' "$s"
    }
    POSTUN="$(expand "$POSTUN_RAW" "$POSTUN_ETC")"
    POSTTRANS="$(expand "$POSTTRANS_RAW" "$POSTUN_ETC")"

    # rpm hands a scriptlet to /bin/sh as a file. `bash -c "$POSTUN"` would let
    # this harness expand ${20} (the procfs offset) before the child runs and
    # silently zero the reclaim's owner checks, so always run from a file.
    run_script() {
        local body="$1" arg="$2" script
        script="$(mktemp "$TMP/scriptlet.XXXXXX")"
        printf '%s\n' "$body" > "$script"
        PATH="$STUB:/usr/bin:/bin" bash "$script" "$arg"
    }
    run_postun() { run_script "$POSTUN" "$1"; }
    run_posttrans() { run_script "$POSTTRANS" "$1"; }

    expect_postun_shells() {
        local name="$1"
        if ! cmp -s "$TMP/postun.expected" "$POSTUN_SHELLS"; then
            echo "ERROR: %postun case '$name' produced unexpected bytes:" >&2
            od -c "$POSTUN_SHELLS" >&2
            exit 1
        fi
    }
    # no stage directory and no registry entry may remain
    expect_no_stage() {
        if compgen -G "$1/.cosh-ng-shells.*" >/dev/null \
            || compgen -G "$1/shells.cosh-ng.*" >/dev/null \
            || compgen -G "$REG/stage.*" >/dev/null; then
            echo "ERROR: $2 left a stage or registry residue" >&2
            ls -la "$1" "$REG" >&2 2>/dev/null || :
            exit 1
        fi
    }
    # make_stage <dir> <token> [mode]: a stage in <dir> registered the same way
    # %postun registers its own
    make_stage() {
        local d="$1/.cosh-ng-shells.$2"
        [ -d "$REG" ] || install -d -m 0700 "$REG"
        install -d -m "${3:-0700}" "$d"
        printf '%s\n' "$d" > "$REG/stage.$2"
        STAGE="$d"
    }
    reset_stages() { rm -rf "$REG" "$POSTUN_ROOT"/*/.cosh-ng-shells.*; }

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
    expect_no_stage "$POSTUN_ETC" "successful %postun"
    if [ -e "$REG" ]; then
        echo "ERROR: successful %postun left the empty stage registry behind" >&2
        exit 1
    fi
    echo "PASS: successful %postun drops only cosh's line and leaves no stage or registry"

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

    # cp failure mid-rewrite: nonzero exit, /etc/shells untouched, nothing left
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
    expect_no_stage "$POSTUN_ETC" "%postun cp-failure"
    echo "PASS: %postun cp-failure exits nonzero, /etc/shells untouched, nothing left"

    # signal (TERM) mid-rewrite must NOT empty /etc/shells: the handler must
    # exit, not clean-and-resume into cp/mv. The awk wrapper TERMs our shell.
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
    expect_no_stage "$POSTUN_ETC" "%postun TERM-interrupt"
    echo "PASS: %postun TERM-interrupt leaves /etc/shells intact, exits nonzero, nothing left"

    # --- guard: the rewrite needs a parent chain only root/this uid can write ---
    # guard_case <name> <dir>: %postun against <dir>/shells must refuse with a
    # warning and leave the file and the registry untouched
    guard_case() {
        local name="$1" d="$2" body st=0
        printf '%s\n' /bin/sh "$STUB/cosh" > "$d/shells"
        cp "$d/shells" "$TMP/guard.before"
        body="$(expand "$POSTUN_RAW" "$d")"
        rm -f "$GUARD_COSH"
        run_script "$body" 0 >/dev/null 2>"$TMP/guard.err" || st=$?
        if [ "$st" -ne 0 ] || ! cmp -s "$TMP/guard.before" "$d/shells" \
            || ! grep -Fq "$3" "$TMP/guard.err"; then
            echo "ERROR: %postun guard case '$name' did not refuse cleanly (rc=$st)" >&2
            cat "$TMP/guard.err" >&2
            exit 1
        fi
        expect_no_stage "$d" "guard case '$name'"
    }
    # S2-A: a group-writable leaf; the rest of the chain passes, so only the
    # group-write bit is in play
    GUARD_ETC="$POSTUN_ROOT/guard-etc"
    install -d -m 0775 "$GUARD_ETC"
    guard_case "group-writable dir" "$GUARD_ETC" 'is not exclusively writable by root or uid'
    echo "PASS: %postun guard refuses a group-writable /etc/shells directory"

    # Foreign ownership: as root the fixtures are chown'd for real; without
    # root a stat wrapper reports uid 65534 for exactly one path, so the real
    # scriptlet predicate still runs on unprivileged CI.
    REAL_STAT="$(command -v stat)"
    make_foreign() {
        if [ "$(id -u)" -eq 0 ]; then
            chown 65534 "$1"
            FOREIGN_MODE="chown"
        else
            # shellcheck disable=SC2016  # $@/$last expand inside the wrapper
            printf '%s\n' '#!/usr/bin/env bash' \
                'for last; do :; done' \
                'if [ "$last" = "$COSH_TEST_FOREIGN_PATH" ]; then' \
                "  out=\"\$('$REAL_STAT' \"\$@\")\" || exit \$?" \
                '  printf "65534 %s\n" "${out#* }"' \
                'else' \
                "  exec '$REAL_STAT' \"\$@\"" \
                'fi' > "$STUB/stat"
            chmod 0755 "$STUB/stat"
            export COSH_TEST_FOREIGN_PATH="$1"
            FOREIGN_MODE="stat-stub"
        fi
    }
    clear_foreign() { rm -f "$STUB/stat"; unset COSH_TEST_FOREIGN_PATH; }

    # S2-A2: an ancestor owned by another uid, even without any write bit
    FOREIGN_ETC="$POSTUN_ROOT/foreign-etc"
    install -d -m 0755 "$FOREIGN_ETC"
    make_foreign "$FOREIGN_ETC"
    guard_case "foreign-owned dir" "$FOREIGN_ETC" 'is not exclusively writable by root or uid'
    clear_foreign
    echo "PASS: %postun guard refuses a foreign-owned ancestor ($FOREIGN_MODE)"

    # S2-A3: an unusable registry (symlink, wrong mode, foreign owner) makes
    # %postun refuse rather than create a stage nobody could find again
    REG_ALT="$POSTUN_ROOT/reg-alt"
    install -d -m 0700 "$REG_ALT"
    ln -s "$REG_ALT" "$REG"
    guard_case "symlinked registry" "$POSTUN_ETC" 'cannot use stage registry'
    rm -f "$REG"; install -d -m 0755 "$REG"
    guard_case "registry mode 0755" "$POSTUN_ETC" 'cannot use stage registry'
    rm -rf "$REG"; install -d -m 0700 "$REG"
    make_foreign "$REG"
    guard_case "foreign registry" "$POSTUN_ETC" 'cannot use stage registry'
    clear_foreign
    rm -rf "$REG"; install -d -m 0700 "$REG"
    chmod 0775 "$POSTUN_ROOT/var/lib/rpm-state"
    guard_case "group-writable registry parent" "$POSTUN_ETC" 'cannot use stage registry'
    chmod 0755 "$POSTUN_ROOT/var/lib/rpm-state"
    reset_stages
    echo "PASS: %postun refuses to stage without a private stage registry"

    # --- SIGKILL residue is registered and reclaimed by the next transaction ---
    # kill_postun <dir>: SIGKILL %postun (shells at <dir>/shells via the fixture
    # /etc/shells) in the middle of the rewrite and return the stage it left
    kill_postun() {
        local st=0
        reset_stages
        rm -f "$GUARD_COSH"
        cp "$POSTUN_SHELLS" "$TMP/postun.before-kill"
        # shellcheck disable=SC2016  # $PPID must stay literal in the written wrapper
        printf '%s\n' '#!/usr/bin/env bash' 'kill -KILL "$PPID"' > "$STUB/awk"
        chmod 0755 "$STUB/awk"
        run_postun 0 >/dev/null 2>&1 || st=$?
        rm -f "$STUB/awk"
        if [ "$st" -eq 0 ] || ! cmp -s "$TMP/postun.before-kill" "$POSTUN_SHELLS"; then
            echo "ERROR: SIGKILL'd %postun must fail without touching /etc/shells (rc=$st)" >&2
            exit 1
        fi
        if compgen -G "$1/shells.cosh-ng.*" >/dev/null; then
            echo "ERROR: SIGKILL'd %postun produced a pre-S2 sibling temp file" >&2
            exit 1
        fi
        local entries=( "$REG"/stage.* ) stages=( "$1"/.cosh-ng-shells.* )
        if [ "${#entries[@]}" -ne 1 ] || [ ! -f "${entries[0]}" ] \
            || [ "${#stages[@]}" -ne 1 ] || [ ! -d "${stages[0]}" ] \
            || [ "$(cat "${entries[0]}")" != "${stages[0]}" ]; then
            echo "ERROR: SIGKILL'd %postun must leave exactly one registered stage" >&2
            ls -la "$1" "$REG" >&2 2>/dev/null || :
            exit 1
        fi
        if [ "$(stat -c '%a %u' "${stages[0]}")" != "700 $(id -u)" ] \
            || ! grep -Fq 'cosh-ng-shells-stage v1' "${stages[0]}/owner"; then
            echo "ERROR: SIGKILL stage must be 0700, owned by uid $(id -u), with an owner record" >&2
            exit 1
        fi
        STAGE="${stages[0]}"
    }
    expect_reclaimed() {
        if [ -e "$1" ] || compgen -G "$REG/stage.*" >/dev/null || [ -e "$REG" ]; then
            echo "ERROR: %posttrans did not reclaim $1 and its registration ($2)" >&2
            ls -la "$(dirname -- "$1")" "$REG" >&2 2>/dev/null || :
            exit 1
        fi
    }
    printf '%s\n' /bin/sh "$STUB/cosh" /usr/bin/zsh > "$POSTUN_SHELLS"
    kill_postun "$POSTUN_ETC"
    run_posttrans 1 >/dev/null 2>&1
    expect_reclaimed "$STAGE" "plain"
    echo "PASS: SIGKILL residue is a registered 0700 stage, reclaimed by %posttrans"

    # S2-R6: a stage is registered before it exists. SIGKILL right after the
    # stage's mkdir must still leave a registration the next transaction
    # follows; creating first and registering second would orphan it.
    reset_stages
    printf '%s\n' /bin/sh "$STUB/cosh" > "$POSTUN_SHELLS"
    REAL_MKDIR="$(command -v mkdir)"
    # shellcheck disable=SC2016  # $@/$a/$PPID expand inside the wrapper
    printf '%s\n' '#!/usr/bin/env bash' \
        "'$REAL_MKDIR' \"\$@\" || exit \$?" \
        'for a; do case "$a" in */.cosh-ng-shells.*) kill -KILL "$PPID" ;; esac; done' \
        > "$STUB/mkdir"
    chmod 0755 "$STUB/mkdir"
    rm -f "$GUARD_COSH"
    run_postun 0 >/dev/null 2>&1 || :
    rm -f "$STUB/mkdir"
    if ! compgen -G "$POSTUN_ETC/.cosh-ng-shells.*" >/dev/null; then
        echo "ERROR: the mkdir-time SIGKILL fixture did not leave a stage" >&2
        exit 1
    fi
    run_posttrans 1 >/dev/null 2>&1
    expect_no_stage "$POSTUN_ETC" "SIGKILL right after the stage mkdir"
    echo "PASS: a stage killed right after creation is already registered and reclaimed"

    # S2-R1/R2: the admin repoints /etc/shells (or leaves it dangling) before
    # the next transaction; the registry still leads the reclaim to the stage
    SHELLS_A="$POSTUN_ROOT/etc-a"
    SHELLS_B="$POSTUN_ROOT/etc-b"
    install -d -m 0755 "$SHELLS_A" "$SHELLS_B"
    for next in "$SHELLS_B/shells" "$POSTUN_ROOT/nowhere/shells"; do
        printf '%s\n' /bin/sh "$STUB/cosh" > "$SHELLS_A/shells"
        printf '%s\n' /bin/sh > "$SHELLS_B/shells"
        rm -f "$POSTUN_SHELLS"; ln -s "$SHELLS_A/shells" "$POSTUN_SHELLS"
        kill_postun "$SHELLS_A"
        ln -sfn "$next" "$POSTUN_SHELLS"
        run_posttrans 1 >/dev/null 2>&1
        expect_reclaimed "$STAGE" "/etc/shells now -> $next"
    done
    rm -f "$POSTUN_SHELLS"
    echo "PASS: %posttrans reclaims a stage after /etc/shells is repointed or dangling"

    # S2-R3: a stage whose directory chain is no longer safe is kept, not
    # removed from under another user's reach
    printf '%s\n' /bin/sh "$STUB/cosh" > "$SHELLS_A/shells"
    ln -s "$SHELLS_A/shells" "$POSTUN_SHELLS"
    kill_postun "$SHELLS_A"
    chmod 0775 "$SHELLS_A"
    run_posttrans 1 >/dev/null 2>"$TMP/unsafe.err"
    chmod 0755 "$SHELLS_A"
    if [ ! -d "$STAGE" ] || ! compgen -G "$REG/stage.*" >/dev/null \
        || ! grep -Fq 'its directory is not exclusively writable' "$TMP/unsafe.err"; then
        echo "ERROR: reclaim touched a stage below an unsafe directory chain" >&2
        cat "$TMP/unsafe.err" >&2
        exit 1
    fi
    rm -f "$POSTUN_SHELLS"
    reset_stages
    echo "PASS: reclaim keeps a stage whose directory became writable by others"

    # S2-R4: registry entries the reclaim must clean up or refuse
    install -d -m 0700 "$REG"
    : > "$REG/stage.emptyxxxxx"                                   # never written
    printf '%s\n' "$POSTUN_ETC/.cosh-ng-shells.gonexxxxxx" > "$REG/stage.gonexxxxxx"
    PRECIOUS="$POSTUN_ROOT/precious"
    install -d -m 0700 "$PRECIOUS"; printf 'keep\n' > "$PRECIOUS/data"
    printf '%s\n' "$PRECIOUS" > "$REG/stage.forgedxxxx"           # not a stage path
    make_stage "$POSTUN_ETC" mismatchxx
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    printf '%s\n' "$STAGE" > "$REG/stage.otherxxxxx"              # token mismatch
    rm -f "$REG/stage.mismatchxx"
    # an unregistered, dead-looking stage is not ours to remove
    install -d -m 0700 "$POSTUN_ETC/.cosh-ng-shells.unregxxxxx"
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' \
        > "$POSTUN_ETC/.cosh-ng-shells.unregxxxxx/owner"
    run_posttrans 1 >/dev/null 2>"$TMP/entries.err"
    if [ -e "$REG/stage.emptyxxxxx" ] || [ -e "$REG/stage.gonexxxxxx" ]; then
        echo "ERROR: reclaim kept registry entries that reference nothing" >&2
        exit 1
    fi
    if [ ! -f "$REG/stage.forgedxxxx" ] || [ "$(cat "$PRECIOUS/data")" != keep ] \
        || [ ! -f "$REG/stage.otherxxxxx" ] || [ ! -d "$STAGE" ] \
        || [ ! -d "$POSTUN_ETC/.cosh-ng-shells.unregxxxxx" ] \
        || [ "$(grep -c 'kept unrecognised stage registry entry' "$TMP/entries.err")" -ne 2 ]; then
        echo "ERROR: reclaim acted on a forged, mismatched, or unregistered entry" >&2
        cat "$TMP/entries.err" >&2
        exit 1
    fi
    reset_stages
    echo "PASS: reclaim drops empty or stale entries and ignores forged or unregistered ones"

    # S2-R5: a symlinked registry is never followed by the reclaim
    install -d -m 0700 "$REG_ALT"
    ln -s "$REG_ALT" "$REG"
    install -d -m 0700 "$POSTUN_ETC/.cosh-ng-shells.viasymlink"
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' \
        > "$POSTUN_ETC/.cosh-ng-shells.viasymlink/owner"
    printf '%s\n' "$POSTUN_ETC/.cosh-ng-shells.viasymlink" > "$REG_ALT/stage.viasymlink"
    run_posttrans 1 >/dev/null 2>&1
    if [ ! -d "$POSTUN_ETC/.cosh-ng-shells.viasymlink" ] || [ ! -f "$REG_ALT/stage.viasymlink" ]; then
        echo "ERROR: reclaim followed a symlinked stage registry" >&2
        exit 1
    fi
    rm -f "$REG"; rm -rf "$REG_ALT"
    reset_stages
    echo "PASS: reclaim never follows a symlinked stage registry"

    # --- S2-D: reclaim spares every registered stage it cannot prove is ours and dead ---
    reset_stages
    # D1: well-formed fields but another tool's magic (dead pid, other boot)
    make_stage "$POSTUN_ETC" D1xxxxxxxx
    printf 'other-tool-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    # D2: owner is a symlink
    make_stage "$POSTUN_ETC" D2xxxxxxxx
    ln -s /etc/passwd "$STAGE/owner"
    # D3: extra files (unknown shape) with otherwise-valid owner magic
    make_stage "$POSTUN_ETC" D3xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    printf 'extra\n' > "$STAGE/extra"
    # D4: mode is not 0700
    make_stage "$POSTUN_ETC" D4xxxxxxxx 0755
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    # D5: live owner process (current shell): pid alive, starttime matches
    live_line="$(cat /proc/$$/stat)"
    # procfs stat has 52 fields; after `${line##*) }` strips pid+comm, the
    # starttime (field 22) becomes positional 20.
    # shellcheck disable=SC2086  # word-split the proc stat fields on purpose
    set -- ${live_line##*) }
    live_start="${20}"
    live_boot="$(cat /proc/sys/kernel/random/boot_id)"
    make_stage "$POSTUN_ETC" D5xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=%s pid=%s start=%s\n' \
        "$live_boot" "$$" "$live_start" > "$STAGE/owner"
    printf 'in-flight\n' > "$STAGE/shells"
    # D6: owner record with trailing extra fields (not a shape we write)
    make_stage "$POSTUN_ETC" D6xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0 extra=1\n' > "$STAGE/owner"
    # D7: shells present but owner is empty -- cosh-ng always writes the
    # owner record before any shells content, so this shape is foreign
    make_stage "$POSTUN_ETC" D7xxxxxxxx
    : > "$STAGE/owner"
    printf 'foreign\n' > "$STAGE/shells"
    # D8: a dead-owner stage owned by another uid (made foreign below)
    make_stage "$POSTUN_ETC" D8xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    # D9: owner fields not in boot=/pid=/start= form (not a record we write)
    make_stage "$POSTUN_ETC" D9xxxxxxxx
    printf 'cosh-ng-shells-stage v1 x=na pid=999999 start=0\n' > "$STAGE/owner"
    d_names="D1xxxxxxxx D2xxxxxxxx D3xxxxxxxx D4xxxxxxxx D5xxxxxxxx D6xxxxxxxx D7xxxxxxxx D8xxxxxxxx D9xxxxxxxx"
    # Snapshot each negative so a mutation that strips owner/shells and only
    # leaves the directory shell behind still turns the case red.
    for n in $d_names; do
        find "$POSTUN_ETC/.cosh-ng-shells.$n" -mindepth 1 -maxdepth 1 -printf '%f\n' \
            | LC_ALL=C sort > "$TMP/d-$n.before"
    done
    # P1: provably dead owner (other boot, missing pid)
    make_stage "$POSTUN_ETC" P1xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=na pid=999999 start=0\n' > "$STAGE/owner"
    printf 'old\n' > "$STAGE/shells"
    # P2: zombie owner (exited, not reaped). A non-reaping PID1 (e.g. a
    # `sleep infinity` container init) leaves SIGKILL'd scriptlets in Z state;
    # the reclaim must treat that as dead. The parent execs into a sleep so it
    # never reaps its child; cleanup_matrix kills it on any exit path.
    bash -c 'sleep 0.05 & echo $! > "$1"; exec sleep 300' zombie-parent "$TMP/zombie.pid" &
    zombie_parent=$!
    for _ in $(seq 1 50); do [ -s "$TMP/zombie.pid" ] && break; sleep 0.05; done
    zombie_pid="$(cat "$TMP/zombie.pid")"
    zombie_state=""
    for _ in $(seq 1 50); do
        zombie_state="$(awk '{print $3}' "/proc/$zombie_pid/stat" 2>/dev/null)"
        [ "$zombie_state" = Z ] && break
        sleep 0.05
    done
    if [ "$zombie_state" != Z ]; then
        echo "ERROR: could not create a zombie owner for the reclaim test (state=$zombie_state)" >&2
        exit 1
    fi
    zombie_line="$(cat "/proc/$zombie_pid/stat")"
    # shellcheck disable=SC2086  # word-split the proc stat fields on purpose
    set -- ${zombie_line##*) }
    make_stage "$POSTUN_ETC" P2xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot=%s pid=%s start=%s\n' \
        "$live_boot" "$zombie_pid" "${20}" > "$STAGE/owner"
    printf 'half-written\n' > "$STAGE/shells"
    # P3: empty owner and no shells (killed between mkdir and the owner write)
    make_stage "$POSTUN_ETC" P3xxxxxxxx
    : > "$STAGE/owner"
    # P4: bare empty stage (killed right after mkdir)
    make_stage "$POSTUN_ETC" P4xxxxxxxx
    # P5: unknown boot and the owner pid no longer exists: not running anywhere
    make_stage "$POSTUN_ETC" P5xxxxxxxx
    printf 'cosh-ng-shells-stage v1 boot= pid=999999 start=\n' > "$STAGE/owner"
    rm -f "$GUARD_COSH"
    make_foreign "$POSTUN_ETC/.cosh-ng-shells.D8xxxxxxxx"
    run_posttrans 1 >/dev/null 2>&1
    clear_foreign
    kill "$zombie_parent" 2>/dev/null || :
    wait "$zombie_parent" 2>/dev/null || :
    zombie_parent=""
    for n in $d_names; do
        d="$POSTUN_ETC/.cosh-ng-shells.$n"
        if [ ! -d "$d" ] || [ ! -f "$REG/stage.$n" ]; then
            echo "ERROR: reclaim removed non-matching stage $n or its registration" >&2
            exit 1
        fi
        if ! diff -u "$TMP/d-$n.before" \
            <(find "$d" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort) >&2; then
            echo "ERROR: reclaim partially deleted $n" >&2
            exit 1
        fi
    done
    for n in P1xxxxxxxx P2xxxxxxxx P3xxxxxxxx P4xxxxxxxx P5xxxxxxxx; do
        if [ -e "$POSTUN_ETC/.cosh-ng-shells.$n" ] || [ -e "$REG/stage.$n" ]; then
            echo "ERROR: reclaim failed to remove dead stage $n and its registration" >&2
            exit 1
        fi
    done
    reset_stages
    echo "PASS: reclaim removes only provably dead registered stages (incl. zombies)"

    # S2-E: an owner whose liveness cannot be verified is kept and reported,
    # never treated as exited. A cat wrapper makes one procfs path unreadable.
    REAL_CAT="$(command -v cat)"
    # shellcheck disable=SC2016  # $@/$a expand inside the wrapper
    printf '%s\n' '#!/usr/bin/env bash' \
        'for a; do [ "$a" = "$COSH_TEST_CAT_FAIL" ] && exit 1; done' \
        "exec '$REAL_CAT' \"\$@\"" > "$STUB/cat"
    chmod 0755 "$STUB/cat"
    expect_kept() {
        local name="$1" owner="$2" fail="$3" reason="$4"
        reset_stages
        make_stage "$POSTUN_ETC" "${name}xxxxxxxx"
        printf '%s\n' "$owner" > "$STAGE/owner"
        printf 'in-flight\n' > "$STAGE/shells"
        COSH_TEST_CAT_FAIL="$fail" run_posttrans 1 >/dev/null 2>"$TMP/kept.err"
        if [ ! -f "$STAGE/owner" ] || [ ! -f "$STAGE/shells" ] \
            || [ ! -f "$REG/stage.${name}xxxxxxxx" ]; then
            echo "ERROR: reclaim removed a stage whose owner is unverifiable ($name)" >&2
            exit 1
        fi
        if ! grep -Fq "$reason" "$TMP/kept.err"; then
            echo "ERROR: unverifiable stage $name kept without the expected warning" >&2
            cat "$TMP/kept.err" >&2
            exit 1
        fi
    }
    expect_kept E1 "cosh-ng-shells-stage v1 boot=$live_boot pid=$$ start=$live_start" \
        /proc/sys/kernel/random/boot_id 'boot_id is unreadable'
    expect_kept E2 "cosh-ng-shells-stage v1 boot=$live_boot pid=$$ start=$live_start" \
        "/proc/$$/stat" "/proc/$$/stat is unreadable"
    expect_kept E3 "cosh-ng-shells-stage v1 boot=$live_boot pid=$$ start=" \
        none 'owner identity is incomplete'
    expect_kept E4 "cosh-ng-shells-stage v1 boot= pid=$$ start=$live_start" \
        none 'owner identity is incomplete'
    expect_kept E5 "cosh-ng-shells-stage v1 boot=$live_boot pid=self start=$live_start" \
        none 'owner record has no usable pid'
    rm -f "$STUB/cat"
    reset_stages
    echo "PASS: reclaim keeps and reports stages whose owner liveness is unverifiable"

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
