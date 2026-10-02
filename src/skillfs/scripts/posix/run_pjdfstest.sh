#!/usr/bin/env bash
#
# SkillFS T0/T0.1 — external POSIX test harness.
#
# Mounts SkillFS over a temporary source, exposes a writable passthrough
# sandbox under a normal skill directory, and runs an external pjdfstest
# checkout against that sandbox. The harness never touches the SKILL.md
# compiled-read path, skill-discover, .skill-meta, or any lifecycle
# reserved roots.
#
# T0.1 additions:
#   - --profile smoke|full        (default smoke = exclude both manifests)
#   - --include / --exclude       (additional glob filters, comma-separated)
#   - --rerun-failures-verbose    (prove -v rerun of unexpected fails)
#   - --blocked-manifest <PATH>   (override blocked_dependent.txt)
#   - three report buckets: expected / blocked / unexpected
#   - verdict label: UNEXPECTED_FAILURES (was REGRESSION)
#
# Hard requirements:
#   - Must run as root (pjdfstest itself requires root).
#   - /dev/fuse, fusermount3, prove (perl Test::Harness) must be present.
#   - The pjdfstest checkout must already have ./pjdfstest built.
#     See docs/testing/posix-external-harness.md for the install guide.
#
# This harness is optional; normal `cargo test` does not invoke it.

set -euo pipefail

usage() {
	cat <<'EOF'
Usage: scripts/posix/run_pjdfstest.sh [options]

Options:
  --pjdfstest <PATH>          Path to a built pjdfstest checkout (the directory
                              containing the `pjdfstest` binary and a `tests/`
                              subdirectory). Overrides SKILLFS_PJDFSTEST_DIR.
  --release                   Build/run the release skillfs binary.
  --profile smoke|full        Selection profile (default: smoke).
                                smoke = exclude every glob in all three
                                        manifests (expected-fail,
                                        blocked-dependent, caller-identity)
                                full  = run every *.t under tests/
  --include <PATTERNS>        Comma-separated globs (relative to tests/).
                              When set, only matching files are included.
                              May be repeated; entries accumulate.
  --exclude <PATTERNS>        Comma-separated globs to exclude. Stacks
                              with the profile-implied excludes and wins
                              over --include. May be repeated.
  --rerun-failures-verbose    After the main run, re-invoke `prove -v`
                              against the unexpected-fail set and write
                              the output to <tmp>/rerun-verbose.log.
                              Path is echoed in the report.
  --expected-fail <PATH>      Path to the expected-fail manifest
                              (default: scripts/posix/expected_fail.txt).
  --blocked-manifest <PATH>   Path to the blocked-dependent manifest
                              (default: scripts/posix/blocked_dependent.txt).
  --caller-identity-manifest <PATH>
                              Path to the caller-identity / semantics-gap
                              manifest (default:
                              scripts/posix/caller_identity.txt).
  --report <PATH>             Write the final summary report to PATH in
                              addition to stdout.
  --keep                      Keep the temporary source/mount tree and
                              logs after the run. Defaults to cleanup.
  --self-test                 Run the parser/classifier on canned prove
                              output (no FUSE, no sudo, no pjdfstest
                              binary). Exits 0 on success, 4 on
                              classification mismatch.
  -h, --help                  Show this help and exit.

Environment:
  SKILLFS_PJDFSTEST_DIR   Same as --pjdfstest (CLI flag wins).
  SKILLFS_POSIX_KEEP=1    Same as --keep.

Exit status:
  0   No unexpected failures (every failing pjdfstest file matched a
      manifest entry); or --self-test passed.
  2   Setup or environment error (missing prerequisites, build failure,
      mount failure, prove not installed, etc.).
  3   At least one unexpected pjdfstest failure.
  4   --self-test classification mismatch.
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PJDFSTEST_DIR="${SKILLFS_PJDFSTEST_DIR:-}"
EXPECTED_FAIL_FILE="$REPO_ROOT/scripts/posix/expected_fail.txt"
BLOCKED_MANIFEST_FILE="$REPO_ROOT/scripts/posix/blocked_dependent.txt"
CALLER_IDENTITY_MANIFEST_FILE="$REPO_ROOT/scripts/posix/caller_identity.txt"
PROFILE="debug"
CARGO_FLAGS=()
KEEP="${SKILLFS_POSIX_KEEP:-0}"
REPORT_PATH=""
SELECT_PROFILE="smoke"
INCLUDE_PATTERNS=()
EXCLUDE_PATTERNS=()
RERUN_VERBOSE=0
SELF_TEST=0

split_csv_into() {
	# $1 = nameref array (bash 4.3+), $2 = comma-separated string
	local -n _out="$1"
	local raw="$2"
	IFS=',' read -ra _tmp <<<"$raw"
	local item
	for item in "${_tmp[@]}"; do
		# Trim leading/trailing whitespace.
		item="${item#"${item%%[![:space:]]*}"}"
		item="${item%"${item##*[![:space:]]}"}"
		[[ -z "$item" ]] && continue
		_out+=("$item")
	done
}

while (( "$#" )); do
	case "$1" in
		--pjdfstest)
			shift
			[[ $# -gt 0 ]] || { echo "--pjdfstest needs a value" >&2; exit 2; }
			PJDFSTEST_DIR="$1"
			;;
		--pjdfstest=*)
			PJDFSTEST_DIR="${1#*=}"
			;;
		--expected-fail)
			shift
			[[ $# -gt 0 ]] || { echo "--expected-fail needs a value" >&2; exit 2; }
			EXPECTED_FAIL_FILE="$1"
			;;
		--expected-fail=*)
			EXPECTED_FAIL_FILE="${1#*=}"
			;;
		--blocked-manifest)
			shift
			[[ $# -gt 0 ]] || { echo "--blocked-manifest needs a value" >&2; exit 2; }
			BLOCKED_MANIFEST_FILE="$1"
			;;
		--blocked-manifest=*)
			BLOCKED_MANIFEST_FILE="${1#*=}"
			;;
		--caller-identity-manifest)
			shift
			[[ $# -gt 0 ]] || { echo "--caller-identity-manifest needs a value" >&2; exit 2; }
			CALLER_IDENTITY_MANIFEST_FILE="$1"
			;;
		--caller-identity-manifest=*)
			CALLER_IDENTITY_MANIFEST_FILE="${1#*=}"
			;;
		--self-test)
			SELF_TEST=1
			;;
		--report)
			shift
			[[ $# -gt 0 ]] || { echo "--report needs a value" >&2; exit 2; }
			REPORT_PATH="$1"
			;;
		--report=*)
			REPORT_PATH="${1#*=}"
			;;
		--profile)
			shift
			[[ $# -gt 0 ]] || { echo "--profile needs a value" >&2; exit 2; }
			SELECT_PROFILE="$1"
			;;
		--profile=*)
			SELECT_PROFILE="${1#*=}"
			;;
		--include)
			shift
			[[ $# -gt 0 ]] || { echo "--include needs a value" >&2; exit 2; }
			split_csv_into INCLUDE_PATTERNS "$1"
			;;
		--include=*)
			split_csv_into INCLUDE_PATTERNS "${1#*=}"
			;;
		--exclude)
			shift
			[[ $# -gt 0 ]] || { echo "--exclude needs a value" >&2; exit 2; }
			split_csv_into EXCLUDE_PATTERNS "$1"
			;;
		--exclude=*)
			split_csv_into EXCLUDE_PATTERNS "${1#*=}"
			;;
		--rerun-failures-verbose)
			RERUN_VERBOSE=1
			;;
		--release)
			PROFILE="release"
			CARGO_FLAGS=("--release")
			;;
		--keep)
			KEEP=1
			;;
		-h|--help)
			usage
			exit 0
			;;
		*)
			echo "Unknown argument: $1" >&2
			usage >&2
			exit 2
			;;
	esac
	shift
done

die_setup() {
	echo "[t0] setup error: $1" >&2
	if [[ $# -ge 2 ]]; then
		echo "[t0]   hint: $2" >&2
	fi
	exit 2
}

case "$SELECT_PROFILE" in
	smoke|full) ;;
	*)
		die_setup \
			"unknown --profile value: ${SELECT_PROFILE}" \
			"use 'smoke' (default) or 'full'"
		;;
esac

# ---------------------------------------------------------------------------
# --self-test: exercise the prove-output parser and the
# manifest-classifier on canned input. Runs without FUSE, sudo, or a
# pjdfstest binary. Exits 0 on success, 4 on mismatch. The harness
# never reaches the build/mount path under --self-test.
# ---------------------------------------------------------------------------
if (( SELF_TEST )); then
	tmp_summary="$(mktemp)"
	trap 'rm -f "$tmp_summary"' EXIT
	cat <<'SUMMARY' > "$tmp_summary"
Test Summary Report
-------------------
/x/tests/link/00.t          (Wstat: 0 Tests: 22 Failed: 22)
  Failed tests: 1-22
/x/tests/chmod/00.t         (Wstat: 0 Tests: 60 Failed: 12)
  Failed tests: 3-7, 20
/x/tests/mkdir/00.t         (Wstat: 0 Tests: 36 Failed: 4)
  Failed tests: 19-22
/x/tests/mkdir/03.t         (Wstat: 0 Tests: 3 Failed: 1)
  Failed tests: 3
SUMMARY

	# Mirror the production parse path.
	st_failed=()
	while IFS= read -r line; do
		trimmed="${line#"${line%%[![:space:]]*}"}"
		case "$trimmed" in
			*".t "*"(Wstat:"*)
				path="${trimmed%% (Wstat:*}"
				path="${path%"${path##*[![:space:]]}"}"
				st_failed+=("$path")
				;;
		esac
	done < "$tmp_summary"

	# Load manifests via the same reader the production path uses.
	read_manifest_into() {
		local -n _arr="$1"
		local path="$2"
		local raw stripped
		while IFS= read -r raw; do
			stripped="${raw%%#*}"
			stripped="${stripped#"${stripped%%[![:space:]]*}"}"
			stripped="${stripped%"${stripped##*[![:space:]]}"}"
			[[ -z "$stripped" ]] && continue
			_arr+=("$stripped")
		done < "$path"
	}
	matches_any() {
		local rel="$1"; shift
		local pat
		for pat in "$@"; do
			# shellcheck disable=SC2053
			case "$rel" in
				$pat) return 0 ;;
			esac
		done
		return 1
	}
	st_expected=()
	st_blocked=()
	st_identity=()
	read_manifest_into st_expected "$EXPECTED_FAIL_FILE"
	read_manifest_into st_blocked "$BLOCKED_MANIFEST_FILE"
	read_manifest_into st_identity "$CALLER_IDENTITY_MANIFEST_FILE"

	classify() {
		local rel="$1"
		if matches_any "$rel" "${st_expected[@]+${st_expected[@]}}"; then
			echo expected
		elif matches_any "$rel" "${st_blocked[@]+${st_blocked[@]}}"; then
			echo blocked
		elif matches_any "$rel" "${st_identity[@]+${st_identity[@]}}"; then
			echo caller_identity
		else
			echo unexpected
		fi
	}

	expected_results=(
		"link/00.t expected"
		"chmod/00.t blocked"
		"mkdir/00.t caller_identity"
		"mkdir/03.t unexpected"
	)

	rc=0
	echo "[self-test] parsed ${#st_failed[@]} failing path(s) from canned input"
	for entry in "${expected_results[@]}"; do
		want_rel="${entry%% *}"
		want_bucket="${entry##* }"
		got_bucket=""
		for abs in "${st_failed[@]}"; do
			rel="${abs#/x/tests/}"
			if [[ "$rel" == "$want_rel" ]]; then
				got_bucket="$(classify "$rel")"
				break
			fi
		done
		if [[ -z "$got_bucket" ]]; then
			echo "[self-test] FAIL: $want_rel not present in parsed list" >&2
			rc=4
		elif [[ "$got_bucket" != "$want_bucket" ]]; then
			echo "[self-test] FAIL: $want_rel classified as $got_bucket, expected $want_bucket" >&2
			rc=4
		else
			echo "[self-test] ok:   $want_rel -> $got_bucket"
		fi
	done
	if (( rc == 0 )); then
		echo "[self-test] OK"
	fi
	exit "$rc"
fi

[[ -n "$PJDFSTEST_DIR" ]] || die_setup \
	"pjdfstest path missing" \
	"pass --pjdfstest <PATH> or export SKILLFS_PJDFSTEST_DIR (see docs/testing/posix-external-harness.md)"

PJDFSTEST_REQUESTED="$PJDFSTEST_DIR"
if ! PJDFSTEST_DIR="$(cd "$PJDFSTEST_REQUESTED" 2>/dev/null && pwd)"; then
	die_setup \
		"pjdfstest path does not exist: ${PJDFSTEST_REQUESTED}" \
		"clone https://github.com/pjd/pjdfstest then build per docs/testing/posix-external-harness.md"
fi
[[ -n "$PJDFSTEST_DIR" ]] || die_setup \
	"pjdfstest path does not exist: ${PJDFSTEST_REQUESTED}" \
	"clone https://github.com/pjd/pjdfstest then build per docs/testing/posix-external-harness.md"

[[ -d "$PJDFSTEST_DIR/tests" ]] || die_setup \
	"no tests/ under ${PJDFSTEST_DIR}" \
	"point --pjdfstest at the checkout root that contains both ./pjdfstest and ./tests/"

[[ -x "$PJDFSTEST_DIR/pjdfstest" ]] || die_setup \
	"pjdfstest binary not built at ${PJDFSTEST_DIR}/pjdfstest" \
	"cd \"$PJDFSTEST_DIR\" && autoreconf -ifs && ./configure && make pjdfstest"

[[ -r "$EXPECTED_FAIL_FILE" ]] || die_setup \
	"expected-fail manifest not readable: ${EXPECTED_FAIL_FILE}"

[[ -r "$BLOCKED_MANIFEST_FILE" ]] || die_setup \
	"blocked-dependent manifest not readable: ${BLOCKED_MANIFEST_FILE}"

[[ -r "$CALLER_IDENTITY_MANIFEST_FILE" ]] || die_setup \
	"caller-identity manifest not readable: ${CALLER_IDENTITY_MANIFEST_FILE}"

command -v prove >/dev/null 2>&1 || die_setup \
	"prove not found on PATH" \
	"install Perl Test::Harness (e.g. dnf install perl-Test-Harness, apt install perl-modules)"

command -v fusermount3 >/dev/null 2>&1 || die_setup \
	"fusermount3 not found on PATH" \
	"install fuse3 (e.g. dnf install fuse3, apt install fuse3)"

[[ -e /dev/fuse ]] || die_setup \
	"/dev/fuse missing" \
	"load the fuse kernel module"

if [[ "$(id -u)" != "0" ]]; then
	die_setup \
		"pjdfstest must be run as root" \
		"re-run with sudo (the harness itself does not escalate)"
fi

SKILLFS_BIN="$REPO_ROOT/target/$PROFILE/skillfs"
echo "[t0] building skillfs ($PROFILE)"
cargo build "${CARGO_FLAGS[@]+${CARGO_FLAGS[@]}}" --bin skillfs --manifest-path "$REPO_ROOT/Cargo.toml" >/dev/null \
	|| die_setup "cargo build failed"
[[ -x "$SKILLFS_BIN" ]] || die_setup "skillfs binary missing after build: ${SKILLFS_BIN}"

# ---------------------------------------------------------------------------
# Read manifests. Each non-comment, non-blank line is a glob pattern (matched
# against `<rel>/<testfile>.t` under tests/).
# ---------------------------------------------------------------------------
read_manifest_into() {
	# $1 = nameref array, $2 = file path
	local -n _arr="$1"
	local path="$2"
	local raw stripped
	while IFS= read -r raw; do
		stripped="${raw%%#*}"
		stripped="${stripped#"${stripped%%[![:space:]]*}"}"
		stripped="${stripped%"${stripped##*[![:space:]]}"}"
		[[ -z "$stripped" ]] && continue
		_arr+=("$stripped")
	done < "$path"
}

EXPECTED_PATTERNS=()
BLOCKED_PATTERNS=()
CALLER_IDENTITY_PATTERNS=()
read_manifest_into EXPECTED_PATTERNS "$EXPECTED_FAIL_FILE"
read_manifest_into BLOCKED_PATTERNS "$BLOCKED_MANIFEST_FILE"
read_manifest_into CALLER_IDENTITY_PATTERNS "$CALLER_IDENTITY_MANIFEST_FILE"

matches_any() {
	# $1 = rel path; $2..N = patterns
	local rel="$1"
	shift
	local pat
	for pat in "$@"; do
		# shellcheck disable=SC2053
		case "$rel" in
			$pat) return 0 ;;
		esac
	done
	return 1
}

# ---------------------------------------------------------------------------
# Build the selection file list. Selection order:
#   1. start from every *.t under tests/;
#   2. drop entries that do not match any --include (if any include given);
#   3. drop entries that match any --exclude;
#   4. drop entries that match either manifest when profile=smoke.
# Manifest globs are NEVER applied as a CLI exclude when profile=full —
# full reproduces the T0 baseline.
# ---------------------------------------------------------------------------
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/skillfs-t0.XXXXXX")"
SOURCE_DIR="$TMP_ROOT/source"
MOUNT_DIR="$TMP_ROOT/mount"
SKILLFS_LOG="$TMP_ROOT/skillfs.log"
PROVE_LOG="$TMP_ROOT/prove.log"
PROVE_SUMMARY="$TMP_ROOT/prove.summary"
RERUN_VERBOSE_LOG="$TMP_ROOT/rerun-verbose.log"
SELECTED_FILE="$TMP_ROOT/selected.txt"
PID_FILE="$TMP_ROOT/skillfs.pid"
MOUNT_PID=""

SKILL_NAME="harness-skill"
SANDBOX_NAME="sandbox"
SANDBOX_PATH="$MOUNT_DIR/skills/$SKILL_NAME/$SANDBOX_NAME"

cleanup() {
	local rc=$?
	set +e
	if [[ -n "$MOUNT_PID" ]] && kill -0 "$MOUNT_PID" 2>/dev/null; then
		kill -TERM "$MOUNT_PID" 2>/dev/null || true
	fi
	if grep -Fq " $MOUNT_DIR " /proc/mounts 2>/dev/null; then
		fusermount3 -u "$MOUNT_DIR" >/dev/null 2>&1 || true
	fi
	if [[ -n "$MOUNT_PID" ]]; then
		wait "$MOUNT_PID" 2>/dev/null || true
	fi
	if [[ "$KEEP" == "1" ]]; then
		echo "[t0] keeping artifacts at $TMP_ROOT"
	else
		rm -rf "$TMP_ROOT"
	fi
	exit "$rc"
}
trap cleanup EXIT

# Enumerate test files.
ALL_TESTS=()
while IFS= read -r -d '' f; do
	ALL_TESTS+=("$f")
done < <(find "$PJDFSTEST_DIR/tests" -name '*.t' -type f -print0 | sort -z)

SELECTED_TESTS=()
SKIPPED_BY_INCLUDE=0
SKIPPED_BY_EXCLUDE=0
SKIPPED_BY_PROFILE=0
for abs in "${ALL_TESTS[@]+${ALL_TESTS[@]}}"; do
	rel="${abs#"$PJDFSTEST_DIR/tests/"}"

	if (( ${#INCLUDE_PATTERNS[@]} > 0 )); then
		if ! matches_any "$rel" "${INCLUDE_PATTERNS[@]}"; then
			SKIPPED_BY_INCLUDE=$((SKIPPED_BY_INCLUDE + 1))
			continue
		fi
	fi

	if (( ${#EXCLUDE_PATTERNS[@]} > 0 )); then
		if matches_any "$rel" "${EXCLUDE_PATTERNS[@]}"; then
			SKIPPED_BY_EXCLUDE=$((SKIPPED_BY_EXCLUDE + 1))
			continue
		fi
	fi

	if [[ "$SELECT_PROFILE" == "smoke" ]]; then
		if matches_any "$rel" "${EXPECTED_PATTERNS[@]+${EXPECTED_PATTERNS[@]}}" \
			|| matches_any "$rel" "${BLOCKED_PATTERNS[@]+${BLOCKED_PATTERNS[@]}}" \
			|| matches_any "$rel" "${CALLER_IDENTITY_PATTERNS[@]+${CALLER_IDENTITY_PATTERNS[@]}}"; then
			SKIPPED_BY_PROFILE=$((SKIPPED_BY_PROFILE + 1))
			continue
		fi
	fi

	SELECTED_TESTS+=("$abs")
done

if (( ${#SELECTED_TESTS[@]} == 0 )); then
	die_setup \
		"no test files selected after applying filters" \
		"check --include / --exclude / --profile arguments (have ${#ALL_TESTS[@]} total under $PJDFSTEST_DIR/tests)"
fi

# Write selection to a file so the prove invocation does not blow argv limits.
: >"$SELECTED_FILE"
for abs in "${SELECTED_TESTS[@]}"; do
	printf '%s\n' "$abs" >>"$SELECTED_FILE"
done

# ---------------------------------------------------------------------------
# Mount SkillFS.
# ---------------------------------------------------------------------------
mkdir -p "$SOURCE_DIR/$SKILL_NAME/$SANDBOX_NAME" "$MOUNT_DIR"
cat > "$SOURCE_DIR/$SKILL_NAME/SKILL.md" <<EOF
---
name: $SKILL_NAME
description: External POSIX test sandbox host. Do not edit at runtime.
version: 0.0.1
enabled: true
---

# Harness Skill

This skill exists purely to expose a writable passthrough sandbox at
\`skills/$SKILL_NAME/$SANDBOX_NAME/\` so pjdfstest can exercise SkillFS
without touching SKILL.md, skill-discover, .skill-meta, or lifecycle
reserved roots.
EOF

cat > "$SOURCE_DIR/skillfs-views.toml" <<EOF
[[view]]
name = "default"
default = true
description = "T0 external POSIX harness view"
skills = ["$SKILL_NAME"]
EOF

echo "[t0] mounting skillfs"
echo "[t0]   source: $SOURCE_DIR"
echo "[t0]   mount:  $MOUNT_DIR"
"$SKILLFS_BIN" mount "$SOURCE_DIR" "$MOUNT_DIR" \
	--foreground \
	--pid-file "$PID_FILE" \
	--log-file "$SKILLFS_LOG" \
	>/dev/null 2>&1 &
MOUNT_PID=$!

mounted=""
for _ in $(seq 1 60); do
	if grep -Fq " $MOUNT_DIR " /proc/mounts 2>/dev/null; then
		mounted=1
		break
	fi
	if ! kill -0 "$MOUNT_PID" 2>/dev/null; then
		break
	fi
	sleep 0.1
done

if [[ -z "$mounted" ]]; then
	echo "--- skillfs log ---" >&2
	[[ -f "$SKILLFS_LOG" ]] && cat "$SKILLFS_LOG" >&2 || true
	die_setup "skillfs mount did not come up at $MOUNT_DIR"
fi

[[ -d "$SANDBOX_PATH" ]] || die_setup \
	"sandbox not visible at $SANDBOX_PATH" \
	"check the skillfs log above"

# A small sanity probe that the sandbox is a real passthrough writable
# location and that we are not accidentally inside a virtual surface.
PROBE="$SANDBOX_PATH/.t0-probe"
echo probe > "$PROBE" || die_setup "sandbox is not writable: $SANDBOX_PATH"
rm -f "$PROBE"

# ---------------------------------------------------------------------------
# Run prove on the selected list.
# ---------------------------------------------------------------------------
echo "[t0] running pjdfstest from sandbox"
echo "[t0]   pjdfstest:    $PJDFSTEST_DIR"
echo "[t0]   sandbox:      $SANDBOX_PATH"
echo "[t0]   profile:      $SELECT_PROFILE"
echo "[t0]   selected:     ${#SELECTED_TESTS[@]} / ${#ALL_TESTS[@]} test files"
echo "[t0]   skipped by:   include=$SKIPPED_BY_INCLUDE exclude=$SKIPPED_BY_EXCLUDE profile=$SKIPPED_BY_PROFILE"

# pjdfstest test scripts walk upward from their own location to find the
# `pjdfstest` binary and the conf file, so we only need to set cwd to the
# sandbox (where temp files land) and feed prove the absolute test paths.
set +e
(
	cd "$SANDBOX_PATH"
	xargs -a "$SELECTED_FILE" prove
) >"$PROVE_LOG" 2>&1
PROVE_EXIT=$?
set -e

# Extract the "Test Summary Report" block and the final aggregate lines.
awk '
	/^Test Summary Report/ { in_summary = 1 }
	in_summary { print }
' "$PROVE_LOG" > "$PROVE_SUMMARY" || true

FAILED_LIST=()
while IFS= read -r line; do
	trimmed="${line#"${line%%[![:space:]]*}"}"
	case "$trimmed" in
		*".t "*"(Wstat:"*)
			path="${trimmed%% (Wstat:*}"
			# prove right-pads the path with spaces so the "(Wstat:"
			# columns align across files. Strip the trailing padding
			# so literal manifest globs (e.g. `chmod/00.t`) match.
			path="${path%"${path##*[![:space:]]}"}"
			FAILED_LIST+=("$path")
			;;
	esac
done < "$PROVE_SUMMARY"

EXPECTED_FAILS=()
BLOCKED_FAILS=()
CALLER_IDENTITY_FAILS=()
UNEXPECTED_FAILS=()
UNEXPECTED_ABS=()
for abs in "${FAILED_LIST[@]+${FAILED_LIST[@]}}"; do
	rel="${abs#"$PJDFSTEST_DIR/tests/"}"
	if [[ "$rel" == "$abs" ]]; then
		rel="$abs"
	fi
	if matches_any "$rel" "${EXPECTED_PATTERNS[@]+${EXPECTED_PATTERNS[@]}}"; then
		EXPECTED_FAILS+=("$rel")
	elif matches_any "$rel" "${BLOCKED_PATTERNS[@]+${BLOCKED_PATTERNS[@]}}"; then
		BLOCKED_FAILS+=("$rel")
	elif matches_any "$rel" "${CALLER_IDENTITY_PATTERNS[@]+${CALLER_IDENTITY_PATTERNS[@]}}"; then
		CALLER_IDENTITY_FAILS+=("$rel")
	else
		UNEXPECTED_FAILS+=("$rel")
		UNEXPECTED_ABS+=("$abs")
	fi
done

PROVE_FILES_LINE="$(grep -E '^Files=' "$PROVE_LOG" | tail -1 || true)"
PROVE_RESULT_LINE="$(grep -E '^Result: ' "$PROVE_LOG" | tail -1 || true)"

# ---------------------------------------------------------------------------
# Optional verbose rerun of unexpected fails. We rerun only the unexpected
# bucket: expected and blocked decisions stand without TAP-level detail.
# ---------------------------------------------------------------------------
RERUN_DID_RUN=0
if (( RERUN_VERBOSE )) && (( ${#UNEXPECTED_ABS[@]} > 0 )); then
	echo "[t0] re-running ${#UNEXPECTED_ABS[@]} unexpected file(s) with prove -v"
	set +e
	(
		cd "$SANDBOX_PATH"
		prove -v "${UNEXPECTED_ABS[@]}"
	) >"$RERUN_VERBOSE_LOG" 2>&1
	set -e
	RERUN_DID_RUN=1
fi

emit_report() {
	local out_fd="$1"
	{
		echo "SkillFS T0/T0.1 — External POSIX Harness Report"
		echo "==============================================="
		echo "repo:           $REPO_ROOT"
		echo "skillfs build:  $PROFILE ($SKILLFS_BIN)"
		echo "pjdfstest:      $PJDFSTEST_DIR"
		echo "sandbox:        $SANDBOX_PATH"
		echo "profile:        $SELECT_PROFILE"
		echo "selected:       ${#SELECTED_TESTS[@]} / ${#ALL_TESTS[@]} test files"
		echo "skipped by:     include=$SKIPPED_BY_INCLUDE exclude=$SKIPPED_BY_EXCLUDE profile=$SKIPPED_BY_PROFILE"
		echo "prove exit:     $PROVE_EXIT"
		echo "prove log:      $PROVE_LOG"
		if (( RERUN_DID_RUN )); then
			echo "rerun log:      $RERUN_VERBOSE_LOG"
		fi
		echo
		echo "prove summary:"
		if [[ -n "$PROVE_FILES_LINE" ]]; then
			echo "  $PROVE_FILES_LINE"
		else
			echo "  (no Files= line found — prove may have aborted; see prove log)"
		fi
		if [[ -n "$PROVE_RESULT_LINE" ]]; then
			echo "  $PROVE_RESULT_LINE"
		fi
		echo
		local nexp="${#EXPECTED_FAILS[@]}"
		local nblk="${#BLOCKED_FAILS[@]}"
		local nid="${#CALLER_IDENTITY_FAILS[@]}"
		local nun="${#UNEXPECTED_FAILS[@]}"
		echo "expected fails (unsupported surface): $nexp"
		if (( nexp > 0 )); then
			local f
			for f in "${EXPECTED_FAILS[@]}"; do
				echo "  - $f"
			done
		fi
		echo
		echo "blocked fails (depends on unsupported helper): $nblk"
		if (( nblk > 0 )); then
			local f
			for f in "${BLOCKED_FAILS[@]}"; do
				echo "  ~ $f"
			done
		fi
		echo
		echo "caller-identity fails (uid/gid/sticky/lchmod semantics gap): $nid"
		if (( nid > 0 )); then
			local f
			for f in "${CALLER_IDENTITY_FAILS[@]}"; do
				echo "  = $f"
			done
		fi
		echo
		echo "unexpected fails: $nun"
		if (( nun > 0 )); then
			local f
			for f in "${UNEXPECTED_FAILS[@]}"; do
				echo "  ! $f"
			done
		fi
		echo
		if (( nun == 0 )); then
			echo "verdict: OK (no unexpected failures)"
		else
			echo "verdict: UNEXPECTED_FAILURES ($nun real failing files; investigate before promoting to a manifest)"
		fi
	} >&"$out_fd"
}

exec 3>&1
emit_report 3
exec 3>&-

if [[ -n "$REPORT_PATH" ]]; then
	exec 4>"$REPORT_PATH"
	emit_report 4
	exec 4>&-
	echo "[t0] report written to $REPORT_PATH"
fi

if (( ${#UNEXPECTED_FAILS[@]} > 0 )); then
	exit 3
fi
exit 0
