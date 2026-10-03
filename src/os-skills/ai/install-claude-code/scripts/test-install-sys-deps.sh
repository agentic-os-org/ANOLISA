#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install-claude-code.sh — install_sys_deps must not
# abort the installer on hosts that have neither rpm nor dnf. The unguarded
# `rpm -q` probes used to add glibc/libstdc++ to the package list on such
# hosts, and the following `sudo dnf install` then killed the script under
# `set -euo pipefail` (exit 127, "dnf: command not found") before the
# native/npm/nvm install methods could run.
#
# Runs the real script's body with the trailing `main "$@"` invocation
# stripped, then calls main against a curated PATH that contains no rpm/dnf
# (same harness style as test-install-claude-code.sh, see #3530/#3547).
# No dependencies; run directly:
#     bash test-install-sys-deps.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/install-claude-code.sh"

pass=0
fail=0
scenario_n=0

# Build a runnable harness: the script body (sans `main "$@"`) plus the
# scenario lines, executed as one script.
make_harness() {
    local out="$1"
    shift
    grep -v '^[[:space:]]*main "\$@"' "$SCRIPT" > "$out"
    for line in "$@"; do
        echo "$line" >> "$out"
    done
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# A bin directory simulating a host without rpm/dnf: mocked sudo/curl/npm/
# node plus symlinks to the real core tools the script uses.
make_mockbin() {
    local mockbin="$1" with_curl="$2"
    mkdir -p "$mockbin"

    printf '#!/bin/sh\nexec "$@"\n' > "$mockbin/sudo"

    cat > "$mockbin/curl" <<'STUB'
#!/bin/sh
case "$*" in
  *claude.ai/install.sh*)
    printf '%s\n' '#!/bin/sh' \
      'mkdir -p "$HOME/.local/bin"' \
      '{ echo "#!/bin/sh"; echo "echo claude-mock 1.0.0"; } > "$HOME/.local/bin/claude"' \
      'chmod +x "$HOME/.local/bin/claude"'
    ;;
  *) echo "curl-mock: $*" ;;
esac
STUB

    printf '#!/bin/sh\necho "npm-mock: $*"\n' > "$mockbin/npm"
    printf '#!/bin/sh\n[ "$1" = "--version" ] && echo v20.0.0 || exit 0\n' > "$mockbin/node"
    chmod +x "$mockbin/sudo" "$mockbin/curl" "$mockbin/npm" "$mockbin/node"

    for tool in bash sh git tar gzip grep which mkdir chmod sed cut cat cp date rm; do
        local real
        real="$(command -v "$tool" 2>/dev/null)" || continue
        ln -sf "$real" "$mockbin/$tool"
    done

    # Scenario 2 also removes curl, so install_sys_deps has work to skip.
    [ "$with_curl" = true ] || rm -f "$mockbin/curl"
}

# run_scenario <name> <mockbin> <home-dir> <expected substrings...>
# Runs `main --skip-tokenless` under the mocked PATH and asserts exit 0 with
# all expected substrings present in the output.
run_scenario() {
    local name="$1" mockbin="$2" home_dir="$3"
    shift 3
    scenario_n=$((scenario_n + 1))
    local harness="$tmp/harness-$scenario_n"
    make_harness "$harness" 'main --skip-tokenless'
    mkdir -p "$home_dir"

    local out exit_code
    out="$(HOME="$home_dir" PATH="$mockbin" bash "$harness" 2>&1)"
    exit_code=$?

    local ok=true
    if [[ "$exit_code" -ne 0 ]]; then
        ok=false
    fi
    local expected
    for expected in "$@"; do
        [[ "$out" == *"$expected"* ]] || ok=false
    done

    if [[ "$ok" == true ]]; then
        echo "ok - $name"
        pass=$((pass + 1))
    else
        echo "not ok - $name (exit=$exit_code)"
        echo "$out" | sed 's/^/    /'
        fail=$((fail + 1))
    fi
}

# Scenario 1: curl/git/tar/gzip present, no rpm/dnf. The rpm probes must not
# fabricate a package list — sys-deps reports "already installed" and the
# native installer runs to completion.
make_mockbin "$tmp/bin-full" true
run_scenario "no rpm/dnf host proceeds past sys-deps to native install" \
    "$tmp/bin-full" "$tmp/home1" \
    "All system prerequisites already installed." \
    "Attempting native installer" \
    "Done!"

# Scenario 2: curl missing as well, so install_sys_deps has packages to
# install but no rpm/dnf to install them with — it must warn, skip, and let
# the npm method take over instead of dying at `dnf`.
make_mockbin "$tmp/bin-nocurl" false
run_scenario "no rpm/dnf and no curl host skips distro packages, npm installs" \
    "$tmp/bin-nocurl" "$tmp/home2" \
    "skipping distribution packages" \
    "Attempting npm global install" \
    "Claude Code installed via npm."

echo "1..$((pass + fail))"
if [[ "$fail" -gt 0 ]]; then
    exit 1
fi
exit 0
