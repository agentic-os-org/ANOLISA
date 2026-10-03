#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"
SPEC="$PROJECT_ROOT/deprecated/copilot-shell/copilot-shell.spec.in"

TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() {
    echo "not ok - $*" >&2
    return 1
}

pass() {
    echo "ok - $*"
}

# ---------------------------------------------------------------------------
# Shared helpers.
# ---------------------------------------------------------------------------

file_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

file_mode() {
    if stat -c %a "$1" >/dev/null 2>&1; then
        stat -c %a "$1"
    else
        stat -f %Lp "$1"
    fi
}

file_owner() {
    if stat -c %u:%g "$1" >/dev/null 2>&1; then
        stat -c %u:%g "$1"
    else
        stat -f %u:%g "$1" 2>/dev/null || true
    fi
}

assert_litter_free() {
    local found
    found="$(find "$1" -name '*.copilot-shell.*.tmp' 2>/dev/null || true)"
    [ -z "$found" ] || fail "$2: leftover temporary files: $found"
}

assert_same() {
    [ "$(file_hash "$1")" == "$(file_hash "$2")" ] \
        || fail "$3: $1 was modified (expected byte-identical to $2)"
}

count_lines() {
    grep -c "^$1\$" "$2" || true
}

# ---------------------------------------------------------------------------
# Extract the %post / %postun lua scriptlets from the spec.
# ---------------------------------------------------------------------------

# The guard is the first top-level `if` line of the %postun lua scriptlet.
GUARD="$(awk '/^%postun -p <lua>/{flag=1} flag && /^if /{print; exit}' "$SPEC" | sed 's/^if //')"
[ -n "$GUARD" ] || fail "could not locate the %postun lua guard in $SPEC"

# extract_scriptlet <directive>: prints the lua body between the directive
# (`%post -p <lua>` / `%postun -p <lua>`) and the next spec section.
extract_scriptlet() {
    awk -v dir="$1" '
        index($0, dir) == 1 { infn = 1; next }
        infn && /^%/ { infn = 0 }
        infn { print }
    ' "$SPEC"
}

# render_macros: expand the %{_sysconfdir}/%{_bindir} macros of a scriptlet
# body exactly as rpmbuild would before the lua interpreter sees it.
render_macros() {
    if command -v rpm >/dev/null 2>&1; then
        rpm --eval "$1"
    else
        printf '%s\n' "$1" | sed -e 's|%{_sysconfdir}|/etc|g' -e 's|%{_bindir}|/usr/bin|g'
    fi
}

# ---------------------------------------------------------------------------
# Static contract: the erase count lives at arg[2] (arg[1] is the "<lua>"
# interpreter token), and the comparison must survive both the string-typed
# arguments of rpm <= 4.17 and the number-coerced arguments of newer rpm.
# ---------------------------------------------------------------------------

test_guard_uses_arg2() {
    grep -q 'arg\[2\]' <<<"$GUARD" \
        || fail "guard '$GUARD' does not read the scriptlet count from arg[2]"
    if grep -q 'arg\[1\]' <<<"$GUARD"; then
        fail "guard '$GUARD' reads arg[1], which rpm fills with the '<lua>' interpreter token"
    fi
    grep -Eq 'tonumber\(' <<<"$GUARD" \
        || fail "guard '$GUARD' compares arg[2] directly; string-typed args (rpm <= 4.17) never equal number 0 in lua"
    pass "guard reads tonumber(arg[2]) and never arg[1]: $GUARD"
}

# ---------------------------------------------------------------------------
# Dynamic regression: evaluate the real guard expression under the four
# argument layouts an rpm lua %postun can see. Verified empirically against
# rpm 4.17.0 (arg = {"<lua>", "0"} on final erase) and against rpm master,
# which prepends `arg[2] = tonumber(arg[2]);` to the scriptlet body
# (lib/rpmscript.cc, runLuaScript()).
# ---------------------------------------------------------------------------

# Engines in preference order: rpm's embedded lua first (the exact
# interpreter class the scriptlet runs under), then standalone interpreters.
find_engine() {
    if command -v rpm >/dev/null 2>&1 && rpm --eval '%{lua: print(1)}' >/dev/null 2>&1; then
        echo "rpm"
    else
        local e
        for e in lua5.4 lua5.3 lua luajit; do
            if command -v "$e" >/dev/null 2>&1; then
                echo "$e"
                return 0
            fi
        done
        echo ""
    fi
}

engine_version() {
    case "$ENGINE" in
        rpm) rpm --eval '%{lua: print(_VERSION)}' 2>/dev/null | tr -d '\n' ;;
        *) "$ENGINE" -e 'print(_VERSION)' 2>/dev/null ;;
    esac
}

# engine_supports <lua-expr>: 0 when the engine evaluates the expression true.
engine_supports() {
    local result
    case "$ENGINE" in
        rpm) result="$(rpm --eval "%{lua: if ${1} then print(1) else print(0) end}" | tr -d '\n')" ;;
        *) result="$(printf 'if %s then print(1) else print(0) end\n' "$1" | "$ENGINE" - | tr -d '\n')" ;;
    esac
    [ "$result" == "1" ]
}

# run_guard <arg2-lua-expr> <raw-expression> ; prints FIRED or SKIPPED
run_guard() {
    local arg2_expr="$1" expr="$2"
    case "$ENGINE" in
        rpm)
            rpm --eval "%{lua: arg = {}; arg[1] = '<lua>'; arg[2] = ${arg2_expr}; if ${expr} then print('FIRED') else print('SKIPPED') end}" \
                | tr -d '\n'
            ;;
        *)
            printf 'arg = {}\narg[1] = %s\narg[2] = %s\nif %s then print("FIRED") else print("SKIPPED") end\n' \
                "'<lua>'" "$arg2_expr" "$expr" | "$ENGINE" -
            ;;
    esac
}

assert_guard() {
    local arg2_expr="$1" expected="$2" label="$3" got
    got="$(run_guard "$arg2_expr" "$GUARD")"
    [ "$got" == "$expected" ] \
        || fail "${label}: guard '${GUARD}' with arg[2]=${arg2_expr} returned ${got}, expected ${expected}"
    pass "${label}: ${got}"
}

test_guard_dynamic() {
    if [ -z "$ENGINE" ]; then
        echo "SKIP - no lua engine (rpm w/ lua, lua5.4, lua5.3, lua, luajit) on this host; static checks only" >&2
        return 0
    fi
    assert_guard "'0'"  FIRED   "final erase on rpm <= 4.17 (string \"0\")"
    assert_guard "0"    FIRED   "final erase on newer rpm (number 0 after upstream tonumber coercion)"
    assert_guard "'1'"  SKIPPED "upgrade on rpm <= 4.17 (string \"1\", old %postun keeps /etc/shells)"
    assert_guard "1"    SKIPPED "upgrade on newer rpm (number 1)"
}

# Documents why the raw numeric comparison is not enough: on string-typed
# rpm (<= 4.17) `arg[2] == 0` is "0" == 0, which is false in lua.
test_raw_comparison_documented() {
    if [ -z "$ENGINE" ]; then
        return 0
    fi
    local got
    got="$(run_guard "'0'" 'arg[2] == 0')"
    [ "$got" == "SKIPPED" ] \
        || fail "raw 'arg[2] == 0' unexpectedly fired under string layout; tonumber() rationale is stale"
    pass "raw arg[2] == 0 stays dead under string-typed rpm layouts (why tonumber is required)"
}

# ---------------------------------------------------------------------------
# Full-scriptlet regression: execute the whole rendered %postun body (not
# just the guard expression) against a sandboxed fake root, for every path
# the cleanup can take:
#   - final erase         (count 0, no /usr/bin/cosh)      -> line dropped,
#     surviving lines keep their order, mode/ownership preserved, no litter
#   - upgrade             (count 1)                        -> byte-identical
#   - replacement-present (count 0, /usr/bin/cosh still there, i.e. the
#     `yum swap copilot-shell cosh-ng` ordering where cosh-ng installs
#     first and re-owns the executable)                     -> byte-identical
#   - write failure       (injected or unwritable dir)     -> scriptlet
#     fails loudly, /etc/shells untouched, no temp litter
# plus the admin-managed symlink and missing-/etc/shells edge cases.
# ---------------------------------------------------------------------------

POSTUN_BODY="$(render_macros "$(extract_scriptlet '%postun -p <lua>')")"
[ -n "$POSTUN_BODY" ] || fail "could not extract the %postun lua body from $SPEC"

SANDBOX="$TEST_TMP/sandbox"
# Tier-3 seeds: the real-rpm tier runs un-substituted scriptlets inside an
# rpm --root, so its /etc/shells carries the literal registration line.
SEED_PRE_INSTALL="$TEST_TMP/seed-pre"    # table before copilot-shell exists
printf '/bin/sh\n/bin/bash\n/sbin/nologin\n' > "$SEED_PRE_INSTALL"

# new_sandbox: build the fake root and the installed-state /etc/shells. The
# sandboxed body sees substituted paths, so the registered cosh line is the
# SANDBOX's /usr/bin/cosh, exactly like %post would have written it there.
new_sandbox() {
    rm -rf "$SANDBOX"
    mkdir -p "$SANDBOX/etc" "$SANDBOX/usr/bin"
    printf '/bin/sh\n%s/usr/bin/cosh\n/bin/bash\n/sbin/nologin\n' "$SANDBOX" \
        > "$SANDBOX/etc/shells"
    cp "$SANDBOX/etc/shells" "$SANDBOX/seed-installed"
    printf '/bin/sh\n/bin/bash\n/sbin/nologin\n' > "$SANDBOX/seed-pre"
}

# sandbox_body: point every /etc/shells and /usr/bin/cosh reference of the
# rendered body at the sandbox fake root.
sandbox_body() {
    sed -e "s|/etc/shells|$SANDBOX/etc/shells|g" -e "s|/usr/bin/cosh|$SANDBOX/usr/bin/cosh|g" \
        <<<"$POSTUN_BODY" > "$SANDBOX/postun.lua"
}

# Body runner: executes the extracted %postun body with an explicit
# environment whose arg table mirrors the rpm lua scriptlet layout
# (arg[1] = "<lua>" interpreter token, arg[2] = scriptlet count, string-typed
# on rpm <= 4.17, number-coerced on newer rpm). The body must NOT be run via
# plain dofile under rpm's %{lua:}: each macro chunk gets a private sandbox
# environment, so an `arg = {...}` assignment in the macro chunk never
# reaches a dofile-loaded chunk (which is compiled against the registry
# globals); load()-ing the body with our own env is the layout-faithful way.
write_runner() {
    cat > "$SANDBOX/runner.lua" <<'LUAEOF'
local body_path = assert(os.getenv('BODY_PATH'))
local arg2 = assert(os.getenv('ARG2'))
local env = setmetatable({ arg = {'<lua>', arg2} }, { __index = _G })
if os.getenv('ARG2_ISNUM') == '1' then
    env.arg[2] = tonumber(env.arg[2])
end
local bf = assert(io.open(body_path, 'rb'))
local src = bf:read('*a')
bf:close()
local chunk, cerr
if _VERSION == 'Lua 5.1' or (jit and jit.version) then
    chunk, cerr = loadstring(src)
    if chunk then setfenv(chunk, env) end
else
    chunk, cerr = load(src, 'postun-body', 't', env)
end
if not chunk then error(tostring(cerr)) end
local ok, rerr = pcall(chunk)
if ok then print('BODY-OK') else print('BODY-FAIL: ' .. tostring(rerr)) end
LUAEOF
}

# mark_rc <process-rc>: the runner signals the scriptlet status with a
# BODY-OK / BODY-FAIL marker line (os.exit is unavailable in rpm's %{lua:}).
mark_rc() {
    if [ "$1" -eq 0 ] && grep -q '^BODY-OK' "$SANDBOX/out.log"; then
        RUN_RC=0
    else
        RUN_RC=1
    fi
}

# run_body <arg2-layout>: execute the sandboxed body under the engine;
# <arg2-layout> is `string-0`, `number-0`, `string-1` or `number-1`.
run_body() {
    local rc
    write_runner
    case "$ENGINE" in
        rpm)
            if BODY_PATH="$SANDBOX/postun.lua" ARG2="${1#*-}" \
               ARG2_ISNUM="$([ "${1%%-*}" == number ] && echo 1 || echo 0)" \
               rpm --eval "%{lua: dofile('${SANDBOX}/runner.lua') }" \
               >"$SANDBOX/out.log" 2>"$SANDBOX/err.log"; then
                rc=0
            else
                rc=1
            fi
            ;;
        *)
            if BODY_PATH="$SANDBOX/postun.lua" ARG2="${1#*-}" \
               ARG2_ISNUM="$([ "${1%%-*}" == number ] && echo 1 || echo 0)" \
               "$ENGINE" "$SANDBOX/runner.lua" >"$SANDBOX/out.log" 2>"$SANDBOX/err.log"; then
                rc=0
            else
                rc=1
            fi
            ;;
    esac
    mark_rc "$rc"
}

# Fault-injection runner: executes the body under load(chunk, ..., env) with
# a doctored io whose Nth write to any write-mode handle fails, proving the
# rewrite cannot truncate /etc/shells in place. Requires Lua 5.2+ load-env.
write_injector() {
    cat > "$SANDBOX/injector.lua" <<'LUAEOF'
local body_path = assert(os.getenv('BODY_PATH'))
local fail_on = assert(tonumber(os.getenv('INJECT_FAIL_ON')))
local arg2 = assert(os.getenv('ARG2'))
if os.getenv('ARG2_ISNUM') == '1' then
    arg2 = tonumber(arg2)
end
local realio, realos = io, os

local writes = 0
local env = {
    arg = {'<lua>', arg2},
    tonumber = tonumber, tostring = tostring, type = type, error = error,
    pairs = pairs, ipairs = ipairs, select = select, pcall = pcall,
    table = table, string = string, math = math,
    os = {
        time = realos.time, remove = realos.remove, rename = realos.rename,
        getenv = realos.getenv, date = realos.date, clock = realos.clock,
    },
    io = nil,
}

local function doctored_io()
    local t = {}
    for k, v in pairs(realio) do t[k] = v end
    local realopen = t.open
    local function open(path, mode)
        local f = realopen(path, mode)
        if f and type(mode) == 'string' and mode:find('w', 1, true) then
            local proxy
            proxy = {
                write = function(_, ...)
                    writes = writes + 1
                    if writes >= fail_on then return nil, 'injected write failure' end
                    return f:write(...)
                end,
                close = function(_) return f:close() end,
                lines = function(_, ...) return f:lines(...) end,
                read = function(_, ...) return f:read(...) end,
                seek = function(_, ...) return f:seek(...) end,
                flush = function(_) return f:flush() end,
            }
            return proxy
        end
        return f
    end
    t.open = open
    t.stderr = { write = function(_, ...) return realio.stderr:write(...) end }
    return t
end
env.io = doctored_io()

local bf = assert(realio.open(body_path, 'rb'))
local src = bf:read('*a')
bf:close()
local chunk, cerr
if _VERSION == 'Lua 5.1' or (jit and jit.version) then
    chunk, cerr = loadstring(src)
    if chunk then setfenv(chunk, env) end
else
    chunk, cerr = load(src, 'postun-body', 't', env)
end
if not chunk then error(tostring(cerr)) end
local ok, rerr = pcall(chunk)
if ok then print('BODY-OK') else print('BODY-FAIL: ' .. tostring(rerr)) end
LUAEOF
}

# run_body_injected <arg2-layout> <fail-on-nth-write>
run_body_injected() {
    local rc
    write_injector
    case "$ENGINE" in
        rpm)
            if BODY_PATH="$SANDBOX/postun.lua" INJECT_FAIL_ON="$2" ARG2="${1#*-}" \
               ARG2_ISNUM="$([ "${1%%-*}" == number ] && echo 1 || echo 0)" \
               rpm --eval "%{lua: dofile('${SANDBOX}/injector.lua') }" \
               >"$SANDBOX/out.log" 2>"$SANDBOX/err.log"; then
                rc=0
            else
                rc=1
            fi
            ;;
        *)
            if BODY_PATH="$SANDBOX/postun.lua" INJECT_FAIL_ON="$2" ARG2="${1#*-}" \
               ARG2_ISNUM="$([ "${1%%-*}" == number ] && echo 1 || echo 0)" \
               "$ENGINE" "$SANDBOX/injector.lua" >"$SANDBOX/out.log" 2>"$SANDBOX/err.log"; then
                rc=0
            else
                rc=1
            fi
            ;;
    esac
    mark_rc "$rc"
}

test_scriptlet_final_erase() {
    local layout label
    for layout in string-0 number-0; do
        label="final erase (arg[2]=${layout#*-} as ${layout%%-*})"
        new_sandbox
        sandbox_body
        run_body "$layout"
        [ "$RUN_RC" -eq 0 ] || fail "$label: scriptlet failed: $(cat "$SANDBOX/err.log")"
        diff -u "$SANDBOX/seed-pre" "$SANDBOX/etc/shells" >/dev/null \
            || fail "$label: /etc/shells after erase is not the seeded table minus the cosh line: $(cat "$SANDBOX/etc/shells")"
        assert_litter_free "$SANDBOX/etc" "$label"
    done
    pass "final erase: drops only the cosh line, order preserved, no temp litter, rc=0"
}

test_scriptlet_mode_preserved() {
    if ! engine_supports 'posix and posix.stat and posix.chmod'; then
        echo "SKIP - mode preservation needs rpm's embedded lua (posix extension); engine has none" >&2
        return 0
    fi
    new_sandbox
    chmod 0640 "$SANDBOX/etc/shells"
    local mode owner
    mode="$(file_mode "$SANDBOX/etc/shells")"
    owner="$(file_owner "$SANDBOX/etc/shells")"
    sandbox_body
    run_body string-0
    [ "$RUN_RC" -eq 0 ] || fail "mode preservation: scriptlet failed: $(cat "$SANDBOX/err.log")"
    [ "$(file_mode "$SANDBOX/etc/shells")" == "$mode" ] \
        || fail "mode preservation: mode changed $(file_mode "$SANDBOX/etc/shells") != $mode"
    if [ -n "$owner" ]; then
        [ "$(file_owner "$SANDBOX/etc/shells")" == "$owner" ] \
            || fail "mode preservation: ownership changed $(file_owner "$SANDBOX/etc/shells") != $owner"
    fi
    pass "final erase: mode/ownership carried over to the rewritten file (mode $mode)"
}

test_scriptlet_upgrade() {
    local layout label
    for layout in string-1 number-1; do
        label="upgrade (arg[2]=${layout#*-} as ${layout%%-*})"
        new_sandbox
        sandbox_body
        run_body "$layout"
        [ "$RUN_RC" -eq 0 ] || fail "$label: scriptlet failed: $(cat "$SANDBOX/err.log")"
        assert_same "$SANDBOX/etc/shells" "$SANDBOX/seed-installed" "$label: /etc/shells"
        assert_litter_free "$SANDBOX/etc" "$label"
    done
    pass "upgrade: count 1 keeps /etc/shells byte-identical, no temp litter"
}

test_scriptlet_replacement_present() {
    new_sandbox
    # The `yum swap copilot-shell cosh-ng` ordering: the replacement installs
    # (and re-owns /usr/bin/cosh) before the old package's final-erase %postun
    # runs, so the registration is still valid and must survive.
    printf '#!/bin/sh\n' > "$SANDBOX/usr/bin/cosh"
    chmod 0755 "$SANDBOX/usr/bin/cosh"
    sandbox_body
    run_body string-0
    [ "$RUN_RC" -eq 0 ] || fail "replacement-present: scriptlet failed: $(cat "$SANDBOX/err.log")"
    assert_same "$SANDBOX/etc/shells" "$SANDBOX/seed-installed" "replacement-present: /etc/shells"
    assert_litter_free "$SANDBOX/etc" "replacement-present"
    pass "replacement-present: cosh-ng-style swap keeps the still-valid /usr/bin/cosh registration"
}

test_scriptlet_missing_shells() {
    new_sandbox
    rm -f "$SANDBOX/etc/shells"
    sandbox_body
    run_body string-0
    [ "$RUN_RC" -eq 0 ] || fail "missing-shells: scriptlet failed: $(cat "$SANDBOX/err.log")"
    [ ! -e "$SANDBOX/etc/shells" ] || fail "missing-shells: scriptlet created /etc/shells"
    pass "missing /etc/shells: cleanup is a clean no-op"
}

test_scriptlet_symlinked_shells() {
    if ! engine_supports 'posix and posix.readlink'; then
        echo "SKIP - symlink handling needs rpm's embedded lua (posix extension); engine has none" >&2
        return 0
    fi
    new_sandbox
    mv "$SANDBOX/etc/shells" "$SANDBOX/etc/shells.real"
    ln -s shells.real "$SANDBOX/etc/shells"
    sandbox_body
    run_body string-0
    [ "$RUN_RC" -eq 0 ] || fail "symlinked-shells: scriptlet failed: $(cat "$SANDBOX/err.log")"
    [ -L "$SANDBOX/etc/shells" ] || fail "symlinked-shells: /etc/shells is no longer a symlink"
    if grep -q "^${SANDBOX}/usr/bin/cosh$" "$SANDBOX/etc/shells"; then
        fail "symlinked-shells: cosh line survived in the rewritten target"
    fi
    grep -q '^/bin/sh$' "$SANDBOX/etc/shells" || fail "symlinked-shells: seeded lines lost"
    assert_litter_free "$SANDBOX/etc" "symlinked-shells"
    pass "symlinked /etc/shells: link survives, its target is rewritten"
}

test_scriptlet_write_failure_injected() {
    local fail_on
    case "$(engine_version)" in
        *"Lua 5.1"*|*"LuaJIT"*)
            echo "SKIP - write-failure injection needs load() with a custom env (Lua 5.2+); engine is $(engine_version)" >&2
            return 0
            ;;
    esac
    for fail_on in 1 3; do
        new_sandbox
        sandbox_body
        run_body_injected string-0 "$fail_on"
        [ "$RUN_RC" -ne 0 ] || fail "write-failure(${fail_on}): scriptlet reported success despite injected write failure"
        grep -q 'BODY-FAIL' "$SANDBOX/out.log" \
            || fail "write-failure(${fail_on}): scriptlet body did not signal the failure"
        assert_same "$SANDBOX/etc/shells" "$SANDBOX/seed-installed" "write-failure(${fail_on}): /etc/shells"
        assert_litter_free "$SANDBOX/etc" "write-failure(${fail_on})"
    done
    pass "write failure (1st and 3rd write): scriptlet fails loudly, /etc/shells intact, no litter"
}

test_scriptlet_write_failure_readonly() {
    if [ "$(id -u)" -eq 0 ]; then
        echo "SKIP - read-only-dir failure injection is bypassed by root; run as a regular user" >&2
        return 0
    fi
    new_sandbox
    sandbox_body
    chmod 0555 "$SANDBOX/etc"
    run_body string-0
    local rc=$RUN_RC
    chmod 0755 "$SANDBOX/etc"
    [ "$rc" -ne 0 ] || fail "write-failure(readonly dir): scriptlet reported success despite unwritable directory"
    assert_same "$SANDBOX/etc/shells" "$SANDBOX/seed-installed" "write-failure(readonly dir): /etc/shells"
    assert_litter_free "$SANDBOX/etc" "write-failure(readonly dir)"
    pass "write failure (unwritable directory): scriptlet fails loudly, /etc/shells intact"
}

# ---------------------------------------------------------------------------
# Real-rpm regression: embed the exact rendered %post/%postun of the spec in
# probe packages and drive install / erase / upgrade / replacement / failure
# transactions against an isolated --root, the way `yum swap copilot-shell
# cosh-ng` drives the real scriptlet. Inside a --root every scriptlet path
# resolves within the fake root, so the host /etc/shells is never touched.
# ---------------------------------------------------------------------------

tier3_enabled() {
    command -v rpmbuild >/dev/null 2>&1 && [ "$ENGINE" == "rpm" ]
}

build_probe_rpms() {
    local top="$1" rel
    mkdir -p "$top"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    for rel in 1 2; do
        {
            printf 'Name: copostun-probe\n'
            printf 'Version: 1.0\n'
            printf 'Release: %s\n' "$rel"
            printf 'Summary: copilot-shell scriptlet probe\n'
            printf 'License: Apache-2.0\n'
            printf 'BuildArch: noarch\n'
            printf '%%description\n'
            printf 'Embeds the exact rendered copilot-shell %%post/%%postun for tests/test-cosh-postun-guard.sh.\n'
            printf '%%install\n'
            printf 'install -d %%{buildroot}%%{_datadir}/copostun-probe\n'
            printf 'echo probe > %%{buildroot}%%{_datadir}/copostun-probe/mark\n'
            printf '%%files\n'
            printf '%%{_datadir}/copostun-probe/mark\n'
            printf '%%post -p <lua>\n'
            printf '%s\n' "$(render_macros "$(extract_scriptlet '%post -p <lua>')")"
            printf '%%postun -p <lua>\n'
            printf '%s\n' "$POSTUN_BODY"
        } > "$top/SPECS/probe-${rel}.spec"
        rpmbuild -bb --define "_topdir $top" "$top/SPECS/probe-${rel}.spec" \
            >"$top/build-${rel}.log" 2>&1 \
            || fail "tier3: rpmbuild failed for probe-${rel}: $(tail -5 "$top/build-${rel}.log")"
    done
    {
        printf 'Name: copostun-provider\n'
        printf 'Version: 1.0\n'
        printf 'Release: 1\n'
        printf 'Summary: replacement /usr/bin/cosh provider probe\n'
        printf 'License: Apache-2.0\n'
        printf 'BuildArch: noarch\n'
        printf '%%description\n'
        printf 'Stands in for cosh-ng during the yum-swap replacement probe.\n'
        printf '%%install\n'
        printf 'install -d %%{buildroot}%%{_bindir}\n'
        printf 'printf "#!/bin/sh\\\\n" > %%{buildroot}%%{_bindir}/cosh\n'
        printf 'chmod 0755 %%{buildroot}%%{_bindir}/cosh\n'
        printf '%%files\n'
        printf '%%{_bindir}/cosh\n'
    } > "$top/SPECS/provider.spec"
    rpmbuild -bb --define "_topdir $top" "$top/SPECS/provider.spec" \
        >"$top/build-provider.log" 2>&1 \
        || fail "tier3: rpmbuild failed for provider: $(tail -5 "$top/build-provider.log")"
}

new_rpmroot() {
    local root="$1"
    rm -rf "$root"
    mkdir -p "$root"/{etc,usr/bin,var/lib/rpm,tmp,run}
    cp "$SEED_PRE_INSTALL" "$root/etc/shells"
    rpm --root "$root" --initdb >/dev/null 2>&1 \
        || fail "tier3: rpm --initdb failed for $root"
}

probe_rpm() {
    local rpm_file
    rpm_file="$(find "$TIER3_TOP/RPMS" -name "copostun-probe-1.0-$1.*.rpm" | head -1)"
    [ -n "$rpm_file" ] || fail "tier3: probe rpm (release $1) not found in $TIER3_TOP/RPMS"
    printf '%s' "$rpm_file"
}

provider_rpm() {
    local rpm_file
    rpm_file="$(find "$TIER3_TOP/RPMS" -name 'copostun-provider-1.0-1.*.rpm' | head -1)"
    [ -n "$rpm_file" ] || fail "tier3: provider rpm not found in $TIER3_TOP/RPMS"
    printf '%s' "$rpm_file"
}

rpm_install() {
    rpm --root "$1" -ivh --nodeps "$2" >"$3" 2>&1 \
        || fail "tier3: rpm -ivh failed: $(tail -5 "$3")"
}

rpm_erase() {
    rpm --root "$1" -e "$2" >"$3" 2>&1 \
        || fail "tier3: rpm -e failed: $(tail -5 "$3")"
}

test_rpm_final_erase() {
    local root="$TIER3_TOP/root-erase"
    new_rpmroot "$root"
    rpm_install "$root" "$(probe_rpm 1)" "$root/install.log"
    grep -q '^/usr/bin/cosh$' "$root/etc/shells" \
        || fail "tier3 final-erase: %post did not register /usr/bin/cosh"
    rpm_erase "$root" copostun-probe "$root/erase.log"
    [ -e "$root/etc/shells" ] || fail "tier3 final-erase: /etc/shells vanished entirely"
    if grep -q '^/usr/bin/cosh$' "$root/etc/shells"; then
        fail "tier3 final-erase: final erase left the cosh line in /etc/shells"
    fi
    diff -u "$SEED_PRE_INSTALL" "$root/etc/shells" >/dev/null \
        || fail "tier3 final-erase: seeded lines changed by the transaction"
    assert_litter_free "$root/etc" "tier3 final-erase"
    pass "rpm final erase: real transaction drops the cosh line, keeps the rest, no litter"
}

test_rpm_upgrade() {
    local root="$TIER3_TOP/root-upgrade"
    new_rpmroot "$root"
    rpm_install "$root" "$(probe_rpm 1)" "$root/install.log"
    local installed count
    count="$(count_lines '/usr/bin/cosh' "$root/etc/shells")"
    [ "$count" -eq 1 ] || fail "tier3 upgrade: %post registered the line ${count} times (want exactly 1)"
    installed="$(file_hash "$root/etc/shells")"
    rpm --root "$root" -Uvh --nodeps "$(probe_rpm 2)" >"$root/upgrade.log" 2>&1 \
        || fail "tier3 upgrade: rpm -Uvh failed: $(tail -5 "$root/upgrade.log")"
    [ "$(file_hash "$root/etc/shells")" == "$installed" ] \
        || fail "tier3 upgrade: /etc/shells changed across the upgrade"
    assert_litter_free "$root/etc" "tier3 upgrade"
    pass "rpm upgrade: count-1 %postun keeps /etc/shells byte-identical across rpm -Uvh"
}

test_rpm_replacement_present() {
    local root="$TIER3_TOP/root-replacement"
    new_rpmroot "$root"
    # yum swap ordering: the replacement lands first, then the old package is
    # erased; the old package's final-erase %postun must keep the shared line.
    rpm_install "$root" "$(provider_rpm)" "$root/provider.log"
    [ -x "$root/usr/bin/cosh" ] || fail "tier3 replacement: provider did not install /usr/bin/cosh"
    rpm_install "$root" "$(probe_rpm 1)" "$root/install.log"
    local before_erase
    before_erase="$(file_hash "$root/etc/shells")"
    rpm_erase "$root" copostun-probe "$root/erase.log"
    grep -q '^/usr/bin/cosh$' "$root/etc/shells" \
        || fail "tier3 replacement: yum-swap erase dropped the still-valid cosh line"
    [ "$(file_hash "$root/etc/shells")" == "$before_erase" ] \
        || fail "tier3 replacement: yum-swap erase modified /etc/shells"
    assert_litter_free "$root/etc" "tier3 replacement"
    pass "rpm replacement-present: swap-style erase keeps the replacement's registration"
}

test_rpm_write_failure() {
    local root="$TIER3_TOP/root-writefail" before_erase erase_rc
    # rpm scriptlets run privileged on most hosts, so directory mode bits
    # cannot block them; an immutable directory does. That needs sudo+chattr;
    # without them the write-failure path stays covered by the tier-1 fault
    # injection under the same lua engine.
    if ! command -v sudo >/dev/null 2>&1 || ! sudo -n true >/dev/null 2>&1; then
        echo "SKIP - real-rpm write-failure injection needs passwordless sudo + chattr (immutable directory); tier-1 fault injection covers the path" >&2
        return 0
    fi
    new_rpmroot "$root"
    rpm_install "$root" "$(probe_rpm 1)" "$root/install.log"
    before_erase="$(file_hash "$root/etc/shells")"
    if ! sudo -n chattr +i "$root/etc" >/dev/null 2>&1; then
        echo "SKIP - chattr +i is unavailable on this filesystem; tier-1 fault injection covers the path" >&2
        return 0
    fi
    rpm --root "$root" -e copostun-probe >"$root/erase.log" 2>&1
    erase_rc=$?
    sudo -n chattr -i "$root/etc"
    grep -qi 'cannot update' "$root/erase.log" \
        || fail "tier3 write-failure: erase transcript lacks the scriptlet error: $(cat "$root/erase.log")"
    [ "$(file_hash "$root/etc/shells")" == "$before_erase" ] \
        || fail "tier3 write-failure: /etc/shells damaged by the failing scriptlet"
    assert_litter_free "$root/etc" "tier3 write-failure"
    pass "rpm write failure: scriptlet error surfaced (rpm rc=${erase_rc}), /etc/shells intact, no litter"
}

test_rpm_transactions() {
    if ! tier3_enabled; then
        echo "SKIP - real-rpm tier needs rpmbuild and rpm's embedded lua on this host" >&2
        return 0
    fi
    TIER3_TOP="$TEST_TMP/tier3"
    build_probe_rpms "$TIER3_TOP"
    test_rpm_final_erase
    test_rpm_upgrade
    test_rpm_replacement_present
    test_rpm_write_failure
}

# ---------------------------------------------------------------------------
# Suite.
# ---------------------------------------------------------------------------

ENGINE="$(find_engine)"

test_guard_uses_arg2
test_guard_dynamic
test_raw_comparison_documented

if [ -n "$ENGINE" ]; then
    test_scriptlet_final_erase
    test_scriptlet_mode_preserved
    test_scriptlet_upgrade
    test_scriptlet_replacement_present
    test_scriptlet_missing_shells
    test_scriptlet_symlinked_shells
    test_scriptlet_write_failure_injected
    test_scriptlet_write_failure_readonly
    test_rpm_transactions
else
    echo "SKIP - full-scriptlet tiers need a lua engine (rpm w/ lua preferred); static checks only" >&2
fi

echo "==> All copilot-shell postun guard tests passed!"
