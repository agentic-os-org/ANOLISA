#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install.sh (install-hermes) — the update path of
# clone_repo stashes uncommitted changes and then runs `git pull --ff-only`.
# When the branch has diverged the pull fails under `set -e`, which used to
# abort with exit 128 before the recovery block could run, leaving the
# user's changes stranded in the stash and gone from the worktree.
#
# The real clone_repo function is extracted from install.sh and driven
# against a temporary bare remote; stdin is /dev/null so any restore prompt
# takes its non-interactive default. No dependencies; run directly:
#     bash test-clone-repo-update.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/install.sh"

pass=0
fail=0
scenario_n=0

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

ok_()   { echo "ok - $1"; pass=$((pass + 1)); }
not_ok_() { echo "not ok - $1"; fail=$((fail + 1)); }

# check <name> <command...> — pass when the command succeeds.
check() {
    scenario_n=$((scenario_n + 1))
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        ok_ "$scenario_n $name"
    else
        not_ok_ "$scenario_n $name"
    fi
}

# check_contains <name> <haystack> <needle>
check_contains() {
    scenario_n=$((scenario_n + 1))
    case "$2" in
        *"$3"*) ok_ "$scenario_n $1" ;;
        *)      not_ok_ "$scenario_n $1" ;;
    esac
}

# Extract clone_repo (a top-level function) from the real installer.
awk '/^clone_repo\(\) \{/,/^\}/' "$SCRIPT" > "$tmp/clone_repo.fn"
if ! grep -q '^clone_repo()' "$tmp/clone_repo.fn"; then
    echo "Bail out! could not extract clone_repo() from $SCRIPT"
    exit 1
fi

# make_repo <dir> — bare remote (HEAD -> main) plus a clone with one commit.
make_repo() {
    mkdir -p "$1/remote.git" "$1/seed"
    git init -q --bare "$1/remote.git"
    git -C "$1/remote.git" symbolic-ref HEAD refs/heads/main
    git -C "$1/seed" init -q
    git -C "$1/seed" config user.email test@example.com
    git -C "$1/seed" config user.name test
    echo base > "$1/seed/file.txt"
    git -C "$1/seed" add file.txt
    git -C "$1/seed" commit -qm base
    git -C "$1/seed" branch -M main
    git -C "$1/seed" remote add origin "$1/remote.git"
    git -C "$1/seed" push -q origin main
    git clone -q "$1/remote.git" "$1/work"
    git -C "$1/work" config user.email test@example.com
    git -C "$1/work" config user.name test
}

# push_remote_commit <repo-dir> <content> — advance origin/main.
push_remote_commit() {
    git clone -q "$1/remote.git" "$1/other"
    git -C "$1/other" config user.email test@example.com
    git -C "$1/other" config user.name test
    echo "$2" > "$1/other/remote-file.txt"
    git -C "$1/other" add remote-file.txt
    git -C "$1/other" commit -qm "remote: $2"
    git -C "$1/other" push -q origin main
}

# run_clone_repo <repo-dir> — run the extracted function non-interactively.
run_clone_repo() {
    local frag="$1/fragment.sh"
    {
        echo '#!/bin/bash'
        echo 'set -e'
        echo 'log_info()    { echo "[info] $*"; }'
        echo 'log_success() { echo "[ok] $*"; }'
        echo 'log_warn()    { echo "[warn] $*"; }'
        echo 'log_error()   { echo "[error] $*"; }'
        echo "INSTALL_DIR=\"$1/work\""
        echo 'BRANCH=main'
        echo 'REPO_URL_SSH=unused'
        echo 'REPO_URL_HTTPS=unused'
        echo ". \"$tmp/clone_repo.fn\""
        echo 'clone_repo'
    } > "$frag"
    bash "$frag" < /dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Scenario 1 (the bug): diverged branch + dirty worktree. The failed pull
# must exit 1 (not 128) with the dirty file restored to the worktree.
# ---------------------------------------------------------------------------
s1="$tmp/s1"; make_repo "$s1"; push_remote_commit "$s1" remote-change
echo local-change > "$s1/work/local-file.txt"
git -C "$s1/work" add local-file.txt
git -C "$s1/work" commit -qm local
echo "dirty content" > "$s1/work/dirty.txt"

out="$(run_clone_repo "$s1")"; code=$?
echo "# scenario1 exit=$code"
echo "$out" | sed 's/^/#     /'
check "diverged pull exits 1 (got $code)" test "$code" -eq 1
check_contains "diverged pull reports failure" "$out" "Update failed:"
check "dirty file restored to worktree after failed pull" test -f "$s1/work/dirty.txt"
check_contains "recovery output mentions git stash apply" "$out" "git stash apply"

# ---------------------------------------------------------------------------
# Scenario 2 (control): diverged branch but a clean worktree. The guard must
# exit 1 without any stash handling.
# ---------------------------------------------------------------------------
s2="$tmp/s2"; make_repo "$s2"; push_remote_commit "$s2" remote-change
echo local-change > "$s2/work/local-file.txt"
git -C "$s2/work" add local-file.txt
git -C "$s2/work" commit -qm local

out="$(run_clone_repo "$s2")"; code=$?
echo "# scenario2 exit=$code"
check "diverged pull with clean tree exits 1 (got $code)" test "$code" -eq 1
check_contains "diverged pull with clean tree reports failure" "$out" "Update failed:"
check_contains "points at rebase as manual resolution" "$out" "git rebase origin/main"
check "no stash left behind" test -z "$(git -C "$s2/work" stash list)"

# ---------------------------------------------------------------------------
# Scenario 3 (control): clean fast-forward update still succeeds.
# ---------------------------------------------------------------------------
s3="$tmp/s3"; make_repo "$s3"; push_remote_commit "$s3" remote-change

out="$(run_clone_repo "$s3")"; code=$?
echo "# scenario3 exit=$code"
check "clean fast-forward update exits 0 (got $code)" test "$code" -eq 0
check_contains "clean update reports repository ready" "$out" "Repository ready"

echo "1..$scenario_n"
if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
