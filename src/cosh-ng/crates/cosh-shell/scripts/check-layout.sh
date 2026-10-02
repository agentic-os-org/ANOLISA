#!/usr/bin/env bash
set -u

# Force a stable locale so `wc -l` emits the ASCII "total" line the parsers
# below filter on. Under a localized locale the total line ("总计") slips past
# the `$2 != "total"` guard and is mis-counted as an unregistered large file.
export LC_ALL=C

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$crate_dir/../.." && pwd)"

# Resolve the monorepo root that owns specs/ from version control so the audit
# depends only on files inside the current git checkout — never on a
# machine-specific absolute path or a sibling tree outside the repository.
if monorepo_root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)"; then
  :
else
  monorepo_root="$(cd "$repo_root/../.." && pwd)"
fi
specs_root="$monorepo_root/specs"

cd "$repo_root" || exit 2

if ! command -v rg >/dev/null 2>&1; then
  echo "error: rg is required" >&2
  exit 2
fi

failures=0

section() {
  printf '\n== %s ==\n' "$1"
}

record_failure() {
  failures=$((failures + 1))
}

count_rg() {
  local pattern="$1"
  local path="$2"
  local count
  count="$(rg -n "$pattern" "$path" 2>/dev/null | wc -l | tr -d ' ')"
  printf '%s' "$count"
}

section "Root src files"
root_files="$(find crates/cosh-shell/src -maxdepth 1 -type f -name '*.rs' | sort)"
printf '%s\n' "$root_files"
unexpected_root_files="$(printf '%s\n' "$root_files" | grep -Ev 'crates/cosh-shell/src/(lib|main)\.rs$' || true)"
if [ -n "$unexpected_root_files" ]; then
  echo "violation: root src contains implementation/facade files"
  printf '%s\n' "$unexpected_root_files"
  record_failure
fi

section "lib.rs public surface"
pub_surface="$(rg -n '^pub (mod|use)' crates/cosh-shell/src/lib.rs || true)"
printf '%s\n' "$pub_surface"
public_api_inventory_script="$script_dir/inventory-public-api.sh"
if [ -x "$public_api_inventory_script" ]; then
  public_api_inventory="$("$public_api_inventory_script")"
  unclassified_public_api="$(printf '%s\n' "$public_api_inventory" | awk -F',' 'NR > 1 && $5 == "unclassified" { print }')"
  nested_public_api="$(printf '%s\n' "$public_api_inventory" | awk -F',' 'NR > 1 && $5 == "nested-public-surface" { print }')"
  if [ -n "$unclassified_public_api" ]; then
    echo "unclassified public API:"
    printf '%s\n' "$unclassified_public_api"
    echo "violation: lib.rs public surface contains unclassified symbols"
    record_failure
  elif [ -n "$nested_public_api" ]; then
    echo "nested public API surface:"
    printf '%s\n' "$nested_public_api"
    echo "violation: nested public API surface must be moved to owner-public-surface or hidden"
    record_failure
  else
    echo "registered public API surface:"
    printf '%s\n' "$public_api_inventory" |
      awk -F',' '
        NR > 1 {
          counts[$5] += 1
        }
        END {
          for (classification in counts) {
            printf "%s %s\n", counts[classification], classification
          }
        }
      ' |
      sort
    echo "ok: lib.rs public surface is classified by inventory-public-api.sh"
  fi
else
  echo "violation: inventory-public-api.sh is missing or not executable"
  record_failure
fi

section "Self-crate public paths"
self_crate_inventory="$specs_root/cosh-ng-code-organization/self-crate-path-inventory.md"
registered_self_crate_counts=""
if [ -f "$self_crate_inventory" ]; then
  registered_self_crate_counts="$(
    awk -F'|' '
      /^\|[[:space:]]*[0-9]+[[:space:]]*\|/ {
        count = $2
        path = $3
        gsub(/[[:space:]]/, "", count)
        gsub(/[`[:space:]]/, "", path)
        if (count != "" && path != "") {
          print count " " path
        }
      }
    ' "$self_crate_inventory"
  )"
fi
self_path_count="$(count_rg 'cosh_shell::' crates/cosh-shell/src)"
echo "cosh_shell:: refs in src: $self_path_count"
if [ "$self_path_count" -gt 0 ]; then
  self_path_counts="$(rg -n 'cosh_shell::' crates/cosh-shell/src | cut -d: -f1 | sort | uniq -c | awk '{ print $1 " " $2 }')"
  registered_self_paths=""
  unregistered_self_paths=""
  while read -r current_count path; do
    [ -n "$path" ] || continue
    registered_count="$(
      printf '%s\n' "$registered_self_crate_counts" |
        awk -v path="$path" '$2 == path { print $1; found = 1 } END { if (!found) print "" }'
    )"
    if [ -n "$registered_count" ] && [ "$current_count" -le "$registered_count" ]; then
      registered_self_paths="$registered_self_paths
$current_count $path"
    else
      unregistered_self_paths="$unregistered_self_paths
$current_count $path"
    fi
  done <<EOF
$self_path_counts
EOF
  registered_self_paths="$(printf '%s\n' "$registered_self_paths" | sed '/^[[:space:]]*$/d')"
  unregistered_self_paths="$(printf '%s\n' "$unregistered_self_paths" | sed '/^[[:space:]]*$/d')"
  if [ -n "$registered_self_paths" ]; then
    echo "registered self-crate public path debt:"
    printf '%s\n' "$registered_self_paths" | sort -nr | head -30
  fi
  if [ -n "$unregistered_self_paths" ]; then
    echo "unregistered or increased self-crate public paths:"
    printf '%s\n' "$unregistered_self_paths" | sort -nr
    echo "violation: self-crate public paths must be removed or registered"
    record_failure
  else
    echo "ok: all self-crate public paths are registered in self-crate-path-inventory.md"
  fi
fi

section "Forbidden dependency direction candidates"
forbidden_inventory="$specs_root/cosh-ng-code-organization/forbidden-dependency-inventory.md"
registered_forbidden_counts=""
if [ -f "$forbidden_inventory" ]; then
  registered_forbidden_counts="$(
    awk -F'|' '
      /^\|[[:space:]]*[0-9]+[[:space:]]*\|/ {
        count = $2
        path = $3
        gsub(/[[:space:]]/, "", count)
        gsub(/[`[:space:]]/, "", path)
        if (count != "" && path != "") {
          print count " " path
        }
      }
    ' "$forbidden_inventory"
  )"
fi
forbidden_hits=""
forbidden_hits="$forbidden_hits$(rg -n 'crate::(agent|approval|ui|agent_render|runtime)' crates/cosh-shell/src/shell_host 2>/dev/null || true)"
forbidden_hits="$forbidden_hits
$(rg -n 'crate::(shell_host|journal|activity)' crates/cosh-shell/src/adapter 2>/dev/null || true)"
forbidden_hits="$forbidden_hits
$(rg -n 'crate::(agent|runtime)' crates/cosh-shell/src/hooks 2>/dev/null || true)"
if [ -d crates/cosh-shell/src/ui ]; then
  forbidden_hits="$forbidden_hits
$(rg -n 'crate::runtime' crates/cosh-shell/src/ui 2>/dev/null || true)"
fi
if [ -d crates/cosh-shell/src/agent_render ]; then
  forbidden_hits="$forbidden_hits
$(rg -n 'crate::runtime' crates/cosh-shell/src/agent_render 2>/dev/null || true)"
fi
forbidden_hits="$(printf '%s\n' "$forbidden_hits" | sed '/^[[:space:]]*$/d')"
if [ -n "$forbidden_hits" ]; then
  forbidden_counts="$(printf '%s\n' "$forbidden_hits" | cut -d: -f1 | sort | uniq -c | awk '{ print $1 " " $2 }')"
  registered_forbidden=""
  unregistered_forbidden=""
  while read -r current_count path; do
    [ -n "$path" ] || continue
    registered_count="$(
      printf '%s\n' "$registered_forbidden_counts" |
        awk -v path="$path" '$2 == path { print $1; found = 1 } END { if (!found) print "" }'
    )"
    if [ -n "$registered_count" ] && [ "$current_count" -le "$registered_count" ]; then
      registered_forbidden="$registered_forbidden
$current_count $path"
    else
      unregistered_forbidden="$unregistered_forbidden
$current_count $path"
    fi
  done <<EOF
$forbidden_counts
EOF
  registered_forbidden="$(printf '%s\n' "$registered_forbidden" | sed '/^[[:space:]]*$/d')"
  unregistered_forbidden="$(printf '%s\n' "$unregistered_forbidden" | sed '/^[[:space:]]*$/d')"
  if [ -n "$registered_forbidden" ]; then
    echo "registered forbidden dependency debt:"
    printf '%s\n' "$registered_forbidden"
  fi
  if [ -n "$unregistered_forbidden" ]; then
    echo "unregistered forbidden dependency candidates:"
    printf '%s\n' "$unregistered_forbidden"
    printf '%s\n' "$forbidden_hits"
    echo "violation: forbidden dependency candidates found"
    record_failure
  else
    echo "ok: all forbidden dependency candidates are registered in forbidden-dependency-inventory.md"
  fi
else
  echo "ok"
fi

section "Large production files"
large_file_inventory="$specs_root/cosh-ng-code-organization/large-file-inventory.md"
registered_large_paths=""
if [ -f "$large_file_inventory" ]; then
  registered_large_paths="$(
    awk -F'|' '
      /^\|[[:space:]]*[0-9]+[[:space:]]*\|/ {
        path = $3
        gsub(/[`[:space:]]/, "", path)
        if (path != "") {
          print "crates/cosh-shell/" path
        }
      }
    ' "$large_file_inventory"
  )"
fi
large_production="$(
  find crates/cosh-shell/src -type f -name '*.rs' -print0 |
    xargs -0 wc -l |
    awk '
      $2 != "total" &&
      $1 >= 700 &&
      $2 !~ /\/tests(\/|\.rs$)/ &&
      $2 !~ /_tests\.rs$/ &&
      $2 !~ /\/runtime_tests(\/|\.rs$)/ &&
      $2 !~ /\/hook_tests\.rs$/ {
        print
      }
    ' |
    sort -nr
)"
if [ -n "$large_production" ]; then
  registered_large_production=""
  unregistered_large_production=""
  while read -r lines path; do
    [ -n "$path" ] || continue
    if printf '%s\n' "$registered_large_paths" | grep -Fxq "$path"; then
      registered_large_production="$registered_large_production
$lines $path"
    else
      unregistered_large_production="$unregistered_large_production
$lines $path"
    fi
  done <<EOF
$large_production
EOF
  registered_large_production="$(printf '%s\n' "$registered_large_production" | sed '/^[[:space:]]*$/d')"
  unregistered_large_production="$(printf '%s\n' "$unregistered_large_production" | sed '/^[[:space:]]*$/d')"
  if [ -n "$registered_large_production" ]; then
    echo "registered large-file debt:"
    printf '%s\n' "$registered_large_production"
  fi
  if [ -n "$unregistered_large_production" ]; then
    echo "unregistered large production files:"
    printf '%s\n' "$unregistered_large_production"
    echo "violation: production files over 700 lines require split plan or waiver"
    record_failure
  else
    echo "ok: all over-threshold production files are registered in large-file-inventory.md"
  fi
else
  echo "ok"
fi

section "Source tests"
src_test_count="$(count_rg '#\[(tokio::)?test\]' crates/cosh-shell/src)"
echo "src tests: $src_test_count"
heavy_src_tests="$(rg -n 'CARGO_BIN_EXE_cosh-shell' crates/cosh-shell/src 2>/dev/null || true)"
if [ -n "$heavy_src_tests" ]; then
  printf '%s\n' "$heavy_src_tests"
  echo "violation: src tests must not spawn the cosh-shell binary"
  record_failure
fi
source_heavy_inventory="$specs_root/shell-test-organization/source-heavy-test-inventory.md"
registered_source_heavy_counts=""
if [ -f "$source_heavy_inventory" ]; then
  registered_source_heavy_counts="$(
    awk -F'|' '
      /^\|[[:space:]]*[0-9]+[[:space:]]*\|/ {
        count = $2
        path = $3
        gsub(/[[:space:]]/, "", count)
        gsub(/[`[:space:]]/, "", path)
        if (count != "" && path != "") {
          print count " " path
        }
      }
    ' "$source_heavy_inventory"
  )"
fi
source_heavy_pattern='/bin/sleep|mock_provider_script|write mock provider|let child = match Command::new\("bash"\)|let child = match Command::new\(&tokens\[0\]\)|openpty\(None, None\)|nix::pty::openpty\(None, None\)|cosh_hook_test'
source_heavy_hits="$(rg -n "$source_heavy_pattern" crates/cosh-shell/src 2>/dev/null || true)"
# Broadened subprocess-spawn detection for test-scoped source code. The named
# markers above can be sidestepped by spawning any other binary (e.g.
# `Command::new("sleep")`), so every process-spawn primitive inside test code
# is treated as a heavy-test risk: dedicated test files by path, plus
# `#[cfg(test)]` module regions embedded in production files (brace-tracked so
# production spawns in the same file stay allowed).
test_path_pattern='/tests(/|\.rs$)|_tests\.rs$|/runtime_tests(/|\.rs$)|/hook_tests\.rs$'
# Bracket classes keep the pattern valid for both rg and awk dynamic regexes.
test_spawn_pattern='Command::new[(]|process::Command|CommandExt|libc::fork|posix_spawn|[.]pre_exec[(]'
dedicated_test_spawns="$(
  find crates/cosh-shell/src -type f -name '*.rs' |
    grep -E "$test_path_pattern" |
    xargs -r rg -n "$test_spawn_pattern" 2>/dev/null || true
)"
inline_test_spawns="$(
  find crates/cosh-shell/src -type f -name '*.rs' |
    grep -Ev "$test_path_pattern" |
    while read -r source_file; do
      awk -v spawn_re="$test_spawn_pattern" -v fname="$source_file" '
        function braces(str, ch,   n) { n = split(str, scratch, ch); return n - 1 }
        {
          line = $0
          if (in_test) {
            if (line ~ spawn_re) print fname ":" FNR ":" line
            depth += braces(line, "{") - braces(line, "}")
            if (depth <= 0) in_test = 0
            next
          }
          if (pending && line ~ /(^|[[:space:]])mod[[:space:]]/) {
            pending = 0
            if (line ~ /\{/) {
              in_test = 1
              depth = braces(line, "{") - braces(line, "}")
              if (line ~ spawn_re) print fname ":" FNR ":" line
              if (depth <= 0) in_test = 0
            }
            next
          }
          if (line ~ /#\[cfg\(test\)\]/) { pending = 1; next }
          if (pending && line !~ /^[[:space:]]*#\[/ && line !~ /^[[:space:]]*$/) pending = 0
        }
      ' "$source_file"
    done
)"
source_heavy_hits="$(
  printf '%s\n%s\n%s\n' "$source_heavy_hits" "$dedicated_test_spawns" "$inline_test_spawns" |
    sed '/^[[:space:]]*$/d' |
    sort -u
)"
if [ -n "$source_heavy_hits" ]; then
  source_heavy_counts="$(printf '%s\n' "$source_heavy_hits" | cut -d: -f1 | sort | uniq -c | awk '{ print $1 " " $2 }')"
  registered_source_heavy=""
  unregistered_source_heavy=""
  while read -r current_count path; do
    [ -n "$path" ] || continue
    registered_count="$(
      printf '%s\n' "$registered_source_heavy_counts" |
        awk -v path="$path" '$2 == path { print $1; found = 1 } END { if (!found) print "" }'
    )"
    if [ -n "$registered_count" ] && [ "$current_count" -le "$registered_count" ]; then
      registered_source_heavy="$registered_source_heavy
$current_count $path"
    else
      unregistered_source_heavy="$unregistered_source_heavy
$current_count $path"
    fi
  done <<EOF
$source_heavy_counts
EOF
  registered_source_heavy="$(printf '%s\n' "$registered_source_heavy" | sed '/^[[:space:]]*$/d')"
  unregistered_source_heavy="$(printf '%s\n' "$unregistered_source_heavy" | sed '/^[[:space:]]*$/d')"
  if [ -n "$registered_source_heavy" ]; then
    echo "registered source heavy-test risk:"
    printf '%s\n' "$registered_source_heavy" | sort -nr
  fi
  if [ -n "$unregistered_source_heavy" ]; then
    echo "unregistered or increased source heavy-test risk:"
    printf '%s\n' "$unregistered_source_heavy" | sort -nr
    printf '%s\n' "$source_heavy_hits"
    echo "violation: source heavy-test risks must be migrated or registered"
    record_failure
  else
    echo "ok: source heavy-test risks are registered in source-heavy-test-inventory.md"
  fi
fi

section "Integration test layout"
for target in raw_cli shell_host logic protocol; do
  file="crates/cosh-shell/tests/${target}.rs"
  if [ -f "$file" ]; then
    test_count="$(count_rg '#\[(tokio::)?test\]' "$file")"
    echo "$file tests: $test_count"
    if [ "$test_count" -gt 0 ]; then
      echo "violation: $file should be aggregator-only"
      record_failure
    fi
  else
    echo "$file missing"
    if [ "$target" = "logic" ] || [ "$target" = "protocol" ]; then
      echo "violation: target layer not created yet: $target"
      record_failure
    fi
  fi
done
unexpected_top_level_tests="$(
  find crates/cosh-shell/tests -maxdepth 1 -type f -name '*.rs' |
    grep -Ev 'crates/cosh-shell/tests/(raw_cli|shell_host|logic|protocol)\.rs$' || true
)"
if [ -n "$unexpected_top_level_tests" ]; then
  echo "unexpected top-level integration targets:"
  printf '%s\n' "$unexpected_top_level_tests"
  echo "violation: new integration targets must be registered as stable layers before use"
  record_failure
fi
support_tests="$(rg -n '#\[(tokio::)?test\]' crates/cosh-shell/tests/support 2>/dev/null || true)"
if [ -n "$support_tests" ]; then
  echo "tests/support contains tests:"
  printf '%s\n' "$support_tests"
  echo "violation: tests/support must contain helpers only"
  record_failure
fi
if [ -x "$script_dir/inventory-tests.sh" ]; then
  uncategorized_tests="$("$script_dir/inventory-tests.sh" | awk -F',' 'NR > 1 && $1 == "uncategorized" { print }')"
  if [ -n "$uncategorized_tests" ]; then
    echo "uncategorized tests:"
    printf '%s\n' "$uncategorized_tests" | head -50
    echo "violation: tests must map to a known category"
    record_failure
  fi
else
  echo "violation: inventory-tests.sh is missing or not executable"
  record_failure
fi

section "Top-level mock scripts"
mock_scripts="$(find crates/cosh-shell/tests -maxdepth 1 -type f -name 'mock_*.sh' | sort)"
if [ -n "$mock_scripts" ]; then
  printf '%s\n' "$mock_scripts"
  echo "violation: mock scripts should move to tests/fixtures/provider after resolver lands"
  record_failure
else
  echo "ok"
fi

section "Result"
if [ "$failures" -gt 0 ]; then
  echo "layout audit failed with $failures violation group(s)"
  exit 1
fi

echo "layout audit passed"
