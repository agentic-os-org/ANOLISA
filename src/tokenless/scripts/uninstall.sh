#!/usr/bin/env bash
# Uninstall a Tokenless CLI installation created by scripts/install.sh.
#
# The installer records every path it created in a receipt file, together with
# the sha256 of each recorded file, the link target each launcher resolves to,
# and an install_id that ownership markers inside the adapter tree and the npm
# module directory repeat. This script removes exactly those recorded paths and
# nothing else, so it stays symmetric with the install and never deletes
# binaries, adapters, or data that belong to another installation method
# (anolisa CLI, a manual `npm install -g`, or a custom TOKENLESS_INSTALL_DIR you
# manage yourself).
#
# Identity is checked, not just content. A later anolisa or npm install of the
# same version reproduces byte-identical binaries and manifests, so a digest
# match alone would let this script delete a newer installation's files, adapter
# tree, framework registrations and npm package. A recorded path whose content no
# longer matches, whose launcher no longer resolves to the recorded target, or
# whose ownership marker belongs to a newer install was taken over after the
# receipt was written, and is left alone.
#
# Usage:
#   bash scripts/uninstall.sh [--dry-run] [--purge] [--receipt <path>]
#
# Options:
#   --dry-run        Print what would be removed, change nothing.
#   --purge          Also delete the runtime data directory (~/.tokenless,
#                    which holds stats.db and stash.db). Off by default so an
#                    uninstall does not destroy collected statistics.
#   --receipt PATH   Read a non-default receipt (mirrors TOKENLESS_RECEIPT).
#
# Environment variables:
#   TOKENLESS_RECEIPT  Receipt path (default:
#                      ${XDG_DATA_HOME:-$HOME/.local/share}/tokenless/install-receipt)
#   TOKENLESS_DATA_DIR Runtime data directory (default: ~/.tokenless)

set -euo pipefail

NPM_PACKAGE="anolisa-tokenless"
DEFAULT_DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
RECEIPT="${TOKENLESS_RECEIPT:-${DEFAULT_DATA_DIR}/tokenless/install-receipt}"
RUNTIME_DATA_DIR="${TOKENLESS_DATA_DIR:-${HOME}/.tokenless}"
MARKER="# Added by tokenless installer"
# Identity anchor inside the npm-owned adapter tree, stamped with the release
# version by npm/scripts/package-npm.js.
ADAPTERS_IDENTITY_FILE="manifest.json"
# Ownership marker written by scripts/install.sh into the artefacts that can
# carry one. It lives where a foreign reinstall removes it, which is what makes
# it evidence rather than a restatement of the content hash.
OWNER_MARKER_FILE=".tokenless-owner"
RECEIPT_SCHEMA_CURRENT=3

DRY_RUN=0
PURGE=0
ADAPTERS_KEPT=0

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; }
die()  { err "$@"; exit 1; }

usage() {
  printf '%s\n' \
    "Usage: bash scripts/uninstall.sh [--dry-run] [--purge] [--receipt <path>]" \
    "" \
    "  --dry-run        Print what would be removed, change nothing." \
    "  --purge          Also delete the runtime data directory (~/.tokenless)." \
    "  --receipt PATH   Read a non-default receipt (mirrors TOKENLESS_RECEIPT)." \
    "  -h, --help       Show this help."
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)  DRY_RUN=1; shift ;;
    --purge)    PURGE=1; shift ;;
    --receipt)  [ "$#" -ge 2 ] || die "--receipt requires a path"; RECEIPT="$2"; shift 2 ;;
    -h|--help)  usage; exit 0 ;;
    *)          usage >&2; die "Unknown option: $1" ;;
  esac
done

run() {
  if [ "$DRY_RUN" = "1" ]; then
    info "[dry-run] $*"
  else
    "$@"
  fi
}

# Portable absolute-path resolution. GNU readlink(1) has -f; the BSD readlink
# shipped with macOS only gained it in 12.3 and prints nothing where it is
# missing, which every caller here would read as "not our link". Walk the
# symlink chain and normalise with `cd -P` instead.
resolve_path() {
  local p="$1" out="" i=0 target dir base
  if out=$(readlink -f -- "$p" 2>/dev/null) && [ -n "$out" ]; then
    printf '%s\n' "$out"
    return 0
  fi
  while [ -L "$p" ] && [ "$i" -lt 32 ]; do
    target=$(readlink "$p" 2>/dev/null) || return 1
    case "$target" in
      /*) p="$target" ;;
      *)  p="$(dirname "$p")/$target" ;;
    esac
    i=$((i + 1))
  done
  [ -e "$p" ] || return 1
  dir=$(dirname "$p")
  base=$(basename "$p")
  out=$(cd "$dir" 2>/dev/null && pwd -P) || return 1
  printf '%s/%s\n' "$out" "$base"
}

owner_marker_read() {
  [ -f "$1" ] || return 1
  head -n 1 "$1" 2>/dev/null
}

# Ownership is the recorded identity, not merely the recorded content: a later
# anolisa or npm install of the same version reproduces byte-identical binaries,
# so a launcher also has to resolve to the target the installer linked it to. An
# empty recorded target means a regular file was written there (source build),
# and a symlink in that spot is therefore somebody else's launcher.
owned_by_receipt() {
  local path="$1" target="$2" resolved
  if [ -n "$target" ]; then
    [ -L "$path" ] || return 1
    resolved=$(resolve_path "$path" 2>/dev/null || true)
    [ -n "$resolved" ] && [ "$resolved" = "$target" ] && return 0
    return 1
  fi
  if [ -L "$path" ]; then
    return 1
  fi
  return 0
}

# True when the receipt carries schema 3 fields, i.e. when the identity checks
# above have evidence to work with. Older receipts record content only, and are
# handled exactly as before rather than being refused.
receipt_schema_at_least() {
  case "${SCHEMA:-}" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$SCHEMA" -ge "$1" ]
}

# sha256 of a file's content, following symlinks. Prints nothing when the file
# is unreadable or no sha256 tool exists, which callers treat as "no identity".
file_digest() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    { sha256sum "$f" 2>/dev/null || true; } | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    { shasum -a 256 "$f" 2>/dev/null || true; } | cut -d' ' -f1
  fi
  return 0
}

# There is deliberately no cleanup for an @anolisa platform package here. Real
# npm nests that dependency under the root package's own node_modules, so the
# `npm uninstall -g` below already removes it (it reports "removed 2 packages"),
# and the manual fallback removes the root module directory, which carries the
# nested payload with it. A sibling ${prefix}/lib/node_modules/@anolisa directory
# is never this receipt's: it can only be a package the user installed globally
# on their own, and deleting it because its name matched this machine's platform
# key destroyed that unrelated install while still reporting success.
# Frameworks register the adapter tree by reference — plugin directories, hook
# entries and symlinks that point into it. Deleting the tree first leaves those
# registrations dangling against a path that no longer exists, and the CLI they
# call is already gone, so each bundled adapter's own uninstall.sh runs before
# its resources are removed.
#
# A framework that could not be deregistered is a failure, not a warning: the
# caller is about to delete the resources that registration points at, and once
# they are gone the registration dangles against a path that no longer exists —
# along with the very script this function suggests re-running. Returns non-zero
# when any framework failed, so the caller can keep the tree and let the user
# retry.
deregister_framework_adapters() {
  local adapters_dir="$1" script framework output status failed=0
  [ -d "$adapters_dir" ] || return 0
  for script in "$adapters_dir"/*/scripts/uninstall.sh; do
    [ -f "$script" ] || continue
    framework=$(basename "$(dirname "$(dirname "$script")")")
    if [ "$DRY_RUN" = "1" ]; then
      info "[dry-run] would deregister the ${framework} adapter via ${script}"
      continue
    fi
    # Deregistration only. These are the adapters' full uninstall scripts, and
    # at least the Codex one also removes ${PREFIX}/bin/tokenless. Step 1 above
    # has already decided what happens to that binary — it keeps it when the
    # recorded digest says another installation took the path over — so the
    # sub-script must not get a second chance at it: TOKENLESS_DEREGISTER_ONLY=1
    # limits it to the framework registration. stdin is closed as well, so an
    # interactive prompt can neither block the run nor vanish into the captured
    # output.
    output=$(TOKENLESS_DEREGISTER_ONLY=1 bash "$script" </dev/null 2>&1) && status=0 || status=$?
    if [ "$status" -ne 0 ]; then
      failed=$((failed + 1))
      warn "Could not deregister the ${framework} adapter (exit ${status}); remove its registration manually:"
      warn "  bash ${script}"
      printf '%s\n' "$output" | sed 's/^/    /' >&2 || true
    else
      info "Deregistered the ${framework} adapter"
    fi
  done
  [ "$failed" -eq 0 ]
}

SCHEMA=""
INSTALL_ID=""
METHOD=""
TL_VERSION=""
INSTALL_DIR=""
NPM_PREFIX=""
NPM_PKG_OWNER=""
ADAPTERS_DIR=""
ADAPTERS_DIR_DIGEST=""
ADAPTERS_OWNER=""
PATH_RC=""
FILES=()
DIGESTS=()
TARGETS=()

if [ ! -f "$RECEIPT" ]; then
  err "No install receipt found at ${RECEIPT}"
  err "This script only removes what scripts/install.sh recorded, so it will not guess."
  err "Uninstall manually instead, matching how you installed Tokenless:"
  err "  anolisa CLI : anolisa uninstall tokenless"
  err "  npm         : npm uninstall -g ${NPM_PACKAGE}"
  err "  source build: rm -f <install-dir>/tokenless"
  exit 1
fi

while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    ''|'#'*) continue ;;
  esac
  key="${line%%=*}"
  value="${line#*=}"
  case "$key" in
    schema)              SCHEMA="$value" ;;
    install_id)          INSTALL_ID="$value" ;;
    method)              METHOD="$value" ;;
    version)             TL_VERSION="$value" ;;
    install_dir)         INSTALL_DIR="$value" ;;
    npm_prefix)          NPM_PREFIX="$value" ;;
    npm_pkg_owner)       NPM_PKG_OWNER="$value" ;;
    adapters_dir)        ADAPTERS_DIR="$value" ;;
    adapters_dir_digest) ADAPTERS_DIR_DIGEST="$value" ;;
    adapters_dir_owner)  ADAPTERS_OWNER="$value" ;;
    path_rc_file)        PATH_RC="$value" ;;
    file)                FILES+=("$value") ;;
    file_digest)         DIGESTS+=("$value") ;;
    file_target)         TARGETS+=("$value") ;;
  esac
done < "$RECEIPT"

info "Tokenless uninstaller"
info "Receipt      : ${RECEIPT}"
info "Method       : ${METHOD:-unknown}"
info "Version      : ${TL_VERSION:-unknown}"
info "Install dir  : ${INSTALL_DIR:-unknown}"
info "Install id   : ${INSTALL_ID:-unknown}"
if [ "$SCHEMA" != "$RECEIPT_SCHEMA_CURRENT" ]; then
  case "$SCHEMA" in
    ''|1)
      warn "Receipt schema is '${SCHEMA:-1}', which records no file identity."
      warn "Recorded paths are removed on path alone; files another installer"
      warn "placed at the same path afterwards cannot be told apart."
      ;;
    2)
      warn "Receipt schema is 2, which records content but no install ownership."
      warn "A later anolisa or npm install of the same version leaves identical"
      warn "bytes behind, so those paths cannot be told apart from this install's."
      ;;
    *)
      warn "Receipt schema is '${SCHEMA}', which this uninstaller does not know;"
      warn "it is read on a best-effort basis."
      ;;
  esac
fi

# Prints the first entry under <prefix>/bin that `npm uninstall -g` would delete
# but that is not a link into the package this receipt owns. Empty means the
# delegation is safe.
#
# This judges npm's *actual* deletion scope rather than the receipt's file list.
# npm removes the module directory together with the bin entries it created for
# it, and with a separate install directory the receipt never records those
# <prefix>/bin links at all — it records the installer's own entry in the install
# directory, pointing straight at the payload inside the package. So a user
# replacing <prefix>/bin/tokenless changed nothing the receipt could see, the
# conflict went undetected, npm deleted the foreign file, and the run reported a
# successful uninstall. Ownership has to be decided over the resources really
# affected, once, before anything is modified.
npm_delegated_conflict() {
  local prefix="$1" pkg_dir="${1}/lib/node_modules/${NPM_PACKAGE}" bin p resolved
  for bin in tokenless rtk; do
    p="${prefix}/bin/${bin}"
    if [ -e "$p" ] || [ -L "$p" ]; then
      resolved=$(resolve_path "$p" 2>/dev/null || true)
      # The trailing slash is the point: without it the pattern also matches a
      # *sibling* directory whose name merely starts with the package name
      # (anolisa-tokenless-backup/...), so a launcher pointing at an identical
      # payload in somebody else's directory was judged ours, npm deleted it, and
      # the run reported a successful uninstall. Containment is a directory
      # boundary, not a string prefix.
      case "$resolved" in
        "${pkg_dir}/"*) ;;
        *) printf '%s\n' "$p"; return 0 ;;
      esac
    fi
  done
  return 0
}

# The npm ownership verdict has to be reached before a single launcher is
# deleted. A direct `npm install -g` of the same version into the same prefix
# recreates the identical symlink and the identical payload, so both the recorded
# digest and the recorded link target still match while the launcher belongs to
# the newer install. Deleting it first and recognising the takeover afterwards
# leaves the global package in place with no command on PATH.
NPM_PKG_TAKEN_OVER=0
NPM_PKG_UNPROVEN=0
NPM_LEFT_BEHIND=0
# Set when npm is present but the uninstall command itself failed. The launchers
# that point into the prefix were deferred rather than deleted, so they are still
# in place and the command keeps working until this is retried.
NPM_REMOVAL_FAILED=0
# Set when step 1 keeps a recorded launcher under the npm prefix because another
# installation took that path over. `npm uninstall -g --prefix` deletes the
# prefix's own bin entries without consulting the ownership check, so step 2 must
# not be allowed to delegate away a path step 1 just decided was not ours.
NPM_BLOCKED_BY_TAKEOVER=0
# The path that made the delegated removal unsafe, named in the warning.
NPM_CONFLICT_PATH=""
# Launchers whose recorded target resolves into the npm prefix. Step 1 collects
# them instead of deleting them, and step 2 removes them only once the package
# removal is confirmed — see the comment there.
DEFERRED_NPM_LAUNCHERS=()
# Why the package in the recorded prefix is being kept, and the reason the
# launchers that resolve into it are kept too. Empty means "this receipt owns the
# package and it is going".
NPM_PKG_KEEP_REASON=""
if [ "$METHOD" = "npm" ] && [ -n "$NPM_PREFIX" ]; then
  if receipt_schema_at_least 3 && [ -z "$NPM_PKG_OWNER" ]; then
    # A schema-3 receipt always records an owner when the installer could prove
    # one. An empty field means the marker could not be written, i.e. ownership
    # was never established — which is not the same as "no evidence, assume ours".
    NPM_PKG_UNPROVEN=1
    NPM_PKG_KEEP_REASON="this receipt does not prove it owns that package"
  elif [ -n "$NPM_PKG_OWNER" ]; then
    pkg_marker="$(owner_marker_read "${NPM_PREFIX}/lib/node_modules/${NPM_PACKAGE}/${OWNER_MARKER_FILE}" 2>/dev/null || true)"
    if [ "$pkg_marker" != "$NPM_PKG_OWNER" ]; then
      NPM_PKG_TAKEN_OVER=1
      NPM_PKG_KEEP_REASON="a newer installation owns that package"
    fi
  fi
  # Whether the package can be removed at all has to be settled here, before a
  # single launcher is deleted. When the install directory is inside the prefix —
  # `npm install -g --prefix ~/.local` with `~/.local/bin` — the recorded
  # launchers *are* the prefix's own bin links, so deleting them first and only
  # then discovering that npm is missing takes away the working command while the
  # package that needs it stays installed.
  if [ -z "$NPM_PKG_KEEP_REASON" ] && [ "$DRY_RUN" != "1" ] \
     && ! command -v npm >/dev/null 2>&1; then
    NPM_LEFT_BEHIND=1
    NPM_PKG_KEEP_REASON="npm is not on PATH, so that package cannot be removed"
  fi
  # And the delegated removal itself has to be safe before a single launcher goes.
  if [ -z "$NPM_PKG_KEEP_REASON" ] && [ "$DRY_RUN" != "1" ]; then
    NPM_CONFLICT_PATH="$(npm_delegated_conflict "$NPM_PREFIX")"
    if [ -n "$NPM_CONFLICT_PATH" ]; then
      NPM_BLOCKED_BY_TAKEOVER=1
      NPM_PKG_KEEP_REASON="${NPM_CONFLICT_PATH} is not a link into that package, so removing it would delete a file this receipt does not own"
    fi
  fi
fi

# 1. Recorded binaries in the install directory. Nothing else in that directory
#    is touched, so a foreign `rtk`/`toon` or an anolisa-managed CLI survives.
#    The recorded digest is checked first: when anolisa or a manual npm install
#    later replaced the file at that path, it is no longer ours to delete.
if [ "${#FILES[@]}" -gt 0 ]; then
  idx=0
  for f in "${FILES[@]}"; do
    digest="${DIGESTS[$idx]:-}"
    target="${TARGETS[$idx]:-}"
    idx=$((idx + 1))
    if [ ! -e "$f" ] && [ ! -L "$f" ]; then
      warn "Already gone, skipping: ${f}"
      continue
    fi
    if [ -d "$f" ] && [ ! -L "$f" ]; then
      warn "Refusing to remove directory not owned by this installer: ${f}"
      continue
    fi
    if [ -n "$digest" ]; then
      current="$(file_digest "$f")"
      if [ "$current" != "$digest" ]; then
        warn "Skipping ${f}: its content no longer matches the receipt,"
        warn "  so another installation has taken over that path."
        continue
      fi
    fi
    # Identical content is not ownership: a newer anolisa or npm install of the
    # same version reproduces these bytes, so the recorded link target has to
    # match as well.
    if receipt_schema_at_least 3 && ! owned_by_receipt "$f" "$target"; then
      warn "Skipping ${f}: it is no longer the artefact this receipt recorded,"
      warn "  so another installation has taken over that path."
      continue
    fi
    # Digest and target both match, but they describe the package this launcher
    # points into — and that package is being kept, either because somebody else
    # owns it now or because it cannot be removed at all. The launcher goes with
    # it: deleting it would leave the package installed with no command on PATH.
    if [ -n "$NPM_PKG_KEEP_REASON" ] && [ -n "$target" ]; then
      case "$target" in
        "${NPM_PREFIX}"/*)
          warn "Skipping ${f}: it is a launcher of the npm installation in ${NPM_PREFIX},"
          warn "  which is being kept because ${NPM_PKG_KEEP_REASON}."
          continue ;;
      esac
    fi
    # This launcher points into the npm package that step 2 is about to remove,
    # and that removal can still fail even with npm on PATH — permissions, a
    # corrupt store or a failing lifecycle script all come back non-zero, and
    # errexit would stop the run right there. Deleting the launcher first leaves
    # the global package and the receipt installed with no command on PATH, so
    # these wait until the removal is confirmed. Both layouts are covered: with
    # `--prefix ~/.local` the recorded launcher *is* the prefix's own bin link,
    # and with a separate install directory it is a symlink resolving into the
    # prefix, which is what the recorded target says either way.
    if [ "$METHOD" = "npm" ] && [ -n "$NPM_PREFIX" ] && [ -n "$target" ]; then
      case "$target" in
        "${NPM_PREFIX}"/*)
          if [ "$DRY_RUN" = "1" ]; then
            info "[dry-run] would remove ${f} once the npm package is removed"
          else
            DEFERRED_NPM_LAUNCHERS+=("$f")
            info "Deferring ${f} until the npm package in ${NPM_PREFIX} is removed"
          fi
          continue ;;
      esac
    fi
    if [ "$DRY_RUN" = "1" ]; then
      info "[dry-run] would remove ${f}"
    else
      rm -f "$f"
      info "Removed ${f}"
    fi
  done
else
  warn "Receipt lists no installed files"
fi

# 2. npm global package — only for the npm method, only from the recorded prefix,
#    and only while the ownership marker the installer wrote is still there. A
#    newer `npm install -g` of the same version replaces the module directory and
#    the marker with it, which is the only way to tell the two apart.
if [ "$METHOD" = "npm" ] && [ -n "$NPM_PREFIX" ]; then
  if [ "$NPM_PKG_UNPROVEN" = "1" ]; then
    warn "Skipping the npm package in ${NPM_PREFIX}: this receipt records no ownership"
    warn "  marker for it, so it cannot be told apart from a package a newer install"
    warn "  put there. Remove it yourself if you no longer need it:"
    warn "  npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
  elif [ "$NPM_PKG_TAKEN_OVER" = "1" ]; then
    warn "Skipping the npm package in ${NPM_PREFIX}: its ownership marker belongs to"
    warn "  a newer installation. Remove it yourself if you no longer need it:"
    warn "  npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
  elif [ "$NPM_LEFT_BEHIND" = "1" ]; then
    # Decided before step 1 ran, so the launchers that resolve into this prefix are
    # still in place: the global module directory, the prefix's own bin links and
    # the receipt all survive together, and Tokenless keeps working until npm is
    # back and this script is re-run.
    warn "npm not found: the global package in ${NPM_PREFIX} is still installed, its"
    warn "  launcher links are still in place, and Tokenless still runs from them."
    warn "  The receipt is kept so this can be finished:"
    warn "  npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
    warn "then re-run this uninstaller."
  elif [ "$NPM_BLOCKED_BY_TAKEOVER" = "1" ]; then
    # Decided before step 1 touched anything, so the ownership verdict constrains
    # the delegated removal as well as this script's own rm: npm deletes
    # <prefix>/bin entries as part of removing the package, and one of them is a
    # file another installation put there.
    warn "Not running npm uninstall in ${NPM_PREFIX}: ${NPM_CONFLICT_PATH} is not a"
    warn "  link into the package this receipt recorded, so npm would delete that"
    warn "  foreign file along with it."
    warn "  The package, that file and the receipt are all kept. Remove the package"
    warn "  yourself once you have decided who owns that path:"
    warn "  npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
  else
    npm_status=0
    run npm uninstall -g "$NPM_PACKAGE" --prefix "$NPM_PREFIX" || npm_status=$?
    if [ "$npm_status" -eq 0 ]; then
      info "Uninstalled npm package ${NPM_PACKAGE} from prefix ${NPM_PREFIX}"
      # The removal is confirmed, so the launchers deferred by step 1 can go. npm
      # already deleted the ones that were the prefix's own bin links; rm -f on a
      # path that is gone is a no-op, and the separate-layout symlinks are ours.
      if [ "${#DEFERRED_NPM_LAUNCHERS[@]}" -gt 0 ]; then
        for deferred in "${DEFERRED_NPM_LAUNCHERS[@]}"; do
          if [ "$DRY_RUN" = "1" ]; then
            info "[dry-run] would remove ${deferred}"
          elif rm -f "$deferred" 2>/dev/null; then
            info "Removed ${deferred}"
          else
            warn "Could not remove ${deferred}; remove it manually."
          fi
        done
      fi
    else
      # npm is on PATH but refused. Keep everything: the package is still
      # installed, so its launchers stay too and Tokenless keeps working, and the
      # receipt stays so this can be finished once the npm failure is fixed.
      NPM_REMOVAL_FAILED=1
      warn "npm could not remove ${NPM_PACKAGE} from ${NPM_PREFIX} (exit ${npm_status})."
      warn "The global package, its launcher links and the receipt are all kept, so"
      warn "Tokenless still runs. Fix the npm failure and re-run this uninstaller, or"
      warn "remove the package yourself:"
      warn "  npm uninstall -g ${NPM_PACKAGE} --prefix ${NPM_PREFIX}"
    fi
  fi
fi

# 3. Adapter resources — recorded only when the npm postinstall placed them.
#    A source build installs no adapters, so this step is skipped for it and an
#    adapter tree owned by the anolisa CLI is left alone. Frameworks that were
#    enabled against this tree are deregistered first, so no plugin directory,
#    hook entry or symlink is left pointing at a deleted path.
if [ -n "$ADAPTERS_DIR" ]; then
  if [ -d "$ADAPTERS_DIR" ]; then
    adapters_owned=1
    if [ -n "$ADAPTERS_DIR_DIGEST" ]; then
      current="$(file_digest "${ADAPTERS_DIR}/${ADAPTERS_IDENTITY_FILE}")"
      if [ "$current" != "$ADAPTERS_DIR_DIGEST" ]; then
        adapters_owned=0
        warn "Skipping ${ADAPTERS_DIR}: it no longer matches the receipt,"
        warn "  so another installation has taken over that adapter tree."
      fi
    fi
    # The manifest is stamped with the release version, so a same-version install
    # by anolisa or by a direct `npm install -g` reproduces it byte for byte. The
    # ownership marker is what such a reinstall removes.
    if [ "$adapters_owned" = "1" ] && receipt_schema_at_least 3 && [ -z "$ADAPTERS_OWNER" ]; then
      adapters_owned=0
      warn "Skipping ${ADAPTERS_DIR}: this receipt records no ownership marker for it,"
      warn "  so it cannot be told apart from a tree a newer installation placed."
      warn "  Remove it yourself once you have confirmed nothing else needs it."
    fi
    if [ "$adapters_owned" = "1" ] && [ -n "$ADAPTERS_OWNER" ]; then
      marker="$(owner_marker_read "${ADAPTERS_DIR}/${OWNER_MARKER_FILE}" 2>/dev/null || true)"
      if [ "$marker" != "$ADAPTERS_OWNER" ]; then
        adapters_owned=0
        warn "Skipping ${ADAPTERS_DIR}: its ownership marker belongs to a newer"
        warn "  installation, so its resources and framework registrations are kept."
      fi
    fi
    if [ "$adapters_owned" = "1" ]; then
      if deregister_framework_adapters "$ADAPTERS_DIR"; then
        run rm -rf "$ADAPTERS_DIR"
        info "Removed adapter resources ${ADAPTERS_DIR}"
      else
        ADAPTERS_KEPT=1
        warn "Keeping ${ADAPTERS_DIR}: a framework registration could not be removed,"
        warn "  and deleting the resources now would leave it pointing at a path that"
        warn "  no longer exists. Fix the framework above and re-run this uninstaller."
      fi
    fi
  else
    warn "Already gone, skipping: ${ADAPTERS_DIR}"
  fi
fi

# 4. PATH entry appended by the installer, only in the recorded rc file and only
#    when it references the recorded install directory.
if [ -n "$PATH_RC" ] && [ -f "$PATH_RC" ] && [ -n "$INSTALL_DIR" ]; then
  if grep -Fq "$MARKER" "$PATH_RC"; then
    if [ "$DRY_RUN" = "1" ]; then
      info "[dry-run] would strip the tokenless PATH entry from ${PATH_RC}"
    else
      tmp_rc="${PATH_RC}.tokenless-uninstall.$$"
      awk -v marker="$MARKER" -v dir="$INSTALL_DIR" '
        BEGIN { skip = 0 }
        {
          if (skip == 1) {
            skip = 0
            if ($0 ~ /^export PATH=/ && index($0, dir) > 0) next
            print marker
          }
          if ($0 == marker) { skip = 1; next }
          print
        }
      ' "$PATH_RC" > "$tmp_rc" && cat "$tmp_rc" > "$PATH_RC" && rm -f "$tmp_rc"
      info "Removed the tokenless PATH entry from ${PATH_RC}"
    fi
  fi
fi

# 5. Runtime data (stats.db / stash.db) is user data, not an installed file.
if [ "$PURGE" = "1" ]; then
  if [ -d "$RUNTIME_DATA_DIR" ]; then
    run rm -rf "$RUNTIME_DATA_DIR"
    info "Purged runtime data ${RUNTIME_DATA_DIR}"
  fi
else
  if [ -d "$RUNTIME_DATA_DIR" ]; then
    info "Kept runtime data ${RUNTIME_DATA_DIR} (re-run with --purge to delete stats/stash)"
  fi
fi

# 6. The receipt itself — unless the run stopped short. Keeping the adapter
#    resources for a retry is only useful if the retry can still find them, and
#    the receipt is the only record that does.
if [ "$DRY_RUN" = "1" ]; then
  info "[dry-run] would remove receipt ${RECEIPT}"
  info "[dry-run] nothing was changed"
  exit 0
fi
if [ "${ADAPTERS_KEPT:-0}" = "1" ] || [ "${NPM_LEFT_BEHIND:-0}" = "1" ] \
   || [ "${NPM_REMOVAL_FAILED:-0}" = "1" ] \
   || [ "${NPM_BLOCKED_BY_TAKEOVER:-0}" = "1" ]; then
  warn "Kept receipt ${RECEIPT} so the uninstall can be retried."
  if [ "${ADAPTERS_KEPT:-0}" = "1" ]; then
    err "Tokenless uninstall incomplete: a framework registration could not be removed."
    err "The adapter resources and the receipt are still in place; remove the"
    err "registration and re-run this script to finish."
  elif [ "${NPM_BLOCKED_BY_TAKEOVER:-0}" = "1" ]; then
    err "Tokenless uninstall incomplete: a launcher under the recorded npm prefix"
    err "belongs to another installation, so the package was not removed either —"
    err "delegating that to npm would have deleted the foreign file. Nothing under"
    err "that prefix was touched and the receipt is kept; resolve the ownership of"
    err "that path, then re-run this script or remove the package manually."
  elif [ "${NPM_REMOVAL_FAILED:-0}" = "1" ]; then
    err "Tokenless uninstall incomplete: npm was found but could not remove the global"
    err "package. The package, its launcher links and the receipt are all still in"
    err "place, so the command keeps working; fix the npm failure and re-run this"
    err "script to finish."
  else
    err "Tokenless uninstall incomplete: the global npm package could not be removed"
    err "because npm is not on PATH. The package, its launcher links under the"
    err "recorded prefix and the receipt are all still in place; install npm (or run"
    err "the command above) and re-run this script to finish."
  fi
  exit 1
fi
rm -f "$RECEIPT"
info "Removed receipt ${RECEIPT}"
receipt_parent=$(dirname "$RECEIPT")
if [ -d "$receipt_parent" ] && [ -z "$(ls -A "$receipt_parent" 2>/dev/null)" ]; then
  rmdir "$receipt_parent"
fi

info "Tokenless uninstall complete"
