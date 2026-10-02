#!/usr/bin/env bash
#
# install-anolisa.sh — alpha installer for the anolisa CLI (P1-A).
#
# Install flow (post P1-A staging redesign):
#
#   1. Detect mode (from-local | auto-checkout | url-fetch).
#   2. Create $STAGING_ROOT under `mktemp -d`; cleaned up on any exit (incl.
#      ERR via `set -e`) and on common signals.
#   3. Populate $STAGING_ROOT with the FULL final layout under
#      $STAGING_ROOT/bin/anolisa and $STAGING_ROOT/share/anolisa/... by either
#      copying from --from-local / auto-checkout, OR by curl + tar from URLs
#      into the staging root (never the final prefix).
#   4. Verify SHAs of bin/bundle against env-provided values when set;
#      refuse under --strict if any are unset (URL-fetch only).
#   5. If --dry-run: print "would promote $STAGING_ROOT → $PREFIX", list
#      planned files, exit 0 WITHOUT touching $PREFIX.
#   6. Otherwise: promote $STAGING_ROOT into $PREFIX via `cp -a` (merging
#      into any existing prefix). On copy error, leave the partial state
#      visible for the operator (rare — staging already validated).
#
# Nothing is written to $PREFIX until step 6. Failures in steps 2-4 (including
# `--strict` checksum failure) exit non-zero leaving $PREFIX completely
# untouched.
#
# Dry-run details (see also `--help`):
#
#   * URL-fetch + --dry-run is PLAN-ONLY. It prints the resolved URLs and
#     planned operations, then exits 0 WITHOUT downloading or extracting
#     anything. Use a `--from-local` / auto-checkout dry-run to validate
#     real bundle contents.
#   * from-local / auto-checkout + --dry-run does the FULL staging against
#     the local source tree, then prints "would promote" and exits 0
#     without touching $PREFIX.
#
# Three install modes (chosen in this order):
#
#   1. --from-local <path>      Explicit. Stage everything from the local
#                               source tree at <path>.
#   2. Auto-checkout            If the script's own dirname has sibling
#                               `manifests/` and `templates/` directories
#                               (i.e. running from inside a repo checkout),
#                               behave as `--from-local <that path>`. Prints
#                               one INFO line so the user knows.
#   3. URL fetch                When neither of the above applies. Requires
#                               `ANOLISA_BIN_URL` and
#                               `ANOLISA_MANIFEST_BUNDLE_URL` to be set, or
#                               to be resolvable from the `ANOLISA_MIRROR` /
#                               `ANOLISA_CHANNEL` defaults. Refuses with a
#                               clear error if any are missing.
#
# Lays out a self-contained ANOLISA install under ${ANOLISA_PREFIX}:
#
#   ${ANOLISA_PREFIX}/bin/anolisa                       (binary)
#   ${ANOLISA_PREFIX}/share/anolisa/manifests/          (osbase)
#
# The distribution index is NOT staged: the CLI downloads and caches it from
# the configured mirror at runtime, so a datadir copy would never be read.
#
# After install, `anolisa env / list / install <component> --dry-run`
# work without the source tree, without overlays, and without any DEMO_ROOT env.
#
# Required env for URL-fetch mode:
#
#   ANOLISA_BIN_URL              binary URL (per-arch / per-os)
#   ANOLISA_MANIFEST_BUNDLE_URL  manifest tarball URL (.tar.gz with
#                                manifests/ and templates/ at the top level
#                                or under a single wrapping directory)
#
# Optional checksum envs (verified with `sha256sum`):
#
#   ANOLISA_BIN_SHA256
#   ANOLISA_MANIFEST_BUNDLE_SHA256
#
# What --strict enforces:
#
#   * Every `ANOLISA_*_SHA256` env above must be set in URL-fetch mode
#     (hard error otherwise).
#
# Pipe-safe:
#   The script never reads from stdin (no interactive prompts), so
#   `curl -fsSL $URL/install-anolisa.sh | bash` is supported. All knobs are
#   env- or flag-driven.

set -euo pipefail

# Default env-driven knobs.
ANOLISA_INSTALL_MODE="${ANOLISA_INSTALL_MODE:-system}"
ANOLISA_CHANNEL="${ANOLISA_CHANNEL:-stable}"
ANOLISA_MIRROR="${ANOLISA_MIRROR:-https://anolisa.oss-cn-hangzhou.aliyuncs.com}"

# ANOLISA_PREFIX defaults depend on install mode. In user-mode we prefer
# ${HOME}/.local so the FHS layout (bin/, share/) lands somewhere the user
# already has on PATH; we still honor an explicit caller-supplied prefix.
case "$ANOLISA_INSTALL_MODE" in
  system) ANOLISA_PREFIX_DEFAULT="/usr/local" ;;
  user)   ANOLISA_PREFIX_DEFAULT="${HOME:-/tmp}/.local" ;;
  *)
    echo "[install-anolisa] unknown ANOLISA_INSTALL_MODE='$ANOLISA_INSTALL_MODE' (expected: system | user)" >&2
    exit 2
    ;;
esac
ANOLISA_PREFIX="${ANOLISA_PREFIX:-$ANOLISA_PREFIX_DEFAULT}"

# Resolve the script's own directory so auto-checkout detection works when
# install-anolisa.sh is invoked from inside a checkout.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANOLISA_SRC_DEFAULT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Parsed args.
FROM_LOCAL=""
DRY_RUN=0
STRICT=0

usage() {
  cat <<EOF
Usage: install-anolisa.sh [OPTIONS]

Stage the anolisa CLI binary + packaged datadir under \${ANOLISA_PREFIX}.

ALL downloads and extractions happen in a temp staging directory.
Nothing is written to \${ANOLISA_PREFIX} until staging + validation pass; a
failure (including --strict checksum failure) leaves \${ANOLISA_PREFIX}
completely untouched.

Three install modes (selected in this order):
  1. --from-local <path>   explicit local source tree
  2. auto-checkout         detected when the script's parent directory has
                           sibling manifests/ and templates/
  3. URL fetch             everything pulled via curl from the mirror

Options:
  --from-local <path>   Path to an anolisa source tree (the directory containing
                        manifests/ and templates/).
  --dry-run             Do not write to \${ANOLISA_PREFIX}. Behavior depends on
                        the install mode:
                          * --from-local / auto-checkout: full staging into a
                            tempdir, then prints "would promote" and exits 0.
                          * URL-fetch: PLAN-ONLY. Prints resolved URLs and the
                            planned operations, then exits 0 WITHOUT curl or
                            tar (use a local-mode dry-run for that).
  --strict              Refuse to finish if checksums are missing: any unset
                        ANOLISA_*_SHA256 env in URL-fetch mode is a hard
                        error (binary / manifest bundle).
  -h, --help            Show this help text and exit.

Environment overrides:
  ANOLISA_PREFIX                  install prefix (default: /usr/local; ${HOME}/.local in user mode)
  ANOLISA_INSTALL_MODE            system | user (default: system)
  ANOLISA_CHANNEL                 release channel (default: stable)
  ANOLISA_MIRROR                  artifact mirror base URL
                                  (default: https://anolisa.oss-cn-hangzhou.aliyuncs.com)
  ANOLISA_BIN_URL                 explicit binary URL
                                  (default: \${ANOLISA_MIRROR}/releases/\${ANOLISA_CHANNEL}/anolisa-<arch>-<os>)
  ANOLISA_MANIFEST_BUNDLE_URL     explicit manifest bundle URL (.tar.gz)
                                  (default: \${ANOLISA_MIRROR}/releases/\${ANOLISA_CHANNEL}/manifests-latest.tar.gz)
  ANOLISA_BIN_SHA256              optional sha256 to verify the fetched binary against
  ANOLISA_MANIFEST_BUNDLE_SHA256  optional sha256 for the manifest bundle
  ANOLISA_DATA_DIR                read at runtime to override the packaged datadir

URL-fetch mode notes:
  * The default manifest bundle path uses manifests-latest.tar.gz. Pin to a
    specific release by setting ANOLISA_MANIFEST_BUNDLE_URL explicitly.
  * The distribution index is not staged: the CLI downloads and caches it
    from the configured mirror at runtime.

Examples:
  # Auto-detected install from a checkout (running this file from scripts/).
  sudo bash install-anolisa.sh

  # User-mode install into ~/.local (no privilege required).
  ANOLISA_INSTALL_MODE=user bash install-anolisa.sh

  # Stage everything under a tmp prefix for smoke testing.
  ANOLISA_PREFIX=/tmp/anolisa-stage bash install-anolisa.sh --dry-run

  # URL-fetch on a fresh ECS box (no checkout). Defaults pull from
  # the OSS mirror; override individual URLs for a private mirror.
  curl -fsSL https://anolisa.oss-cn-hangzhou.aliyuncs.com/releases/stable/install-anolisa.sh \\
    | ANOLISA_INSTALL_MODE=user bash
EOF
}

log() {
  echo "[install-anolisa] $*"
}

warn() {
  echo "[install-anolisa] WARN: $*" >&2
}

err() {
  echo "[install-anolisa] ERROR: $*" >&2
}

# Parse args.
while [ $# -gt 0 ]; do
  case "$1" in
    --from-local)
      [ $# -ge 2 ] || { err "--from-local requires a path"; exit 2; }
      FROM_LOCAL="$2"
      shift 2
      ;;
    --from-local=*)
      FROM_LOCAL="${1#--from-local=}"
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      err "unknown argument: $1"
      usage >&2
      exit 2
      ;;
  esac
done

# ---- Mode selection ---------------------------------------------------------
#
# MODE is one of: from-local (explicit), auto-checkout, url-fetch
MODE=""
if [ -n "$FROM_LOCAL" ]; then
  MODE="from-local"
elif [ -d "$ANOLISA_SRC_DEFAULT/manifests" ] && [ -d "$ANOLISA_SRC_DEFAULT/templates" ]; then
  MODE="auto-checkout"
  FROM_LOCAL="$ANOLISA_SRC_DEFAULT"
  log "INFO: detected checkout at $FROM_LOCAL, using local staging."
else
  MODE="url-fetch"
fi

# Validate from-local source layout up front.
if [ -n "$FROM_LOCAL" ]; then
  if [ ! -d "$FROM_LOCAL/manifests" ] || [ ! -d "$FROM_LOCAL/templates" ]; then
    err "--from-local source does not look like an anolisa checkout: $FROM_LOCAL"
    err "expected manifests/ and templates/ subdirectories"
    exit 2
  fi
fi

# ---- URL composition (used in url-fetch mode) -------------------------------
#
# Default per-channel paths; callers can override any of these by setting
# the matching ANOLISA_*_URL env explicitly. We do not fabricate fake paths:
# the OSS MIRROR default is the real bucket; per-channel artifact paths are
# the documented placeholders for what P1-J will publish.

resolve_target_triple_os() {
  case "$(uname -s)" in
    Linux)  echo "linux" ;;
    Darwin) echo "darwin" ;;
    *)      uname -s | tr 'A-Z' 'a-z' ;;
  esac
}

resolve_target_triple_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *) uname -m ;;
  esac
}

default_bin_url() {
  local os arch
  os="$(resolve_target_triple_os)"
  arch="$(resolve_target_triple_arch)"
  echo "$ANOLISA_MIRROR/releases/$ANOLISA_CHANNEL/anolisa-${arch}-${os}"
}

default_manifest_bundle_url() {
  # `manifests-latest.tar.gz` per `--help`. Override with
  # ANOLISA_MANIFEST_BUNDLE_URL=...manifests-X.Y.Z.tar.gz to pin to a release.
  echo "$ANOLISA_MIRROR/releases/$ANOLISA_CHANNEL/manifests-latest.tar.gz"
}

ANOLISA_BIN_URL_EXPLICIT=0
ANOLISA_MANIFEST_BUNDLE_URL_EXPLICIT=0
[ -n "${ANOLISA_BIN_URL:-}" ] && ANOLISA_BIN_URL_EXPLICIT=1
[ -n "${ANOLISA_MANIFEST_BUNDLE_URL:-}" ] && ANOLISA_MANIFEST_BUNDLE_URL_EXPLICIT=1

ANOLISA_BIN_URL="${ANOLISA_BIN_URL:-$(default_bin_url)}"
ANOLISA_MANIFEST_BUNDLE_URL="${ANOLISA_MANIFEST_BUNDLE_URL:-$(default_manifest_bundle_url)}"

# In strict mode we require checksum envs *for whichever fetches we will
# actually perform*. In from-local / auto-checkout modes nothing is fetched
# so the checksum envs are not required.
if [ "$STRICT" -eq 1 ] && [ "$MODE" = "url-fetch" ]; then
  missing=()
  [ -z "${ANOLISA_BIN_SHA256:-}" ] && missing+=("ANOLISA_BIN_SHA256")
  [ -z "${ANOLISA_MANIFEST_BUNDLE_SHA256:-}" ] && missing+=("ANOLISA_MANIFEST_BUNDLE_SHA256")
  if [ "${#missing[@]}" -gt 0 ]; then
    err "--strict mode requires checksum envs to be set:"
    for v in "${missing[@]}"; do
      err "  $v"
    done
    exit 2
  fi
fi

# ---- Final install targets (where step 6 promotion lands) ------------------
FINAL_BIN_DIR="$ANOLISA_PREFIX/bin"
FINAL_DATADIR="$ANOLISA_PREFIX/share/anolisa"
FINAL_MANIFESTS_DIR="$FINAL_DATADIR/manifests"
FINAL_BIN_DEST="$FINAL_BIN_DIR/anolisa"

log "mode            : $MODE"
log "install mode    : $ANOLISA_INSTALL_MODE"
log "prefix          : $ANOLISA_PREFIX (final)"
log "channel         : $ANOLISA_CHANNEL"
log "mirror          : $ANOLISA_MIRROR"
log "strict          : $STRICT"
log "dry-run         : $DRY_RUN"
log "binary target   : $FINAL_BIN_DEST"
log "packaged datadir: $FINAL_DATADIR"
if [ "$MODE" = "url-fetch" ]; then
  log "binary URL      : $ANOLISA_BIN_URL"
  log "manifest bundle : $ANOLISA_MANIFEST_BUNDLE_URL"
else
  log "source tree     : $FROM_LOCAL"
fi

# ---- URL-fetch + dry-run short-circuit -------------------------------------
#
# Per the redesign: dry-run for URL-fetch is plan-only. We do NOT curl and do
# NOT tar. If you want the full extraction dry-run, use --from-local /
# auto-checkout (where the bundle contents already exist locally and can be
# validated without network).
if [ "$MODE" = "url-fetch" ] && [ "$DRY_RUN" -eq 1 ]; then
  log "INFO: URL-fetch + --dry-run is plan-only (no curl, no tar)."
  log "      use --from-local for a dry-run that validates bundle contents."
  log "would fetch binary  : $ANOLISA_BIN_URL"
  log "would fetch bundle  : $ANOLISA_MANIFEST_BUNDLE_URL"
  log "would stage under   : <mktemp staging dir> (auto-removed on exit)"
  log "would write to prefix:"
  log "  $FINAL_BIN_DEST"
  log "  $FINAL_MANIFESTS_DIR/osbase/"
  exit 0
fi

# ---- $STAGING_ROOT: everything is built here first --------------------------
#
# Cleaned up on any exit (including ERR via `set -e`) and common signals.
# Promotion to $ANOLISA_PREFIX is the LAST step; until then, the final prefix
# is never touched.
STAGING_ROOT=""
cleanup_staging() {
  if [ -n "${STAGING_ROOT:-}" ] && [ -d "$STAGING_ROOT" ]; then
    rm -rf "$STAGING_ROOT"
  fi
}
trap cleanup_staging EXIT INT TERM HUP

STAGING_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/anolisa-stage.XXXXXX")"
log "staging root    : $STAGING_ROOT"

# Staging targets (mirror the final layout under STAGING_ROOT). All steps 3-4
# read/write through these; only step 6 (promotion) touches FINAL_*.
BIN_DIR="$STAGING_ROOT/bin"
DATADIR="$STAGING_ROOT/share/anolisa"
MANIFESTS_DIR="$DATADIR/manifests"
BIN_DEST="$BIN_DIR/anolisa"

# Download workspace lives under the staging root so cleanup is unified.
DOWNLOAD_DIR="$STAGING_ROOT/.download"

mkdir -p "$BIN_DIR" "$MANIFESTS_DIR" "$DOWNLOAD_DIR"

# ---- sha256 verification helper --------------------------------------------
#
# Picks whichever of `sha256sum` or `shasum -a 256` is available. Returns
# non-zero if neither is on PATH.
sha256_of() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    err "neither sha256sum nor shasum found on PATH; cannot verify checksums"
    return 127
  fi
}

verify_sha256() {
  # verify_sha256 <file> <expected-hex> <label>
  local file="$1" expected="$2" label="$3"
  if [ -z "$expected" ]; then
    return 0
  fi
  local actual
  actual="$(sha256_of "$file")"
  if [ "$actual" != "$expected" ]; then
    err "$label sha256 mismatch:"
    err "  expected: $expected"
    err "  actual  : $actual"
    return 1
  fi
  log "verified $label sha256 ($expected)"
}

# ---- Tar bundle safety -----------------------------------------------------
#
# We are about to extract an attacker-influenceable tarball into the staging
# tree, then copy parts of it into the staged datadir we promote to $PREFIX.
# Even though staging lives in `mktemp -d`, layered defense matters: a
# malicious or corrupt bundle could
# try absolute-path entries (`/etc/passwd`), `..` traversal
# (`../../etc/something`), or symlinks / hardlinks / device / fifo / socket
# entries. We refuse such bundles BEFORE invoking `tar -xz`. After
# extraction we also realpath-confine every path we walk into to the
# extraction root, in case some unusual encoding slipped past the
# entry-name parser.

# verify_tar_bundle_safe <bundle-path>
#
# Inspects the bundle without extracting it. Exits 1 (returns non-zero)
# if any entry has:
#   * an absolute path (`/...`)
#   * `..` as a path component (`..`, `../foo`, `foo/..`, `foo/../bar`)
#   * an empty / whitespace-only name
#   * a type other than regular file (`-`) or directory (`d`) — i.e. any
#     symlink (`l`), hardlink (`h`/`L`), device (`c`/`b`), fifo (`p`), or
#     socket (`s`).
# The first character of each `tar -tvzf` line is the entry type; this
# parsing works for both GNU tar and bsdtar.
verify_tar_bundle_safe() {
  local bundle="$1"
  if ! command -v tar >/dev/null 2>&1; then
    err "tar not found on PATH; cannot validate bundle: $bundle"
    return 1
  fi

  # 1. Reject unsafe entry NAMES.
  local names
  if ! names="$(tar -tzf "$bundle" 2>/dev/null)"; then
    err "failed to list entries of bundle: $bundle"
    return 1
  fi
  local bad_names=""
  while IFS= read -r name; do
    # Empty / whitespace-only entry name.
    local trimmed
    trimmed="$(printf %s "$name" | tr -d '[:space:]')"
    if [ -z "$trimmed" ]; then
      bad_names="${bad_names}  empty entry name"$'\n'
      continue
    fi
    # Absolute path.
    case "$name" in
      /*)
        bad_names="${bad_names}  absolute path: $name"$'\n'
        continue
        ;;
    esac
    # `..` as a path component (start, end, middle, or whole).
    case "$name" in
      ..|../*|*/..|*/../*)
        bad_names="${bad_names}  .. traversal: $name"$'\n'
        ;;
    esac
  done <<EOF
$names
EOF
  if [ -n "$bad_names" ]; then
    err "manifest bundle has unsafe entry names (bundle URL: $ANOLISA_MANIFEST_BUNDLE_URL):"
    printf '%s' "$bad_names" >&2
    return 1
  fi

  # 2. Reject unsafe entry TYPES.
  local verbose
  if ! verbose="$(tar -tvzf "$bundle" 2>/dev/null)"; then
    err "failed to list verbose entries of bundle: $bundle"
    return 1
  fi
  local bad_types=""
  while IFS= read -r row; do
    [ -z "$row" ] && continue
    local t="${row:0:1}"
    case "$t" in
      -|d)
        : # regular file / directory — allowed
        ;;
      l)
        bad_types="${bad_types}  symlink: $row"$'\n'
        ;;
      h|L)
        bad_types="${bad_types}  hardlink: $row"$'\n'
        ;;
      c|b)
        bad_types="${bad_types}  device file: $row"$'\n'
        ;;
      p)
        bad_types="${bad_types}  fifo: $row"$'\n'
        ;;
      s)
        bad_types="${bad_types}  socket: $row"$'\n'
        ;;
      *)
        bad_types="${bad_types}  unknown entry type ($t): $row"$'\n'
        ;;
    esac
  done <<EOF
$verbose
EOF
  if [ -n "$bad_types" ]; then
    err "manifest bundle has unsafe entry types (bundle URL: $ANOLISA_MANIFEST_BUNDLE_URL):"
    printf '%s' "$bad_types" >&2
    return 1
  fi
}

# realpath_of <path> — print canonical absolute path for <path> to stdout,
# or empty string on failure. Tries `readlink -f` (GNU coreutils, also
# available on Linux busybox); falls back to a python3 one-liner so the
# script remains portable to macOS dev environments where the BSD
# `readlink` lacks `-f`. python3 is the documented fallback dep.
realpath_of() {
  local p="$1"
  if [ -z "$p" ]; then
    return 1
  fi
  if command -v readlink >/dev/null 2>&1 && readlink -f / >/dev/null 2>&1; then
    local out
    out="$(readlink -f -- "$p" 2>/dev/null)" || true
    if [ -n "$out" ]; then
      printf '%s\n' "$out"
      return 0
    fi
  fi
  if command -v python3 >/dev/null 2>&1; then
    local out
    out="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$p" 2>/dev/null)" || true
    if [ -n "$out" ]; then
      printf '%s\n' "$out"
      return 0
    fi
  fi
  # Last resort: cd + pwd -P only works for directories that exist.
  if [ -d "$p" ]; then
    local out
    out="$(cd "$p" 2>/dev/null && pwd -P)" || true
    if [ -n "$out" ]; then
      printf '%s\n' "$out"
      return 0
    fi
  fi
  return 1
}

# realpath_inside <path> <root_real>
#
# Returns 0 iff realpath(<path>) equals <root_real> or starts with
# "<root_real>/". <root_real> MUST already be canonicalized by the caller
# (we do not re-resolve it here so callers can resolve once and reuse).
realpath_inside() {
  local target="$1" root_real="$2"
  local target_real
  target_real="$(realpath_of "$target")"
  if [ -z "$target_real" ] || [ -z "$root_real" ]; then
    err "realpath_inside: failed to resolve real paths (target=$target root=$root_real)"
    return 1
  fi
  case "$target_real" in
    "$root_real"|"$root_real"/*)
      return 0
      ;;
  esac
  err "realpath confinement violation: $target_real escapes $root_real"
  return 1
}

# ---- Step 3: staging (manifests + templates) --------------------------------
#
# Both stage_from_local and stage_from_url write into $STAGING_ROOT. They
# always execute (no DRY_RUN gating) — the staging tree is a tempdir, so the
# operations are safe.
#
# Only osbase manifests are staged. The dev-tree runtime/ manifests and the
# distribution-index/ directory were retired: component manifests are
# downloaded from the distribution backend at install time, and the CLI
# fetches + caches the distribution index from the mirror at runtime, so a
# datadir copy of either would never be read.

stage_from_local() {
  for subdir in osbase; do
    local src="$FROM_LOCAL/manifests/$subdir"
    if [ ! -d "$src" ]; then
      warn "$src missing; skipping"
      continue
    fi
    local dest="$MANIFESTS_DIR/$subdir"
    log "stage manifests/$subdir → $dest"
    rm -rf "$dest"
    mkdir -p "$dest"
    # `cp -R src/. dest` copies contents (incl. dotfiles) without nesting.
    cp -R "$src/." "$dest/"
    find "$dest" -type d -exec chmod 0755 {} \;
    find "$dest" -type f -exec chmod 0644 {} \;
  done

  # Optional SPEC.md doc — informational, mirrors what a packaged distro
  # would ship next to the manifests.
  if [ -f "$FROM_LOCAL/manifests/SPEC.md" ]; then
    log "stage manifests/SPEC.md"
    cp "$FROM_LOCAL/manifests/SPEC.md" "$MANIFESTS_DIR/SPEC.md"
    chmod 0644 "$MANIFESTS_DIR/SPEC.md"
  fi
}

stage_from_url() {
  if ! command -v curl >/dev/null 2>&1; then
    err "URL-fetch mode requires curl on PATH"
    exit 2
  fi
  if ! command -v tar >/dev/null 2>&1; then
    err "URL-fetch mode requires tar on PATH"
    exit 2
  fi

  # ---- manifest bundle ----
  local bundle_path="$DOWNLOAD_DIR/manifests.tar.gz"
  log "fetching manifest bundle: $ANOLISA_MANIFEST_BUNDLE_URL"
  curl --fail --location --silent --show-error \
    --output "$bundle_path" \
    "$ANOLISA_MANIFEST_BUNDLE_URL"

  if [ -n "${ANOLISA_MANIFEST_BUNDLE_SHA256:-}" ]; then
    verify_sha256 "$bundle_path" "$ANOLISA_MANIFEST_BUNDLE_SHA256" "manifest bundle" || exit 1
  else
    warn "ANOLISA_MANIFEST_BUNDLE_SHA256 not set; skipping bundle checksum (pass --strict to refuse)"
  fi

  # Pre-extraction safety gate: reject absolute paths, `..` traversal,
  # symlinks, hardlinks, and device / fifo / socket entries. See
  # verify_tar_bundle_safe() above for the full rule set. We do this
  # BEFORE `tar -xz` so a malicious bundle never touches the filesystem.
  if ! verify_tar_bundle_safe "$bundle_path"; then
    err "refusing to extract unsafe manifest bundle"
    exit 1
  fi

  # Extract bundle into a fresh empty subdir of the staging root; from
  # there we copy manifests/<subdir> into the staged layout. The bundle is
  # expected to contain manifests/ and templates/ at the top level, or
  # under a single wrapping directory (auto-unwrap).
  #
  # Tar option rationale (script's runtime target is Linux/GNU tar; we
  # degrade gracefully on bsdtar — macOS dev environments):
  #   --no-same-owner          : do not honor the bundle's recorded uid/gid;
  #                              avoid chown-to-root from a hostile bundle.
  #                              Accepted by both GNU tar and bsdtar.
  #   --no-same-permissions    : drop the bundle's umask-bypass and any
  #                              setuid/setgid bits. Accepted by both
  #                              GNU tar and bsdtar.
  #   --no-overwrite-dir       : GNU-only; refuse to replace an existing
  #                              directory's metadata. Feature-detected via
  #                              `tar --help`; omitted on bsdtar where the
  #                              option does not exist.
  # We intentionally do NOT pass any option that dereferences symlinks
  # (no -h / --dereference), and we already rejected symlink entries
  # above for layered defense.
  local bundle_extract_root="$DOWNLOAD_DIR/bundle"
  mkdir -p "$bundle_extract_root"
  local tar_opts=(--no-same-owner --no-same-permissions)
  if tar --help 2>&1 | grep -q -- '--no-overwrite-dir'; then
    tar_opts+=(--no-overwrite-dir)
  fi
  tar "${tar_opts[@]}" -xzf "$bundle_path" -C "$bundle_extract_root"

  # Auto-unwrap: if the tar produced a single directory that itself
  # contains manifests/ + templates/, descend into it.
  local bundle_stage="$bundle_extract_root"
  if [ ! -d "$bundle_stage/manifests" ] || [ ! -d "$bundle_stage/templates" ]; then
    # Look for exactly one subdir that has manifests/ and templates/.
    local candidate
    candidate="$(find "$bundle_stage" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -n 1)"
    if [ -n "$candidate" ] \
       && [ -d "$candidate/manifests" ] \
       && [ -d "$candidate/templates" ]; then
      bundle_stage="$candidate"
    else
      err "manifest bundle does not contain manifests/ and templates/ at the top level"
      err "extracted contents: $(ls "$bundle_stage" 2>/dev/null | tr '\n' ' ')"
      exit 1
    fi
  fi

  # Post-extraction realpath confinement: every path we are about to copy
  # out of $bundle_extract_root must resolve back inside it. We resolve
  # the extraction root once and reuse it. `find -P` (the default) does
  # NOT follow symlinks, so this walk is safe even if a symlink slipped
  # past the type check above. This is the layered-defense check.
  local bundle_root_real
  bundle_root_real="$(realpath_of "$bundle_extract_root")"
  if [ -z "$bundle_root_real" ]; then
    err "could not resolve realpath of bundle extraction root: $bundle_extract_root"
    exit 1
  fi
  # Confine the top-level dirs we are going to copy.
  for sub in manifests templates; do
    if ! realpath_inside "$bundle_stage/$sub" "$bundle_root_real"; then
      err "manifest bundle '$sub' escapes extraction root; refusing to copy"
      err "bundle URL: $ANOLISA_MANIFEST_BUNDLE_URL"
      exit 1
    fi
  done
  # Walk every extracted path and confine each one. `find -P` (default)
  # does not follow symlinks; combined with the entry-type rejection above,
  # this is belt-and-suspenders.
  local offender=""
  while IFS= read -r entry; do
    [ -z "$entry" ] && continue
    local entry_real
    entry_real="$(realpath_of "$entry")"
    case "$entry_real" in
      "$bundle_root_real"|"$bundle_root_real"/*)
        : # inside extraction root — ok
        ;;
      *)
        offender="$entry (realpath: $entry_real)"
        break
        ;;
    esac
  done < <(find "$bundle_extract_root" -mindepth 1 2>/dev/null)
  if [ -n "$offender" ]; then
    err "realpath confinement violation in manifest bundle:"
    err "  $offender"
    err "  extraction root: $bundle_root_real"
    err "  bundle URL: $ANOLISA_MANIFEST_BUNDLE_URL"
    exit 1
  fi

  # Copy manifests subdirs verbatim from the extracted bundle.
  for subdir in osbase; do
    local src="$bundle_stage/manifests/$subdir"
    if [ ! -d "$src" ]; then
      warn "$src missing in manifest bundle; skipping"
      continue
    fi
    local dest="$MANIFESTS_DIR/$subdir"
    log "stage manifests/$subdir → $dest"
    rm -rf "$dest"
    mkdir -p "$dest"
    cp -R "$src/." "$dest/"
  done

  if [ -f "$bundle_stage/manifests/SPEC.md" ]; then
    cp "$bundle_stage/manifests/SPEC.md" "$MANIFESTS_DIR/SPEC.md"
  fi
}

if [ "$MODE" = "url-fetch" ]; then
  stage_from_url
else
  stage_from_local
fi

# ---- Step 3 (cont'd): binary staging ---------------------------------------
#
# In from-local / auto-checkout modes we prefer the locally-built release
# binary (building it on the fly if missing); ANOLISA_BIN_URL is honored as
# an opt-in even from a checkout. In url-fetch mode we always curl the
# binary.

stage_bin_from_url() {
  log "fetching binary: $ANOLISA_BIN_URL"
  if ! command -v curl >/dev/null 2>&1; then
    err "binary fetch requires curl on PATH"
    exit 2
  fi
  local tmp_bin="$DOWNLOAD_DIR/anolisa"
  curl --fail --location --silent --show-error \
    --output "$tmp_bin" \
    "$ANOLISA_BIN_URL"
  if [ -n "${ANOLISA_BIN_SHA256:-}" ]; then
    verify_sha256 "$tmp_bin" "$ANOLISA_BIN_SHA256" "binary" || exit 1
  else
    warn "ANOLISA_BIN_SHA256 not set; skipping binary checksum (pass --strict to refuse)"
  fi
  install -m 0755 "$tmp_bin" "$BIN_DEST"
}

stage_bin_from_local() {
  local local_bin="$FROM_LOCAL/target/release/anolisa"
  if [ ! -x "$local_bin" ]; then
    log "release binary missing at $local_bin; running cargo build --release -p anolisa-cli"
    (cd "$FROM_LOCAL" && cargo build --release -p anolisa-cli)
  fi
  if [ ! -x "$local_bin" ]; then
    err "cargo build did not produce $local_bin"
    exit 1
  fi
  install -m 0755 "$local_bin" "$BIN_DEST"
}

case "$MODE" in
  url-fetch)
    stage_bin_from_url
    ;;
  from-local|auto-checkout)
    # In a local-staging mode the caller can still force a URL fetch by
    # setting ANOLISA_BIN_URL explicitly (captured above). Otherwise we
    # build / copy the binary from the local checkout.
    if [ "$ANOLISA_BIN_URL_EXPLICIT" -eq 1 ]; then
      stage_bin_from_url
    else
      stage_bin_from_local
    fi
    ;;
esac

# ---- Step 5: dry-run gate (local/auto-checkout dry-run stops here) ---------
#
# URL-fetch + dry-run already exited above; reaching here in dry-run mode
# implies we have a fully-staged tree from a local source. Print the
# promotion plan and exit 0 without touching $ANOLISA_PREFIX.
if [ "$DRY_RUN" -eq 1 ]; then
  log "dry-run: staging complete. would promote:"
  log "  $STAGING_ROOT/  →  $ANOLISA_PREFIX/"
  if command -v find >/dev/null 2>&1; then
    # List staged files (relative to staging root) so the operator sees
    # exactly what would land in $PREFIX. Filter out the .download workspace.
    (cd "$STAGING_ROOT" && find . -type f ! -path './.download/*' | sort) | while IFS= read -r rel; do
      log "  would write: $ANOLISA_PREFIX/${rel#./}"
    done
  fi
  log "dry-run: NOT promoting. \$ANOLISA_PREFIX left untouched."
  exit 0
fi

# ---- Step 6: promote staging → $ANOLISA_PREFIX -----------------------------
#
# Tradeoff note: we use `cp -a` (not `mv`) because $STAGING_ROOT lives under
# TMPDIR and may be on a different filesystem from $ANOLISA_PREFIX. `cp -a`
# is non-atomic per-file but works across filesystems and merges cleanly
# into a pre-existing prefix. If a copy fails partway, we leave the partial
# state visible (staging already validated, so this is rare) so the operator
# can inspect and clean up.
promote_to_prefix() {
  log "promoting staging → $ANOLISA_PREFIX"
  mkdir -p "$ANOLISA_PREFIX"
  # Copy the entire staged layout (bin/, share/) into the final prefix.
  # `cp -a` preserves mode/timestamps. We exclude the .download workspace
  # by copying only the top-level entries we care about.
  for entry in bin share; do
    local src="$STAGING_ROOT/$entry"
    if [ ! -d "$src" ]; then
      continue
    fi
    log "  cp -a $src/. $ANOLISA_PREFIX/$entry/"
    mkdir -p "$ANOLISA_PREFIX/$entry"
    if ! cp -a "$src/." "$ANOLISA_PREFIX/$entry/"; then
      err "promotion failed while copying $src → $ANOLISA_PREFIX/$entry"
      err "staging had already validated, so partial state in $ANOLISA_PREFIX may need"
      err "manual cleanup. Staging tree preserved for inspection: $STAGING_ROOT"
      # Disable the cleanup trap so the operator can inspect the staging tree.
      trap - EXIT INT TERM HUP
      return 1
    fi
    if [ "$entry" = "bin" ]; then
      find "$ANOLISA_PREFIX/$entry" -type d -exec chmod 0755 {} \;
      find "$ANOLISA_PREFIX/$entry" -type f -exec chmod 0755 {} \;
    else
      find "$ANOLISA_PREFIX/$entry" -type d -exec chmod 0755 {} \;
      find "$ANOLISA_PREFIX/$entry" -type f -exec chmod 0644 {} \;
    fi
  done
}

if ! promote_to_prefix; then
  exit 1
fi

log "done"
log "next: ${FINAL_BIN_DEST} --help"
