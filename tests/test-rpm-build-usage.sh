#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"
SCRIPT="$PROJECT_ROOT/scripts/rpm-build.sh"

TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() {
    echo "not ok - $*" >&2
    return 1
}

pass() {
    echo "ok - $*"
}

assert_contains() {
    local file="$1" expected="$2"
    grep -Fq -- "$expected" "$file" || fail "expected '$expected' in $file"
}

assert_not_contains() {
    local file="$1" unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        fail "did not expect '$unexpected' in $file"
    fi
}

# ---------------------------------------------------------------------------
# Static contract: usage() must advertise exactly the dispatch targets.
# ---------------------------------------------------------------------------

# The script prints usage and exits 1 when called without arguments.
set +e
USAGE_OUT="$(bash "$SCRIPT" 2>&1)"
USAGE_STATUS=$?
set -e
[ "$USAGE_STATUS" -eq 1 ] || fail "no-arg invocation should exit 1, got $USAGE_STATUS"

usage_targets() {
    sed -n '/^Packages:$/,/^$/p' <<<"$USAGE_OUT" | grep -oE '^  [a-z0-9-]+' | tr -d ' '
}

dispatch_targets() {
    awk '/^case "\$TARGET" in/{flag=1;next} flag && /^esac/{flag=0} flag' "$SCRIPT" \
        | grep -oE '^    [a-z0-9-]+\)' | tr -d ' )'
}

test_usage_matches_dispatch() {
    local diff_out
    diff_out="$(diff <(usage_targets | sort) <(dispatch_targets | sort))" \
        || fail "usage() packages and dispatch cases diverge: $diff_out"
    pass "usage() advertises exactly the dispatch targets ($(usage_targets | wc -l | tr -d ' ') targets)"
}

# ---------------------------------------------------------------------------
# Static contract: every env var usage() advertises is consumed by the
# script (outside usage()), and the dead sandbox-build names stay absent.
# ---------------------------------------------------------------------------

# Script source with the usage() function body stripped, so greps for
# advertised names do not match the advertisement itself.
SCRIPT_NO_USAGE="$TEST_TMP/rpm-build-no-usage.sh"
awk '/^usage\(\) \{/{skip=1} !skip{print} skip && /^\}/{skip=0}' "$SCRIPT" > "$SCRIPT_NO_USAGE"

test_advertised_env_vars_are_consumed() {
    local var
    while IFS= read -r var; do
        [ -n "$var" ] || continue
        grep -Eq "\\\$\{?${var}" "$SCRIPT_NO_USAGE" \
            || fail "usage() advertises env var ${var} but nothing consumes it"
    done < <(sed -n '/^Environment variables:$/,/^$/p' <<<"$USAGE_OUT" | grep -oE '^  [A-Z_]+' | tr -d ' ')
    pass "every advertised env var is consumed outside usage()"
}

test_dead_sandbox_names_stay_absent() {
    local name
    for name in DIST_TAG GVISOR_RELEASE GVISOR_RELEASE_VERSION GVISOR_BASE_URL \
                SANDBOX_PKG_DIR rpmbuild_with_dist build_gvisor_runsc \
                build_sandbox_all; do
        if grep -Fq "$name" "$SCRIPT"; then
            fail "removed dead sandbox identifier '$name' is still referenced"
        fi
    done
    pass "removed dead sandbox identifiers are absent"
}

# ---------------------------------------------------------------------------
# Functional contract: sandbox-repo needs no spec templates and covers both
# metadata locations, gated on createrepo_c/createrepo availability.
# ---------------------------------------------------------------------------

# Run the script from an isolated copy with only scripts/rpm-build.sh in it:
# no spec templates, no distribution/ tree — proving the sandbox-repo path
# cannot depend on them.
make_sandbox() {
    local work="$1"
    mkdir -p "$work/scripts" "$work/bin"
    cp "$SCRIPT" "$work/scripts/"
}

# Fake createrepo_c that records its arguments; a stand-in for the real
# metadata writer so the test verifies directory selection, not repodata.
make_fake_createrepo() {
    local work="$1"
    cat > "$work/bin/createrepo_c" <<EOF
#!/usr/bin/env bash
echo "createrepo_c \$*" >> "$work/createrepo.log"
EOF
    chmod +x "$work/bin/createrepo_c"
}

test_sandbox_repo_covers_both_branches() {
    local work="$TEST_TMP/both"
    make_sandbox "$work"
    make_fake_createrepo "$work"
    mkdir -p "$work/scripts/rpmbuild/RPMS/x86_64" \
             "$work/dist/sandbox/alinux4/RPMS" "$work/dist/sandbox/alinux4/SRPMS"
    touch "$work/scripts/rpmbuild/RPMS/x86_64/stub-1.0-1.x86_64.rpm" \
          "$work/dist/sandbox/alinux4/RPMS/stub-1.0-1.x86_64.rpm" \
          "$work/dist/sandbox/alinux4/SRPMS/stub-1.0-1.src.rpm"

    PATH="$work/bin:$PATH" RPMBUILD=true TARGET_ARCH=x86_64 \
        SANDBOX_DIST_DIR="$work/dist/sandbox/alinux4" \
        bash "$work/scripts/rpm-build.sh" sandbox-repo > "$work/out.log" 2>&1 \
        || fail "sandbox-repo failed on staged RPMs: $(cat "$work/out.log")"

    assert_contains "$work/createrepo.log" "createrepo_c --update $work/scripts/rpmbuild/RPMS/x86_64"
    assert_contains "$work/createrepo.log" "createrepo_c --update $work/dist/sandbox/alinux4/RPMS"
    assert_contains "$work/createrepo.log" "createrepo_c --update $work/dist/sandbox/alinux4/SRPMS"
    [ "$(wc -l < "$work/createrepo.log" | tr -d ' ')" -eq 3 ] \
        || fail "expected exactly 3 createrepo calls, got $(cat "$work/createrepo.log")"
    assert_not_contains "$work/out.log" "Spec template not found"
    pass "sandbox-repo writes metadata for RPMS/<arch> and both SANDBOX_DIST_DIR subdirs"
}

test_sandbox_repo_tolerates_missing_dist_dir() {
    local work="$TEST_TMP/no-dist"
    make_sandbox "$work"
    make_fake_createrepo "$work"
    mkdir -p "$work/scripts/rpmbuild/RPMS/x86_64"

    PATH="$work/bin:$PATH" RPMBUILD=true TARGET_ARCH=x86_64 \
        bash "$work/scripts/rpm-build.sh" sandbox-repo > "$work/out.log" 2>&1 \
        || fail "sandbox-repo failed without SANDBOX_DIST_DIR: $(cat "$work/out.log")"

    assert_contains "$work/createrepo.log" "createrepo_c --update $work/scripts/rpmbuild/RPMS/x86_64"
    [ "$(wc -l < "$work/createrepo.log" | tr -d ' ')" -eq 1 ] \
        || fail "expected exactly 1 createrepo call, got $(cat "$work/createrepo.log")"
    pass "sandbox-repo skips absent SANDBOX_DIST_DIR branches"
}

test_sandbox_repo_requires_createrepo() {
    local work="$TEST_TMP/no-createrepo"
    local minbin="$work/minbin"
    make_sandbox "$work"
    mkdir -p "$minbin"
    # Minimal PATH: the interpreter builtins plus the few externals the
    # script touches before dispatch — and deliberately no createrepo*.
    for tool in true uname mkdir dirname; do
        ln -s "$(command -v "$tool")" "$minbin/$tool"
    done

    local status=0
    # Invoke through "$BASH" (absolute interpreter path): PATH contains only
    # the minimal tool set above, so a bare "bash" would not resolve.
    PATH="$minbin" RPMBUILD=true TARGET_ARCH=x86_64 \
        "$BASH" "$work/scripts/rpm-build.sh" sandbox-repo > "$work/out.log" 2>&1 \
        || status=$?
    [ "$status" -ne 0 ] || fail "sandbox-repo should fail without createrepo_c/createrepo"
    assert_contains "$work/out.log" "createrepo_c (or createrepo) is required"
    pass "sandbox-repo gates on createrepo_c/createrepo availability"
}

test_usage_matches_dispatch
test_advertised_env_vars_are_consumed
test_dead_sandbox_names_stay_absent
test_sandbox_repo_covers_both_branches
test_sandbox_repo_tolerates_missing_dist_dir
test_sandbox_repo_requires_createrepo

echo "==> All rpm-build usage contract tests passed!"
