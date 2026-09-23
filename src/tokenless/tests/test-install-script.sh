#!/usr/bin/env bash
# Regression tests for the standalone curl installer and its uninstaller:
#   scripts/install.sh / scripts/uninstall.sh
#
# Covered behaviours (all offline — curl, npm and cargo are stubbed):
#   1. The source-build fallback locates src/tokenless/Cargo.toml inside a real
#      GitHub archive layout, where the manifest sits four levels below the
#      temporary directory.
#   2. A version pin whose tag does not exist fails hard and never requests the
#      `main` branch archive.
#   3. The temporary source tree is removed and the EXIT trap does not report
#      "tmpdir: unbound variable" after try_source_build() has returned.
#   4. install.sh writes a receipt; uninstall.sh removes only the recorded
#      paths, so the npm / source / custom-install-dir scenarios each keep
#      files they do not own.
#   5. A failed write (install(1), ln, mkdir) fails the run instead of being
#      swallowed by the `||`/`elif` condition that disables errexit, and never
#      records a receipt or a pre-existing foreign binary.
#   6. Re-running with another method retires the previous receipt's artefacts
#      (rtk link, npm global package, adapter tree), and uninstall.sh refuses a
#      recorded path whose content another installer has since replaced.
#   7. uninstall.sh deregisters an enabled framework adapter (real qwencode
#      scripts, link-type registration) before deleting the adapter resources.
#   8. A npm attempt that fails after `npm install -g` is rolled back, so the
#      source-build fallback inherits no unowned package, link or adapter tree.
#   9. An npm prefix whose bin directory *is* the install directory: the
#      previous package is retired before the new CLI is written, so
#      `npm uninstall` cannot take it away again.
#  10. macOS has no source-build route — the installer exits without running
#      cargo, on Intel macOS where no npm package is published as well.
#  11. A shared adapter directory that belongs to another installation is
#      neither adopted into the receipt nor destroyed, round trip included.
#  12. Adapter deregistration removes framework registrations only, never the
#      component binary the caller decided to keep.
#  13. A replacement that fails halfway (missing tag, failing build) leaves the
#      previous install working and its receipt accurate.
#  14. A readlink(1) without -f (BSD, macOS before 12.3) still records and still
#      rolls back the launcher links.
#  15. A newer installation of the *same version* leaves byte-identical content
#      behind; ownership, not the hash, decides what the uninstaller removes.
#  16. A npm upgrade whose new binary is broken, with a source fallback that also
#      fails, puts the previous payload back so the old CLI still runs.
#  17. A receipt that cannot be updated fails the run instead of leaving a stale
#      record that a later uninstall would apply to the new install.
#  18. A restore that fails keeps the only remaining copy instead of deleting it,
#      and a foreign adapter tree that cannot be put back fails the whole run
#      rather than reporting an install that broke somebody else's.
#  19. A successful run discards its rollback snapshot: no litter in TMPDIR and no
#      "only remaining copy" claim for a backup nothing needs any more.
#  20. A direct `npm install -g` of the same version into the same prefix
#      recreates an identical launcher; the uninstaller must not delete it.
#  21. A framework that cannot be deregistered keeps the adapter resources, the
#      registration and the receipt, so the uninstall can be retried.
#  22. Staging the previous install is fail-closed: no staging directory or a
#      single file that cannot be copied aside stops the run before it writes.
#  23. A framework CLI that refuses to deregister fails the real adapter script,
#      which keeps the adapter resources and the receipt for a retry.
#  24. A snapshot that cannot be taken stops the npm route before `npm install -g`
#      replaces anything, for an owned adapter tree and for a foreign launcher.
#  25. A marker that cannot be written makes the receipt record "not owned", and
#      the uninstaller then refuses to delete the package, the tree or the
#      launchers that point into it.
#  26. A rollback that could not copy everything back stops the run instead of
#      letting a source build report success over a half-restored install.
#  27. npm present but refusing to uninstall keeps the launchers, the package and
#      the receipt, in the overlapping and the separate layout alike.
#  28. npm nests the @anolisa platform dependency inside the root package, so
#      removing the root package is enough — and a *sibling* @anolisa scope is a
#      standalone global install that must survive retirement, rollback and a
#      receipt-driven uninstall, on x86_64 and on aarch64 alike.
#  29. A framework CLI that is merely missing is not proof its registration is
#      gone: codex, hermes and qoder fail closed while the registration persists.
#  30. A launcher under the npm prefix that another installation took over also
#      blocks the *delegated* `npm uninstall`, which would delete it regardless.
#  31. "Cannot confirm" is a third state, not a success: a legal flow-sequence
#      hermes config, and a claude settings.json with neither CLI nor jq.
#  32. A same-version npm takeover is visible only in the owner marker, so
#      staging and retirement must share that one verdict — a kept package used
#      to lose its `rtk` entry point. A receipt that never proved ownership is a
#      third state, and is kept rather than deleted.
#  33. Once `npm install` has been invoked, no exit status proves its postinstall
#      left no side effects, so the run reports itself incomplete instead of
#      switching method; the source build stays available explicitly.
#  34. The delegated removal is judged over the paths npm actually deletes, so a
#      foreign <prefix>/bin entry blocks it in the separate layout too — and
#      containment there is a directory boundary, not a string prefix.
#  35. A failed attempt's rollback deregisters nothing: the bundled adapter scripts
#      are full uninstallers, so running them all removed a Qwen extension that
#      predated the run.
#  36. Hermes registration state is read with a real YAML parser, so a normal
#      plugins.disabled entry (what a successful disable writes) is a success and
#      not a permanent failure, while a still-enabled plugin fails in any legal
#      shape — including the ones a hand-written subset parser misread as absent.
#      With no parser importable the answer is "unknown" and fails closed; both
#      paths are exercised, the no-parser one forced with `python3 -S`.
#  37. A postinstall's external framework registration is not orphaned by a failed
#      attempt: the resources it points at are kept, a pre-existing Qwen
#      registration and unrelated Claude config survive untouched, and no source
#      build is started.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TOKENLESS_ROOT="$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)"
INSTALL_SH="$TOKENLESS_ROOT/scripts/install.sh"
UNINSTALL_SH="$TOKENLESS_ROOT/scripts/uninstall.sh"

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

STUB_DIR="$TEST_DIR/stubs"
DIST_DIR="$TEST_DIR/dist"
mkdir -p "$STUB_DIR" "$DIST_DIR"

FAKE_VERSION="0.7.9"
ARCHIVE_ROOT="ANOLISA-tokenless-v${FAKE_VERSION}"

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }

assert_file() { [ -e "$2" ] || fail "$1: expected file $2"; pass "$1"; }
assert_no_file() { [ ! -e "$2" ] && [ ! -L "$2" ] || fail "$1: unexpected file $2"; pass "$1"; }
assert_contains() {
  case "$2" in
    *"$3"*) pass "$1" ;;
    *) fail "$1: expected substring '$3' in: $2" ;;
  esac
}
assert_not_contains() {
  case "$2" in
    *"$3"*) fail "$1: unexpected substring '$3' in: $2" ;;
    *) pass "$1" ;;
  esac
}
assert_eq() { [ "$2" = "$3" ] || fail "$1: expected '$3', got '$2'"; pass "$1"; }

# --- a GitHub-shaped source archive -----------------------------------------
# <archive-root>/src/tokenless/Cargo.toml is depth 4 from the extraction dir;
# a nested crate manifest must not be picked up instead.
build_fake_archive() {
  local stage="$TEST_DIR/stage/$ARCHIVE_ROOT"
  rm -rf "$TEST_DIR/stage"
  mkdir -p "$stage/src/tokenless/crates/tokenless-cli"
  printf '[workspace]\nmembers = ["crates/tokenless-cli"]\n' > "$stage/src/tokenless/Cargo.toml"
  printf '[package]\nname = "tokenless-cli"\n' > "$stage/src/tokenless/crates/tokenless-cli/Cargo.toml"
  printf '# ANOLISA\n' > "$stage/README.md"
  tar -czf "$DIST_DIR/tag.tar.gz" -C "$TEST_DIR/stage" "$ARCHIVE_ROOT"
  printf '%s\n' "$DIST_DIR/tag.tar.gz"
}
FAKE_TARBALL="$(build_fake_archive)"

# --- stubs -------------------------------------------------------------------
cat > "$STUB_DIR/curl" <<'STUB'
#!/usr/bin/env bash
url=""; out=""; args=("$@"); i=0
while [ "$i" -lt "${#args[@]}" ]; do
  a="${args[$i]}"
  case "$a" in
    -o) out="${args[$((i+1))]}"; i=$((i+2)); continue ;;
    -*) i=$((i+1)); continue ;;
    *)  url="$a"; i=$((i+1)); continue ;;
  esac
done
printf '%s\n' "$url" >> "$CURL_LOG"
case "$url" in
  *"/archive/refs/tags/"*)
    [ "${CURL_TAG_STATUS:-0}" = "0" ] || exit "${CURL_TAG_STATUS}"
    [ -n "$out" ] && cp "$CURL_TAG_TARBALL" "$out"
    exit 0 ;;
  *"/archive/refs/heads/main"*)
    [ -n "$out" ] && cp "${CURL_MAIN_TARBALL:-/dev/null}" "$out"
    exit 0 ;;
  *"registry.npmjs.org"*)
    printf '{"name":"anolisa-tokenless","version":"%s"}\n' "${CURL_NPM_LATEST:-0.7.9}"
    exit 0 ;;
esac
exit 22
STUB

cat > "$STUB_DIR/npm" <<'STUB'
#!/usr/bin/env bash
# NPM_STUB_BROKEN_BIN=1 ships a `tokenless` that fails when run, so the
# installer's verify_cli gate trips *after* the links were written — the case a
# rollback has to undo.
write_bin() {
  if [ "${NPM_STUB_BROKEN_BIN:-0}" = "1" ] && [ "$2" = "tokenless" ]; then
    printf '#!/usr/bin/env bash\necho "%s %s-npm" >&2\nexit 1\n' "$2" "${FAKE_VERSION:-0.7.9}" > "$1"
  else
    printf '#!/usr/bin/env bash\necho "%s %s-npm"\n' "$2" "${FAKE_VERSION:-0.7.9}" > "$1"
  fi
  chmod +x "$1"
}
pkg_prefix() {
  local prev="" a
  for a in "$@"; do
    if [ "$prev" = "--prefix" ]; then printf '%s\n' "$a"; return 0; fi
    prev="$a"
  done
  printf '%s\n' "${NPM_STUB_PREFIX:-$HOME/.npm-global}"
}
case "$1" in
  config)
    printf '%s\n' "${NPM_STUB_PREFIX:-$HOME/.npm-global}"; exit 0 ;;
  install)
    if [ "${NPM_STUB_FAIL:-0}" = "1" ]; then
      echo "npm ERR! code EACCES" >&2; exit 243
    fi
    prefix="$(pkg_prefix "$@")"
    mkdir -p "$prefix/bin" "$prefix/lib/node_modules/anolisa-tokenless"
    # NPM_STUB_PLATFORM_PKG=1 reproduces the real global layout, verified against
    # npm 10.9.4 with packed tarballs: the platform package is an optional
    # dependency and npm NESTS it under the root package's own node_modules. npm
    # never creates a sibling <prefix>/lib/node_modules/@anolisa directory for it,
    # so a sibling scope in a test fixture means a standalone global install the
    # user made themselves. This stub used to model the sibling layout instead,
    # which is how "clean up the platform package" came to look necessary.
    plat="${NPM_STUB_PLATFORM_KEY:-linux-x64}"
    platdir="$prefix/lib/node_modules/anolisa-tokenless/node_modules/@anolisa/tokenless-$plat"
    if [ "${NPM_STUB_PLATFORM_PKG:-0}" = "1" ]; then
      mkdir -p "$platdir/bin"
      for b in tokenless rtk; do write_bin "$platdir/bin/$b" "$b"; done
    fi
    for b in tokenless rtk; do
      # Faithful to real npm by default: a global install keeps the payload in the
      # module directory and links it into <prefix>/bin. Modelling those bin
      # entries as regular files instead produced a layout npm never creates, and
      # hid exactly the distinction the uninstaller has to make — a <prefix>/bin
      # entry that is *not* a link into the package is somebody else's file, and
      # `npm uninstall -g` deletes it anyway. NPM_STUB_SYMLINK_BINS=0 opts back
      # into the unfaithful shape for scenarios that specifically want it.
      if [ "${NPM_STUB_SYMLINK_BINS:-1}" = "1" ]; then
        # Real npm keeps the payload in the module directory and links it into
        # <prefix>/bin. Needed to reproduce a prefix whose bin directory is the
        # installer's install directory.
        payload="$prefix/lib/node_modules/anolisa-tokenless/bin/$b"
        mkdir -p "$(dirname "$payload")"
        if [ "${NPM_STUB_PLATFORM_PKG:-0}" = "1" ]; then
          ln -sfn "$platdir/bin/$b" "$payload"
        else
          write_bin "$payload" "$b"
        fi
        ln -sfn "$payload" "$prefix/bin/$b"
      else
        write_bin "$prefix/bin/$b" "$b"
      fi
    done
    # Mimic npm/scripts/postinstall.js, including the part that is easy to stub
    # wrong: the real postinstall only replaces the shared adapter directory when
    # it can prove it owns what is there. A stub that always deletes it would
    # exercise a path production never takes and hide the installer's own
    # rebuild-over-an-untouched-tree behaviour.
    adapters="$HOME/.local/share/anolisa/adapters/tokenless"
    contract="$HOME/.local/share/anolisa/components/tokenless/component.toml"
    adapters_foreign=0
    if [ -d "$adapters" ]; then
      if [ -f "$contract" ]; then
        adapters_foreign=1
      else
        case "$(head -n1 "$adapters/.tokenless-owner" 2>/dev/null || true)" in
          npm:*|curl-installer:*) ;;
          *) adapters_foreign=1 ;;
        esac
      fi
    fi
    if [ "$adapters_foreign" = "1" ] && [ "${NPM_STUB_FORCE_ADAPTERS:-0}" != "1" ]; then
      echo "anolisa-tokenless: keeping $adapters (belongs to another installation)"
    else
      rm -rf "$adapters"
      mkdir -p "$adapters/claude-code/scripts"
      printf '#!/usr/bin/env bash\n' > "$adapters/claude-code/scripts/install.sh"
      # package-npm.js stamps adapters/tokenless/manifest.json with the release
      # version; install.sh uses its digest as the adapter tree's identity.
      printf '{"component":"tokenless","version":"%s"}\n' "${FAKE_VERSION:-0.7.9}" > "$adapters/manifest.json"
      printf 'npm:anolisa-tokenless@%s\n' "${FAKE_VERSION:-0.7.9}" > "$adapters/.tokenless-owner"
      # Optionally ship a real adapter payload so the enable/disable chain can be
      # exercised against the repository's own scripts.
      if [ -n "${NPM_STUB_ADAPTER_SRC:-}" ] && [ -d "${NPM_STUB_ADAPTER_SRC}/qwencode" ]; then
        cp -R "${NPM_STUB_ADAPTER_SRC}/qwencode" "$adapters/qwencode"
      fi
      # NPM_STUB_ENABLE_CLAUDE=1 models what the real package postinstall does on
      # main: after placing the adapter resources it runs the claude-code adapter's
      # own install.sh, which registers the plugin with the Claude CLI. That
      # registration lands in the framework's own config -- outside the npm prefix
      # and outside this tree -- so it is precisely the side effect a file-level
      # rollback cannot see, and the one a failed attempt must not orphan.
      if [ "${NPM_STUB_ENABLE_CLAUDE:-0}" = "1" ] \
         && [ -d "${NPM_STUB_ADAPTER_SRC:-}/claude-code" ]; then
        rm -rf "$adapters/claude-code"
        cp -R "${NPM_STUB_ADAPTER_SRC}/claude-code" "$adapters/claude-code"
        CLAUDE_BIN="${CLAUDE_BIN:-claude}" bash "$adapters/claude-code/scripts/install.sh" >/dev/null 2>&1 || true
      fi
    fi
    # NPM_STUB_POSTINSTALL_REGISTRATION models what the real package postinstall
    # does on current main: it auto-enables a framework, which writes a
    # registration *outside* the prefix and the adapter tree. With
    # NPM_STUB_FAIL_AFTER_POSTINSTALL=1 the install then fails, so npm returns
    # non-zero while that registration is already on disk — which is exactly why
    # no exit status can prove the absence of side effects.
    if [ -n "${NPM_STUB_POSTINSTALL_REGISTRATION:-}" ]; then
      mkdir -p "$(dirname "${NPM_STUB_POSTINSTALL_REGISTRATION}")" 2>/dev/null || true
      printf 'tokenless registered by postinstall\n' > "${NPM_STUB_POSTINSTALL_REGISTRATION}"
    fi
    if [ "${NPM_STUB_FAIL_AFTER_POSTINSTALL:-0}" = "1" ]; then
      echo "npm ERR! code 42 postinstall failed" >&2; exit 42
    fi
    echo "added 1 package"; exit 0 ;;
  uninstall)
    # NPM_STUB_UNINSTALL_FAIL=1 models npm being on PATH but refusing: a permission
    # error, a corrupt store, a failing lifecycle script. The uninstaller must not
    # have deleted the launchers by the time this comes back non-zero.
    if [ "${NPM_STUB_UNINSTALL_FAIL:-0}" = "1" ]; then
      echo "npm ERR! code EACCES: permission denied, unlink" >&2; exit 243
    fi
    prefix="$(pkg_prefix "$@")"
    rm -rf "$prefix/lib/node_modules/anolisa-tokenless"
    rm -f "$prefix/bin/tokenless" "$prefix/bin/rtk"
    echo "removed 1 package"; exit 0 ;;
esac
exit 0
STUB

cat > "$STUB_DIR/cargo" <<'STUB'
#!/usr/bin/env bash
# `cargo build --release --locked -p tokenless-cli`, run from src/tokenless.
# Every invocation is logged: the macOS scenarios assert cargo is never reached.
printf '%s\n' "$*" >> "${CARGO_LOG:-/dev/null}"
if [ "${CARGO_STUB_FAIL:-0}" = "1" ]; then
  echo "error: could not compile \`tokenless-cli\`" >&2; exit 101
fi
mkdir -p target/release
printf '#!/usr/bin/env bash\necho "tokenless %s-src"\n' "${FAKE_VERSION:-0.7.9}" > target/release/tokenless
chmod +x target/release/tokenless
exit 0
STUB

# install(1) and ln pass through to the real binaries unless a scenario asks
# for a write failure. Both are called from functions that main() uses as
# `||`/`elif` conditions, where Bash disables errexit, so their status has to be
# checked explicitly by the installer — these stubs prove that it is.
cat > "$STUB_DIR/install" <<'STUB'
#!/usr/bin/env bash
if [ -n "${INSTALL_STUB_STATUS:-}" ]; then
  echo "install: cannot create regular file: Permission denied" >&2
  exit "${INSTALL_STUB_STATUS}"
fi
real="${REAL_INSTALL_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/install /bin/install; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "install stub: no real install(1) found" >&2; exit 127; }
exec "$real" "$@"
STUB

cat > "$STUB_DIR/ln" <<'STUB'
#!/usr/bin/env bash
if [ "${LN_STUB_FAIL:-0}" = "1" ]; then
  echo "ln: failed to create symbolic link: Permission denied" >&2
  exit 1
fi
real="${REAL_LN_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/ln /bin/ln; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "ln stub: no real ln found" >&2; exit 127; }
exec "$real" "$@"
STUB

# Minimal `qwen` CLI so the real qwencode adapter scripts can link and unlink
# an extension that points into the adapter tree.
cat > "$STUB_DIR/qwen" <<'STUB'
#!/usr/bin/env bash
ext_dir="$HOME/.qwen/extensions"
case "$1 $2" in
  "extensions link")
    mkdir -p "$ext_dir"
    ln -sfn "$3" "$ext_dir/tokenless"
    echo "linked $3" ;;
  "extensions list")
    [ -e "$ext_dir/tokenless" ] && echo "tokenless" ;;
  "extensions uninstall")
    rm -rf "$ext_dir/tokenless"
    echo "uninstalled tokenless" ;;
esac
exit 0
STUB

# Platform boundaries have to be testable on a Linux CI host, so `uname` is
# stubbed too. Without UNAME_STUB_OS / UNAME_STUB_ARCH it passes through to the
# real binary and every other scenario still runs against the host platform.
cat > "$STUB_DIR/uname" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  -s) if [ -n "${UNAME_STUB_OS:-}" ]; then printf '%s\n' "$UNAME_STUB_OS"; exit 0; fi ;;
  -m) if [ -n "${UNAME_STUB_ARCH:-}" ]; then printf '%s\n' "$UNAME_STUB_ARCH"; exit 0; fi ;;
esac
real="${REAL_UNAME_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/uname /bin/uname; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "uname stub: no real uname found" >&2; exit 127; }
exec "$real" "$@"
STUB

# The BSD readlink(1) shipped with macOS before 12.3 has no -f. READLINK_STUB_NO_F=1
# reproduces it, so the installer's portable resolution path is exercised on a
# Linux host instead of only failing on a machine nobody tests on.
cat > "$STUB_DIR/readlink" <<'STUB'
#!/usr/bin/env bash
if [ "${READLINK_STUB_NO_F:-0}" = "1" ]; then
  case "${1:-}" in
    -*)
      echo "readlink: illegal option -- ${1#-}" >&2
      echo "usage: readlink [-n] [file ...]" >&2
      exit 1 ;;
  esac
fi
real="${REAL_READLINK_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/readlink /bin/readlink; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "readlink stub: no real readlink found" >&2; exit 127; }
exec "$real" "$@"
STUB

# `cp` is what every snapshot and every restore goes through. CP_STUB_FAIL_DEST
# makes exactly one copy fail, which is the only way to reach the "could not put
# it back" branches: a full disk or a permission change at restore time is not
# reproducible from a test otherwise.
cat > "$STUB_DIR/cp" <<'STUB'
#!/usr/bin/env bash
if [ -n "${CP_STUB_FAIL_DEST:-}" ] || [ -n "${CP_STUB_FAIL_DEST_BASE:-}" ]; then
  last="${!#}"
  case "$last" in
    "${CP_STUB_FAIL_DEST:-/no/such/prefix}"*)
      echo "cp: cannot create '$last': No space left on device" >&2
      exit 1 ;;
  esac
  # Snapshot entries live under a mktemp directory whose suffix nobody can
  # predict, so a scenario that has to fail exactly one of them matches on the
  # entry's own name instead of the full path.
  if [ -n "${CP_STUB_FAIL_DEST_BASE:-}" ] \
     && [ "$(basename "$last")" = "${CP_STUB_FAIL_DEST_BASE}" ]; then
    echo "cp: cannot create '$last': No space left on device" >&2
    exit 1
  fi
fi
# CP_STUB_FAIL_SRC fails on the source instead, which is how the staging copy of
# a previous install is made to fail without also breaking every snapshot.
if [ -n "${CP_STUB_FAIL_SRC:-}" ]; then
  for a in "$@"; do
    case "$a" in
      -*) continue ;;
      *)
        case "$a" in
          "${CP_STUB_FAIL_SRC}"*)
            echo "cp: cannot open '$a' for reading: Permission denied" >&2
            exit 1 ;;
        esac
        break ;;
    esac
  done
fi
real="${REAL_CP_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/cp /bin/cp; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "cp stub: no real cp found" >&2; exit 127; }
exec "$real" "$@"
STUB

# MKTEMP_STUB_FAIL_PATTERN makes mktemp fail only for templates matching it, so a
# single staging (or rollback) directory can be made uncreatable while the rest of
# the script still gets its temporaries.
cat > "$STUB_DIR/mktemp" <<'STUB'
#!/usr/bin/env bash
if [ -n "${MKTEMP_STUB_FAIL_PATTERN:-}" ]; then
  for a in "$@"; do
    case "$a" in
      *"${MKTEMP_STUB_FAIL_PATTERN}"*)
        echo "mktemp: failed to create directory via mktemp: No space left on device" >&2
        exit 1 ;;
    esac
  done
fi
real="${REAL_MKTEMP_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/mktemp /bin/mktemp; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "mktemp stub: no real mktemp found" >&2; exit 127; }
exec "$real" "$@"
STUB

# Minimal codex CLI so the repository's real Codex adapter script can be driven
# through a deregistration that the framework refuses. CODEX_STUB_REMOVE_FAILS=1
# makes `plugin remove` exit non-zero *and leave the plugin listed*, which is what
# a refusing CLI looks like from outside; without it the removal takes effect.
cat > "$STUB_DIR/codex" <<'STUB'
#!/usr/bin/env bash
state="${CODEX_STUB_STATE:-$HOME/.codex-stub}"
mkdir -p "$state"
case "$1 $2" in
  "plugin list")
    # CODEX_STUB_LIST_FAILS=1 models a CLI that cannot answer the query at all,
    # which is not the same as answering "nothing is registered".
    if [ "${CODEX_STUB_LIST_FAILS:-0}" = "1" ]; then
      echo "codex: error: cannot read the plugin list" >&2
      exit 1
    fi
    # codex-cli 0.154.0 lists every plugin the registered marketplaces offer,
    # in whatever state it is in, under a header that names the marketplace:
    #
    #   Marketplace `anolisa-tokenless`
    #   PLUGIN                       STATUS              VERSION  SOURCE
    #   tokenless@anolisa-tokenless  installed, enabled  local    <source>
    #
    # `plugin remove` only flips that STATUS to `not installed`; the row itself
    # survives until the marketplace that ships the plugin is removed. Reading
    # the listing correctly under both facts is what the adapter's deregistration
    # check is judged on, so the stub reproduces them rather than printing a bare
    # row that disappears with the plugin.
    if [ -f "$state/marketplace" ]; then
      echo "Marketplace \`anolisa-tokenless\`"
      echo "PLUGIN                       STATUS              VERSION  SOURCE"
      if [ -f "$state/plugin" ]; then
        echo "tokenless@anolisa-tokenless  installed, enabled  local    $state/plugin"
      else
        echo "tokenless@anolisa-tokenless  not installed                $state/plugin"
      fi
    fi
    exit 0 ;;
  "plugin remove")
    if [ "${CODEX_STUB_REMOVE_FAILS:-0}" = "1" ]; then
      echo "codex: error: refusing to remove tokenless@anolisa-tokenless" >&2
      exit 1
    fi
    rm -f "$state/plugin"
    echo "removed tokenless@anolisa-tokenless"
    exit 0 ;;
  "plugin marketplace")
    case "$3" in
      list)
        [ -f "$state/marketplace" ] && echo "anolisa-tokenless    $state/marketplace"
        exit 0 ;;
      remove)
        if [ "${CODEX_STUB_REMOVE_FAILS:-0}" = "1" ]; then
          echo "codex: error: refusing to remove the marketplace" >&2
          exit 1
        fi
        rm -f "$state/marketplace"
        exit 0 ;;
    esac
    exit 0 ;;
esac
exit 0
STUB

# TAR_STUB_FAIL=1 reproduces a truncated or corrupt archive: the extraction has
# to be checked, because building on a partially extracted tree yields a CLI that
# is missing sources rather than an error.
cat > "$STUB_DIR/tar" <<'STUB'
#!/usr/bin/env bash
if [ "${TAR_STUB_FAIL:-0}" = "1" ]; then
  echo "tar: Unexpected EOF in archive" >&2
  exit 2
fi
real="${REAL_TAR_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/tar /bin/tar; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "tar stub: no real tar found" >&2; exit 127; }
exec "$real" "$@"
STUB

# Minimal hermes CLI. HERMES_STUB_REFUSE=1 makes `plugins disable` fail *and leave
# the plugins.enabled entry in config.yaml*, which is what a framework refusing to
# deregister looks like from outside.
cat > "$STUB_DIR/hermes" <<'STUB'
#!/usr/bin/env bash
cfg="${HERMES_HOME:-$HOME/.hermes}/config.yaml"
case "$1 $2" in
  "plugins disable")
    if [ "${HERMES_STUB_REFUSE:-0}" = "1" ]; then
      echo "hermes: error: refusing to disable tokenless" >&2
      exit 1
    fi
    if [ -f "$cfg" ]; then
      grep -v '^[[:space:]]*-[[:space:]]*tokenless[[:space:]]*$' "$cfg" > "$cfg.tmp" && mv "$cfg.tmp" "$cfg"
    fi
    exit 0 ;;
  "plugins remove")
    [ "${HERMES_STUB_REFUSE:-0}" = "1" ] && exit 1
    exit 0 ;;
esac
exit 0
STUB

# Minimal claude CLI. CLAUDE_STUB_REFUSE=1 leaves the plugin and the marketplace
# in settings.json.
cat > "$STUB_DIR/claude" <<'STUB'
#!/usr/bin/env bash
case "$1 $2" in
  "plugin uninstall"|"plugin marketplace")
    [ "${CLAUDE_STUB_REFUSE:-0}" = "1" ] && exit 1
    exit 0 ;;
esac
exit 0
STUB

REAL_INSTALL_BIN="$(command -v install || true)"
REAL_CP_BIN="$(command -v cp || true)"
REAL_MKTEMP_BIN="$(command -v mktemp || true)"
REAL_TAR_BIN="$(command -v tar || true)"
REAL_LN_BIN="$(command -v ln || true)"
REAL_UNAME_BIN="$(command -v uname || true)"
REAL_READLINK_BIN="$(command -v readlink || true)"
[ -n "$REAL_INSTALL_BIN" ] && [ -n "$REAL_LN_BIN" ] && [ -n "$REAL_UNAME_BIN" ] \
  && [ -n "$REAL_READLINK_BIN" ] && [ -n "$REAL_CP_BIN" ] && [ -n "$REAL_MKTEMP_BIN" ] \
  && [ -n "$REAL_TAR_BIN" ] \
  || { echo "FAIL the host provides no install(1), ln, uname, readlink, cp, mktemp or tar" >&2; exit 1; }
chmod +x "$STUB_DIR/curl" "$STUB_DIR/npm" "$STUB_DIR/cargo" \
         "$STUB_DIR/install" "$STUB_DIR/ln" "$STUB_DIR/qwen" "$STUB_DIR/uname" \
         "$STUB_DIR/readlink" "$STUB_DIR/cp" "$STUB_DIR/mktemp" "$STUB_DIR/codex" \
         "$STUB_DIR/tar" "$STUB_DIR/hermes" "$STUB_DIR/claude"

# --- harness -----------------------------------------------------------------
# run_script <script> <scenario> [ENV=VAL ...] [-- <script-arg> ...]
# Runs a script in an isolated HOME with only the stubs on PATH; combined output
# lands in RUN_OUTPUT and the exit status in RUN_STATUS.
RUN_STATUS=0
RUN_OUTPUT=""
# Set RUN_WITHOUT to a space-separated list of tool names before a run_script call
# to make them genuinely absent from that scenario's PATH — not just stubbed, but
# unfindable by `command -v`. Both the stub directory and the system directories
# are re-linked per scenario, because a real npm in /usr/bin would otherwise
# answer for the missing stub.
RUN_WITHOUT=""
run_script() {
  local script="$1"; shift
  local scenario="$1"; shift
  local home="$TEST_DIR/$scenario/home"
  local tmp="$TEST_DIR/$scenario/tmp"
  local path_dirs="$STUB_DIR:/usr/local/bin:/usr/bin:/bin"
  if [ -n "$RUN_WITHOUT" ]; then
    local stubs="$TEST_DIR/$scenario/stubs" sysbin="$TEST_DIR/$scenario/sysbin"
    local t base d hidden name
    rm -rf "$stubs" "$sysbin"
    mkdir -p "$stubs" "$sysbin"
    for t in "$STUB_DIR"/*; do
      base=$(basename "$t")
      hidden=0
      for name in $RUN_WITHOUT; do [ "$base" = "$name" ] && hidden=1; done
      [ "$hidden" = "1" ] || ln -sfn "$t" "$stubs/$base"
    done
    for d in /usr/local/bin /usr/bin /bin; do
      [ -d "$d" ] || continue
      for t in "$d"/*; do
        [ -e "$t" ] || continue
        base=$(basename "$t")
        hidden=0
        for name in $RUN_WITHOUT; do [ "$base" = "$name" ] && hidden=1; done
        [ "$hidden" = "1" ] && continue
        [ -e "$sysbin/$base" ] || ln -sfn "$t" "$sysbin/$base" 2>/dev/null || true
      done
    done
    path_dirs="$stubs:$sysbin"
  fi
  local envs=() script_args=() seen_sep=0 a
  for a in "$@"; do
    if [ "$seen_sep" = "1" ]; then script_args+=("$a"); continue; fi
    if [ "$a" = "--" ]; then seen_sep=1; continue; fi
    envs+=("$a")
  done
  mkdir -p "$home" "$tmp"
  RUN_OUTPUT="$(
    env -i \
      PATH="$path_dirs" \
      HOME="$home" \
      SHELL=/bin/bash \
      TMPDIR="$tmp" \
      CURL_LOG="$TEST_DIR/$scenario/curl.log" \
      CARGO_LOG="$TEST_DIR/$scenario/cargo.log" \
      REAL_UNAME_BIN="$REAL_UNAME_BIN" \
      REAL_READLINK_BIN="$REAL_READLINK_BIN" \
      REAL_CP_BIN="$REAL_CP_BIN" \
      REAL_MKTEMP_BIN="$REAL_MKTEMP_BIN" \
      REAL_TAR_BIN="$REAL_TAR_BIN" \
      CURL_TAG_TARBALL="$FAKE_TARBALL" \
      CURL_MAIN_TARBALL="$DIST_DIR/main.tar.gz" \
      FAKE_VERSION="$FAKE_VERSION" \
      NPM_STUB_PREFIX="$TEST_DIR/$scenario/npm-prefix" \
      REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
      REAL_LN_BIN="$REAL_LN_BIN" \
      ${envs[@]+"${envs[@]}"} \
      bash "$script" ${script_args[@]+"${script_args[@]}"} 2>&1
  )" && RUN_STATUS=0 || RUN_STATUS=$?
  touch "$TEST_DIR/$scenario/curl.log" "$TEST_DIR/$scenario/cargo.log"
}

receipt_of() { printf '%s\n' "$TEST_DIR/$1/home/.local/share/tokenless/install-receipt"; }
receipt_value() { sed -n "s/^$2=//p" "$(receipt_of "$1")" | head -1; }
receipt_files() { sed -n 's/^file=//p' "$(receipt_of "$1")"; }
curl_log() { cat "$TEST_DIR/$1/curl.log"; }
cargo_log() { cat "$TEST_DIR/$1/cargo.log"; }
# kept_copy_of <text> prints the path in a "only remaining copy is kept at <path>."
# warning, so a test can assert the backup really is still on disk.
kept_copy_of() {
  printf '%s\n' "$1" \
    | sed -n 's/.*only remaining copy is kept at \(.*\)\.$/\1/p' | head -1
}

# Whether a YAML parser is importable decides which hermes registration path a
# scenario exercises, so it is detected once here rather than per scenario. It is
# an environment fact, not a skip: every hermes scenario below asserts something
# real on both branches, and the no-parser branch can additionally be *forced*
# with a `python3 -S` wrapper (see scenario 55). CI's python has no PyYAML, so
# without this the wording assertions would pass locally and fail there.
# The adapter resolves python3 through a PATH it builds for itself:
#   $HOME/.local/bin : $HERMES_HOME/bin : /usr/local/bin : <inherited>
# so the interpreter this parent shell finds is not necessarily the one the script
# under test will use — where /usr/local/bin/python3 exists it outranks anything
# run_script put in STUB_DIR. Probing one and asserting about the other is how an
# assertion gets written for the wrong branch, which is exactly how a suite that is
# green locally failed in CI (whose setup-python has no PyYAML).
#
# So the interpreter is *pinned* per case, into $HERMES_HOME/bin, which the adapter
# prepends ahead of /usr/local/bin; pin_hermes_python then verifies under the
# adapter's own PATH construction that the pin produced the intended state.
# Detection and execution cannot diverge. Nothing is installed here: when no
# interpreter can import a YAML parser, the with-parser branch says so loudly and
# the fail-closed branch is asserted instead.
HERMES_REAL_PY3="$(command -v python3 || true)"
if [ -z "$HERMES_REAL_PY3" ]; then
  for c in /usr/local/bin/python3 /usr/bin/python3; do
    [ -x "$c" ] && HERMES_REAL_PY3="$c" && break
  done
fi
HERMES_YAML_PY3=""
for c in "$HERMES_REAL_PY3" /usr/local/bin/python3 /usr/bin/python3; do
  [ -n "$c" ] && [ -x "$c" ] || continue
  if "$c" -c 'import yaml' >/dev/null 2>&1; then HERMES_YAML_PY3="$c"; break; fi
done
HOST_YAML=0
if [ -n "$HERMES_YAML_PY3" ]; then
  HOST_YAML=1
fi

# pin_hermes_python <home> <hermes-home> <yaml|noyaml>
pin_hermes_python() {
  local hm="$1" hh="$2" mode="$3" want_rc=0 rc=0
  mkdir -p "$hh/bin"
  if [ "$mode" = "yaml" ]; then
    if [ -z "$HERMES_YAML_PY3" ]; then
      fail "pin_hermes_python: no interpreter that can import a YAML parser was found"
    fi
    printf '#!/bin/sh\nexec %s "$@"\n' "$HERMES_YAML_PY3" > "$hh/bin/python3"
    want_rc=0
  else
    if [ -z "$HERMES_REAL_PY3" ]; then
      fail "pin_hermes_python: no python3 on this host, so the no-parser branch cannot be forced"
    fi
    printf '#!/bin/sh\nexec %s -S "$@"\n' "$HERMES_REAL_PY3" > "$hh/bin/python3"
    want_rc=1
  fi
  chmod +x "$hh/bin/python3"
  # Self-check under the exact PATH the adapter builds: `import yaml` must succeed
  # for the yaml mode and fail for the noyaml mode, or the case is testing the
  # other branch than the one its assertions were written for.
  env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$hm" HERMES_HOME="$hh" bash -c \
    'export PATH="$HOME/.local/bin:${HERMES_HOME%/}/bin:/usr/local/bin:$PATH"
     command -v python3 >/dev/null 2>&1 || exit 1
     python3 -c "import yaml"' >/dev/null 2>&1 || rc=1
  if [ "$rc" != "$want_rc" ]; then
    fail "pin_hermes_python($mode): the pinned interpreter does not give the intended YAML availability (rc=$rc want=$want_rc)"
  fi
  pass "hermes: interpreter pinned for the $mode branch"
  return 0
}
if [ "$HOST_YAML" = "1" ]; then
  echo "NOTE a YAML parser is importable on this host: hermes assertions take the with-parser branch"
else
  echo "NOTE no YAML parser importable on this host: hermes assertions take the fail-closed branch"
fi

# =============================================================================
# Scenario 1 — forced source build against a real GitHub archive layout
# =============================================================================
run_script "$INSTALL_SH" source-build \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "source build exits 0" "$RUN_STATUS" "0"
S1_DIR="$TEST_DIR/source-build/home/.local/bin"
assert_file "source build installs the CLI" "$S1_DIR/tokenless"
assert_no_file "source build does not install rtk" "$S1_DIR/rtk"
assert_not_contains "no unbound-variable error from the EXIT trap" "$RUN_OUTPUT" "unbound variable"
assert_not_contains "no leftover-tempdir warning" "$RUN_OUTPUT" "No such file or directory"
assert_eq "no temporary source tree left behind" "$(find "$TEST_DIR/source-build/tmp" -mindepth 1 | wc -l | tr -d ' ')" "0"
assert_contains "requests the version tag archive" "$(curl_log source-build)" "archive/refs/tags/tokenless/v${FAKE_VERSION}.tar.gz"
assert_not_contains "never requests the main branch archive" "$(curl_log source-build)" "refs/heads/main"
assert_eq "receipt records the source method" "$(receipt_value source-build method)" "source"
assert_eq "receipt records the version" "$(receipt_value source-build version)" "$FAKE_VERSION"
assert_eq "receipt records only the CLI" "$(receipt_files source-build)" "$S1_DIR/tokenless"
assert_eq "source receipt has no npm prefix" "$(receipt_value source-build npm_prefix)" ""
assert_eq "source receipt has no adapters dir" "$(receipt_value source-build adapters_dir)" ""
assert_contains "reports CLI-only scope" "$RUN_OUTPUT" "CLI only"

# =============================================================================
# Scenario 2 — source build must not remove files it never installed
# =============================================================================
S2_DIR="$TEST_DIR/source-ownership/home/.local/bin"
run_script "$INSTALL_SH" source-ownership \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "TOKENLESS_INSTALL_DIR=$S2_DIR"
assert_eq "custom install dir source build exits 0" "$RUN_STATUS" "0"
# Foreign artefacts from another installation method (anolisa CLI / npm).
mkdir -p "$S2_DIR" "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder"
printf '#!/bin/sh\n' > "$S2_DIR/rtk"
printf '#!/bin/sh\n' > "$S2_DIR/toon"
printf 'foreign\n' > "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder/keep"
mkdir -p "$TEST_DIR/source-ownership/home/.tokenless"
printf 'stats\n' > "$TEST_DIR/source-ownership/home/.tokenless/stats.db"
assert_eq "custom install dir recorded" "$(receipt_value source-ownership install_dir)" "$S2_DIR"
assert_contains "installer appended a PATH entry before uninstall" \
  "$(cat "$TEST_DIR/source-ownership/home/.bashrc")" "# Added by tokenless installer"
run_script "$UNINSTALL_SH" source-ownership
assert_eq "uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the CLI it installed" "$S2_DIR/tokenless"
assert_file "keeps a foreign rtk" "$S2_DIR/rtk"
assert_file "keeps a foreign retired toon binary" "$S2_DIR/toon"
assert_file "keeps an adapter tree it never installed" \
  "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder/keep"
assert_file "keeps runtime data without --purge" \
  "$TEST_DIR/source-ownership/home/.tokenless/stats.db"
assert_no_file "removes the receipt" "$(receipt_of source-ownership)"
assert_not_contains "strips only the PATH entry it appended" \
  "$(cat "$TEST_DIR/source-ownership/home/.bashrc")" "tokenless installer"

# =============================================================================
# Scenario 3 — pinned version with a missing tag must fail and never fetch main
# =============================================================================
run_script "$INSTALL_SH" missing-tag \
  "TOKENLESS_VERSION=does-not-exist" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_TAG_STATUS=22"
[ "$RUN_STATUS" -ne 0 ] || fail "missing tag: expected a non-zero exit"
pass "missing tag fails the install"
assert_contains "requests the pinned tag" "$(curl_log missing-tag)" "archive/refs/tags/tokenless/vdoes-not-exist.tar.gz"
assert_not_contains "never falls back to main" "$(curl_log missing-tag)" "refs/heads/main"
assert_contains "explains the no-main-fallback contract" "$RUN_OUTPUT" "never falls back to the 'main' branch"
assert_contains "reports the missing tag" "$RUN_OUTPUT" "does not exist on alibaba/anolisa"
assert_no_file "installs nothing" "$TEST_DIR/missing-tag/home/.local/bin/tokenless"

# A transport failure (not a 404) must also stop the install without fetching main.
run_script "$INSTALL_SH" transport-failure \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_TAG_STATUS=52"
[ "$RUN_STATUS" -ne 0 ] || fail "transport failure: expected a non-zero exit"
pass "transport failure fails the install"
assert_contains "reports a transport failure, not a missing tag" "$RUN_OUTPUT" "transport failure"
assert_not_contains "transport failure never requests main" "$(curl_log transport-failure)" "refs/heads/main"
assert_no_file "transport failure installs nothing" "$TEST_DIR/transport-failure/home/.local/bin/tokenless"

# Same contract on the automatic npm -> source fallback chain.
run_script "$INSTALL_SH" missing-tag-npm-fallback \
  "TOKENLESS_VERSION=does-not-exist" \
  "NPM_STUB_FAIL=1" \
  "CURL_TAG_STATUS=22"
[ "$RUN_STATUS" -ne 0 ] || fail "npm fallback + missing tag: expected a non-zero exit"
pass "npm failure then missing tag fails the install"
assert_not_contains "npm fallback never requests main" "$(curl_log missing-tag-npm-fallback)" "refs/heads/main"

# =============================================================================
# Scenario 4 — npm path: receipt contents and ownership-aware uninstall
# =============================================================================
run_script "$INSTALL_SH" npm-install "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0" "$RUN_STATUS" "0"
S4_DIR="$TEST_DIR/npm-install/home/.local/bin"
assert_file "npm path links tokenless" "$S4_DIR/tokenless"
assert_file "npm path links rtk" "$S4_DIR/rtk"
assert_no_file "npm path does not create the retired toon binary" "$S4_DIR/toon"
assert_eq "receipt records the npm method" "$(receipt_value npm-install method)" "npm"
assert_eq "receipt records the npm prefix" "$(receipt_value npm-install npm_prefix)" "$TEST_DIR/npm-install/npm-prefix"
assert_eq "receipt records the adapters dir" "$(receipt_value npm-install adapters_dir)" \
  "$TEST_DIR/npm-install/home/.local/share/anolisa/adapters/tokenless"
assert_eq "receipt records both binaries" "$(receipt_files npm-install | tr '\n' ' ')" "$S4_DIR/tokenless $S4_DIR/rtk "
assert_file "records the rc file it modified" "$TEST_DIR/npm-install/home/.bashrc"
assert_contains "rc file carries the installer marker" \
  "$(cat "$TEST_DIR/npm-install/home/.bashrc")" "# Added by tokenless installer"

# Foreign neighbours in the same install dir must survive.
printf '#!/bin/sh\n' > "$S4_DIR/toon"
printf '#!/bin/sh\n' > "$S4_DIR/anolisa"
mkdir -p "$TEST_DIR/npm-install/home/.tokenless"
printf 'stats\n' > "$TEST_DIR/npm-install/home/.tokenless/stats.db"

run_script "$UNINSTALL_SH" npm-install -- "--dry-run"
assert_eq "dry run exits 0" "$RUN_STATUS" "0"
assert_file "dry run keeps the CLI" "$S4_DIR/tokenless"
assert_file "dry run keeps the receipt" "$(receipt_of npm-install)"

run_script "$UNINSTALL_SH" npm-install
assert_eq "npm uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the tokenless link" "$S4_DIR/tokenless"
assert_no_file "removes the rtk link" "$S4_DIR/rtk"
assert_file "keeps a foreign toon binary" "$S4_DIR/toon"
assert_file "keeps a foreign anolisa binary" "$S4_DIR/anolisa"
assert_no_file "removes the npm-owned adapters dir" \
  "$TEST_DIR/npm-install/home/.local/share/anolisa/adapters/tokenless"
assert_file "keeps runtime data without --purge" "$TEST_DIR/npm-install/home/.tokenless/stats.db"
assert_not_contains "removes the PATH entry it appended" \
  "$(cat "$TEST_DIR/npm-install/home/.bashrc")" "tokenless installer"
assert_no_file "removes the receipt" "$(receipt_of npm-install)"
assert_contains "npm global package removed" "$RUN_OUTPUT" "removed 1 package"

run_script "$UNINSTALL_SH" npm-install
[ "$RUN_STATUS" -ne 0 ] || fail "second uninstall without a receipt: expected a non-zero exit"
pass "uninstall without a receipt refuses to guess"
assert_contains "points at the per-method manual steps" "$RUN_OUTPUT" "npm uninstall -g anolisa-tokenless"

# =============================================================================
# Scenario 5 — latest version resolution still uses a tag, never main
# =============================================================================
run_script "$INSTALL_SH" latest-source \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_NPM_LATEST=$FAKE_VERSION"
assert_eq "unpinned source build exits 0" "$RUN_STATUS" "0"
assert_contains "resolves the latest version from the npm registry" "$(curl_log latest-source)" \
  "registry.npmjs.org/anolisa-tokenless/latest"
assert_contains "downloads the resolved tag" "$(curl_log latest-source)" \
  "archive/refs/tags/tokenless/v${FAKE_VERSION}.tar.gz"
assert_not_contains "unpinned build never requests main" "$(curl_log latest-source)" "refs/heads/main"

# =============================================================================
# Scenario 6 — --purge removes the runtime data directory
# =============================================================================
run_script "$INSTALL_SH" purge "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1"
assert_eq "purge scenario install exits 0" "$RUN_STATUS" "0"
mkdir -p "$TEST_DIR/purge/home/.tokenless"
printf 'stash\n' > "$TEST_DIR/purge/home/.tokenless/stash.db"
run_script "$UNINSTALL_SH" purge -- "--purge"
assert_eq "purge uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "--purge removes runtime data" "$TEST_DIR/purge/home/.tokenless/stash.db"

# =============================================================================
# Scenario 7 — a failed install(1) must fail the run, not report success
# =============================================================================
# main() calls try_source_build as an `||`/`elif` condition, so errexit is off
# inside it: without an explicit status check, `install` exiting 73 used to be
# swallowed and the run still wrote a receipt and claimed success.
run_script "$INSTALL_SH" install-write-failure \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "INSTALL_STUB_STATUS=73"
[ "$RUN_STATUS" -ne 0 ] || fail "install(1) exit 73: expected a non-zero exit"
pass "a failed install(1) fails the run"
assert_contains "reports the install(1) exit status" "$RUN_OUTPUT" "install(1) failed with exit 73"
assert_not_contains "never claims a successful install" "$RUN_OUTPUT" "installed successfully"
assert_no_file "writes no binary" "$TEST_DIR/install-write-failure/home/.local/bin/tokenless"
assert_no_file "writes no receipt" "$(receipt_of install-write-failure)"

# Same contract on the automatic npm -> source chain: neither method may report
# success when the write fails.
run_script "$INSTALL_SH" install-write-failure-fallback \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_FAIL=1" \
  "INSTALL_STUB_STATUS=73"
[ "$RUN_STATUS" -ne 0 ] || fail "npm failure + install(1) exit 73: expected a non-zero exit"
pass "a failed install(1) also fails the npm fallback chain"
assert_no_file "npm fallback writes no receipt" "$(receipt_of install-write-failure-fallback)"

# =============================================================================
# Scenario 8 — an install directory that cannot be created fails loudly
# =============================================================================
BLOCKED_DIR="$TEST_DIR/blocked-dir/blocked"
mkdir -p "$TEST_DIR/blocked-dir"
printf 'not a directory\n' > "$BLOCKED_DIR"
run_script "$INSTALL_SH" blocked-dir \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_INSTALL_DIR=$BLOCKED_DIR" \
  "CARGO_STUB_FAIL=1"
[ "$RUN_STATUS" -ne 0 ] || fail "uncreatable install dir: expected a non-zero exit"
pass "an uncreatable install directory fails the run"
assert_contains "reports the mkdir failure" "$RUN_OUTPUT" "Cannot create the install directory"
assert_no_file "uncreatable install dir writes no receipt" "$(receipt_of blocked-dir)"

# =============================================================================
# Scenario 9 — a failed ln must not adopt a pre-existing foreign binary
# =============================================================================
S9_DIR="$TEST_DIR/stale-link/home/.local/bin"
mkdir -p "$S9_DIR"
printf '#!/bin/sh\necho foreign-tokenless\n' > "$S9_DIR/tokenless"
chmod +x "$S9_DIR/tokenless"
run_script "$INSTALL_SH" stale-link \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "LN_STUB_FAIL=1" \
  "CARGO_STUB_FAIL=1"
[ "$RUN_STATUS" -ne 0 ] || fail "failed ln: expected a non-zero exit"
pass "a failed ln fails the run"
assert_contains "reports the failed link" "$RUN_OUTPUT" "Cannot write"
assert_file "leaves the pre-existing binary alone" "$S9_DIR/tokenless"
assert_contains "the pre-existing binary was not overwritten" \
  "$(cat "$S9_DIR/tokenless")" "foreign-tokenless"
assert_no_file "failed ln writes no receipt" "$(receipt_of stale-link)"
assert_not_contains "never claims a successful install" "$RUN_OUTPUT" "installed successfully"

# =============================================================================
# Scenario 10 — switching method retires the previous receipt's artefacts
# =============================================================================
run_script "$INSTALL_SH" switch-method "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0 before the switch" "$RUN_STATUS" "0"
S10_DIR="$TEST_DIR/switch-method/home/.local/bin"
S10_NPM="$TEST_DIR/switch-method/npm-prefix/lib/node_modules/anolisa-tokenless"
S10_ADAPTERS="$TEST_DIR/switch-method/home/.local/share/anolisa/adapters/tokenless"
assert_file "npm path linked rtk" "$S10_DIR/rtk"
assert_file "npm path installed the global package" "$S10_NPM"
assert_file "npm path placed the adapter tree" "$S10_ADAPTERS/manifest.json"
assert_eq "receipt records the npm method before the switch" "$(receipt_value switch-method method)" "npm"

run_script "$INSTALL_SH" switch-method \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "source reinstall over an npm install exits 0" "$RUN_STATUS" "0"
assert_eq "receipt now records the source method" "$(receipt_value switch-method method)" "source"
assert_eq "receipt records only the CLI" "$(receipt_files switch-method)" "$S10_DIR/tokenless"
assert_file "the CLI itself survives the switch" "$S10_DIR/tokenless"
assert_no_file "removes the rtk link the npm method left behind" "$S10_DIR/rtk"
assert_no_file "removes the npm global package the previous method installed" "$S10_NPM"
assert_no_file "removes the adapter tree the previous method owned" "$S10_ADAPTERS"
assert_contains "reports that the previous install was kept aside first" "$RUN_OUTPUT" \
  "Kept the previous npm install aside"
assert_contains "reports the npm package it retired" "$RUN_OUTPUT" \
  "Removing the npm package left behind by the previous install"

# The retired tree must also be gone for a later uninstall.sh run, and the new
# receipt must not claim artefacts this run never created.
run_script "$UNINSTALL_SH" switch-method
assert_eq "uninstall after the switch exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the source-built CLI" "$S10_DIR/tokenless"
assert_no_file "no adapter tree is left to clean" "$S10_ADAPTERS"

# =============================================================================
# Scenario 11 — a recorded path taken over by another installer is kept
# =============================================================================
run_script "$INSTALL_SH" taken-over \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "taken-over scenario install exits 0" "$RUN_STATUS" "0"
S11_CLI="$TEST_DIR/taken-over/home/.local/bin/tokenless"
assert_file "records the CLI it installed" "$S11_CLI"
# anolisa (or a manual npm install) later replaces the very same path.
printf '#!/bin/sh\necho "anolisa-managed tokenless"\n' > "$S11_CLI"
chmod +x "$S11_CLI"
run_script "$UNINSTALL_SH" taken-over
assert_eq "uninstall with a replaced file exits 0" "$RUN_STATUS" "0"
assert_file "keeps the file another installer put at the recorded path" "$S11_CLI"
assert_contains "the surviving file is the foreign one" "$(cat "$S11_CLI")" "anolisa-managed"
assert_contains "explains why the path was skipped" "$RUN_OUTPUT" "another installation has taken over that path"
assert_no_file "still removes the receipt" "$(receipt_of taken-over)"

# Same protection for the adapter tree.
run_script "$INSTALL_SH" adapters-taken-over "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "adapters-taken-over install exits 0" "$RUN_STATUS" "0"
S11_ADAPTERS="$TEST_DIR/adapters-taken-over/home/.local/share/anolisa/adapters/tokenless"
printf '{"component":"tokenless","version":"99.0.0-replaced"}\n' > "$S11_ADAPTERS/manifest.json"
run_script "$UNINSTALL_SH" adapters-taken-over
assert_eq "uninstall with a replaced adapter tree exits 0" "$RUN_STATUS" "0"
assert_file "keeps an adapter tree another installer replaced" "$S11_ADAPTERS/manifest.json"
assert_contains "explains why the adapter tree was skipped" "$RUN_OUTPUT" \
  "another installation has taken over that adapter tree"

# A schema-1 receipt carries no identity, so the uninstaller says so instead of
# silently deleting by path.
S11_LEGACY="$TEST_DIR/legacy-receipt/home"
mkdir -p "$S11_LEGACY/.local/share/tokenless" "$S11_LEGACY/.local/bin"
printf '#!/bin/sh\necho legacy\n' > "$S11_LEGACY/.local/bin/tokenless"
cat > "$S11_LEGACY/.local/share/tokenless/install-receipt" <<LEGACY
# Tokenless installer receipt (schema 1).
schema=1
method=source
version=$FAKE_VERSION
install_dir=$S11_LEGACY/.local/bin
file=$S11_LEGACY/.local/bin/tokenless
LEGACY
run_script "$UNINSTALL_SH" legacy-receipt
assert_eq "schema-1 uninstall exits 0" "$RUN_STATUS" "0"
assert_contains "warns that a schema-1 receipt has no identity" "$RUN_OUTPUT" "records no file identity"
assert_no_file "still removes the recorded path" "$S11_LEGACY/.local/bin/tokenless"

# =============================================================================
# Scenario 12 — adapter install -> enable -> uninstall, with a link-type adapter
# =============================================================================
run_script "$INSTALL_SH" adapter-lifecycle \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless"
assert_eq "install with a bundled adapter payload exits 0" "$RUN_STATUS" "0"
S12_ADAPTERS="$TEST_DIR/adapter-lifecycle/home/.local/share/anolisa/adapters/tokenless"
assert_file "postinstall placed the real qwencode adapter" "$S12_ADAPTERS/qwencode/scripts/uninstall.sh"

# Enable: run the repository's own adapter install script, which links the
# extension into the framework home.
S12_HOME="$TEST_DIR/adapter-lifecycle/home"
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S12_HOME" \
    SHELL=/bin/bash \
    REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
    REAL_LN_BIN="$REAL_LN_BIN" \
    bash "$S12_ADAPTERS/qwencode/scripts/install.sh" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "adapter enable exits 0" "$RUN_STATUS" "0"
S12_LINK="$S12_HOME/.qwen/extensions/tokenless"
assert_file "enable linked the extension into the framework home" "$S12_LINK"
assert_eq "the framework link points into the adapter tree" \
  "$(readlink "$S12_LINK")" "$S12_ADAPTERS/qwencode"

run_script "$UNINSTALL_SH" adapter-lifecycle -- "--dry-run"
assert_eq "dry run exits 0" "$RUN_STATUS" "0"
assert_contains "dry run announces the deregistration" "$RUN_OUTPUT" \
  "[dry-run] would deregister the qwencode adapter"
assert_file "dry run keeps the framework link" "$S12_LINK"
assert_file "dry run keeps the adapter tree" "$S12_ADAPTERS/qwencode/scripts/uninstall.sh"

run_script "$UNINSTALL_SH" adapter-lifecycle
assert_eq "uninstall after enable exits 0" "$RUN_STATUS" "0"
assert_contains "deregisters the framework adapter" "$RUN_OUTPUT" "Deregistered the qwencode adapter"
assert_no_file "leaves no dangling framework link behind" "$S12_LINK"
assert_no_file "removes the adapter resources after deregistering" "$S12_ADAPTERS"
assert_no_file "removes the npm-owned CLI link" "$TEST_DIR/adapter-lifecycle/home/.local/bin/tokenless"

# =============================================================================
# Scenario 13 — re-running must not stack PATH entries uninstall.sh cannot strip
# =============================================================================
run_script "$INSTALL_SH" path-idempotent "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "first install exits 0" "$RUN_STATUS" "0"
S13_RC="$TEST_DIR/path-idempotent/home/.bashrc"
run_script "$INSTALL_SH" path-idempotent "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "second install exits 0" "$RUN_STATUS" "0"
assert_eq "the rc file carries exactly one installer marker" \
  "$(grep -cF "# Added by tokenless installer" "$S13_RC")" "1"
assert_contains "the second run reports the entry it already owns" "$RUN_OUTPUT" "already adds"
assert_eq "receipt records the rc file once" \
  "$(sed -n 's/^path_rc_file=//p' "$(receipt_of path-idempotent)" | wc -l | tr -d ' ')" "1"
run_script "$UNINSTALL_SH" path-idempotent
assert_eq "uninstall after the re-run exits 0" "$RUN_STATUS" "0"
assert_not_contains "no PATH entry survives the uninstall" "$(cat "$S13_RC")" "tokenless installer"

# =============================================================================
# Scenario 14 — a failed npm attempt is rolled back and does NOT switch method
# =============================================================================
# npm succeeds, the launcher links cannot be written. Everything the npm route put
# on disk has to go with it — an unowned global package, `rtk` link or adapter tree
# would otherwise survive with no uninstaller that can see it. But npm has *run*,
# and no exit status proves its postinstall left no framework registration behind,
# so the run reports itself incomplete instead of building from source and
# announcing success over a machine it cannot fully account for.
run_script "$INSTALL_SH" npm-rollback \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "LN_STUB_FAIL=1"
assert_eq "a failure after npm ran exits non-zero" "$RUN_STATUS" "1"
assert_contains "says no other method will be tried automatically" "$RUN_OUTPUT" \
  "no other method will be tried automatically"
assert_contains "says it refuses to switch after npm ran" "$RUN_OUTPUT" \
  "Refusing to switch install method after npm ran"
assert_not_contains "never starts the source build" "$RUN_OUTPUT" "Building from source"
S14_HOME="$TEST_DIR/npm-rollback/home"
S14_NPM="$TEST_DIR/npm-rollback/npm-prefix"
assert_no_file "records no receipt for an install that did not complete" \
  "$(receipt_of npm-rollback)"
assert_no_file "the failed npm attempt left no rtk link behind" "$S14_HOME/.local/bin/rtk"
assert_no_file "the failed npm attempt's global package was rolled back" \
  "$S14_NPM/lib/node_modules/anolisa-tokenless"
assert_contains "reports the package it rolled back" "$RUN_OUTPUT" \
  "Removing the npm package the failed attempt installed"
# The tree this attempt created is *kept*: the script cannot tell which framework
# registrations the attempt added, and running every bundled adapter uninstall
# script to find out deregisters frameworks this install never touched.
assert_file "keeps the adapter tree it cannot prove is safe to remove" \
  "$S14_HOME/.local/share/anolisa/adapters/tokenless"
# Wrapped across two lines in the script's own output, so match within one line.
assert_contains "says it deregisters nothing and deletes nothing there" "$RUN_OUTPUT" \
  "so it deregisters nothing and deletes"
assert_contains "and names the directory to clean up" "$RUN_OUTPUT" \
  "rm -rf $S14_HOME/.local/share/anolisa/adapters/tokenless"
# The source build is still reachable — as an explicit choice, which is what
# TOKENLESS_FORCE_BUILD=1 is for, and what keeps "a Linux user with no Node can
# still install" true without an automatic cross-method switch.
run_script "$INSTALL_SH" npm-rollback \
  "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1"
assert_eq "an explicit source build still exits 0" "$RUN_STATUS" "0"
assert_eq "and records the source method" "$(receipt_value npm-rollback method)" "source"
assert_file "the source-built CLI is there" "$S14_HOME/.local/bin/tokenless"

# =============================================================================
# Scenario 15 — an npm prefix whose bin directory is the install directory
# =============================================================================
# `npm install -g --prefix ~/.local` puts its bin links in ~/.local/bin, which is
# also the installer's default install directory. Retiring that package *after*
# a source build has written ~/.local/bin/tokenless deletes the CLI this run just
# installed, so the retirement has to happen first.
S15_HOME="$TEST_DIR/npm-prefix-overlap/home"
S15_PKG="$S15_HOME/.local/lib/node_modules/anolisa-tokenless"
run_script "$INSTALL_SH" npm-prefix-overlap \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S15_HOME/.local" \
  "TOKENLESS_INSTALL_DIR=$S15_HOME/.local/bin"
assert_eq "npm install into an overlapping prefix exits 0" "$RUN_STATUS" "0"
assert_eq "receipt records the npm method" "$(receipt_value npm-prefix-overlap method)" "npm"
assert_eq "receipt records the overlapping npm prefix" \
  "$(receipt_value npm-prefix-overlap npm_prefix)" "$S15_HOME/.local"
assert_file "the npm route placed the CLI" "$S15_HOME/.local/bin/tokenless"
assert_file "the npm route placed rtk" "$S15_HOME/.local/bin/rtk"
assert_file "the npm route installed the global package" "$S15_PKG"

run_script "$INSTALL_SH" npm-prefix-overlap \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S15_HOME/.local" \
  "TOKENLESS_INSTALL_DIR=$S15_HOME/.local/bin" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "source build over an overlapping npm prefix exits 0" "$RUN_STATUS" "0"
assert_eq "receipt now records the source method" "$(receipt_value npm-prefix-overlap method)" "source"
assert_file "the source-built CLI survives retiring the overlapping package" \
  "$S15_HOME/.local/bin/tokenless"
if [ -L "$S15_HOME/.local/bin/tokenless" ]; then
  fail "the CLI at the overlapping path is still the npm link"
fi
pass "the CLI at the overlapping path is a regular file, not the npm link"
assert_contains "the surviving CLI is the source build" \
  "$("$S15_HOME/.local/bin/tokenless" --version 2>&1)" "-src"
assert_no_file "removes rtk from the overlapping prefix" "$S15_HOME/.local/bin/rtk"
assert_no_file "removes the npm global package from the overlapping prefix" "$S15_PKG"
assert_contains "retires the previous npm package before writing" "$RUN_OUTPUT" \
  "Removing the npm package left behind by the previous install"

run_script "$UNINSTALL_SH" npm-prefix-overlap
assert_eq "uninstall after the overlapping switch exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the source-built CLI" "$S15_HOME/.local/bin/tokenless"

# =============================================================================
# Scenario 16 — macOS has no source-build route, so no cargo and no false success
# =============================================================================
run_script "$INSTALL_SH" darwin-x64-npm-failure \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "UNAME_STUB_OS=Darwin" \
  "UNAME_STUB_ARCH=x86_64" \
  "NPM_STUB_FAIL=1"
assert_eq "Intel macOS with a failing npm route exits non-zero" "$RUN_STATUS" "1"
assert_eq "Intel macOS never invokes cargo" "$(cargo_log darwin-x64-npm-failure)" ""
assert_no_file "Intel macOS writes no CLI" \
  "$TEST_DIR/darwin-x64-npm-failure/home/.local/bin/tokenless"
assert_no_file "Intel macOS writes no receipt" "$(receipt_of darwin-x64-npm-failure)"
assert_contains "says the source build is not supported on macOS" "$RUN_OUTPUT" \
  "Source builds are not supported on macOS"
assert_contains "says Intel macOS has no supported install route" "$RUN_OUTPUT" \
  "no supported install route"

run_script "$INSTALL_SH" darwin-force-build \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "UNAME_STUB_OS=Darwin" \
  "UNAME_STUB_ARCH=arm64" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "a forced source build on macOS exits non-zero" "$RUN_STATUS" "1"
assert_eq "a forced source build on macOS never invokes cargo" "$(cargo_log darwin-force-build)" ""
assert_contains "points at the prebuilt route instead" "$RUN_OUTPUT" \
  "npm install -g anolisa-tokenless"

# =============================================================================
# Scenario 17 — a shared adapter directory owned by another installation
# =============================================================================
# ~/.local/share/anolisa/adapters/tokenless is shared with the anolisa CLI and
# with a direct `npm install -g`, and the npm postinstall replaces it wholesale.
# A tree that was already there must survive, must not be claimed by the receipt,
# and must not be deregistered or deleted by a later uninstall.
S17_HOME="$TEST_DIR/foreign-adapters/home"
S17_ADAPTERS="$S17_HOME/.local/share/anolisa/adapters/tokenless"
S17_LINK="$S17_HOME/.qwen/extensions/tokenless"
mkdir -p "$S17_ADAPTERS"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/qwencode" "$S17_ADAPTERS/qwencode"
printf '{"component":"tokenless","version":"0.6.0-anolisa"}\n' > "$S17_ADAPTERS/manifest.json"
S17_MANIFEST_BEFORE="$(cat "$S17_ADAPTERS/manifest.json")"
# The pre-existing installation is enabled: the registration points into the tree.
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S17_HOME" \
    SHELL=/bin/bash \
    REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
    REAL_LN_BIN="$REAL_LN_BIN" \
    bash "$S17_ADAPTERS/qwencode/scripts/install.sh" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "the pre-existing adapter can be enabled" "$RUN_STATUS" "0"
assert_eq "the pre-existing registration points into the shared tree" \
  "$(readlink "$S17_LINK")" "$S17_ADAPTERS/qwencode"

run_script "$INSTALL_SH" foreign-adapters \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless"
assert_eq "npm install next to a foreign adapter tree exits 0" "$RUN_STATUS" "0"
assert_eq "receipt records the npm method" "$(receipt_value foreign-adapters method)" "npm"
assert_eq "receipt claims no adapter directory" "$(receipt_value foreign-adapters adapters_dir)" ""
assert_file "the CLI this run linked is there" "$S17_HOME/.local/bin/tokenless"
assert_eq "the foreign manifest survives the npm postinstall" \
  "$(cat "$S17_ADAPTERS/manifest.json")" "$S17_MANIFEST_BEFORE"
assert_eq "the foreign registration still points into the shared tree" \
  "$(readlink "$S17_LINK")" "$S17_ADAPTERS/qwencode"
assert_contains "says the directory belongs to another installation" "$RUN_OUTPUT" \
  "already belonged to another installation"

run_script "$UNINSTALL_SH" foreign-adapters
assert_eq "uninstall next to a foreign adapter tree exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the CLI link this run created" "$S17_HOME/.local/bin/tokenless"
assert_file "keeps the foreign adapter tree" "$S17_ADAPTERS/manifest.json"
assert_file "keeps the foreign framework registration" "$S17_LINK"
assert_eq "the foreign registration is unchanged by the uninstall" \
  "$(readlink "$S17_LINK")" "$S17_ADAPTERS/qwencode"
assert_not_contains "does not deregister a framework it does not own" "$RUN_OUTPUT" \
  "Deregistered the qwencode adapter"

# =============================================================================
# Scenario 18 — adapter deregistration must not remove the component binary
# =============================================================================
# The adapters' own uninstall.sh scripts are full uninstallers, and the Codex one
# also removes $PREFIX/bin/tokenless. When the receipt-driven uninstaller has
# decided to keep that binary because another installation took the path over,
# deregistering the adapter must not get a second chance at deleting it.
run_script "$INSTALL_SH" codex-deregister "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install with a Codex adapter in the tree exits 0" "$RUN_STATUS" "0"
S18_ADAPTERS="$TEST_DIR/codex-deregister/home/.local/share/anolisa/adapters/tokenless"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/codex" "$S18_ADAPTERS/codex"
S18_CLI="$TEST_DIR/codex-deregister/home/.local/bin/tokenless"
# Replace the link with a real file, the way another installer taking the path
# over would: writing through the link would only rewrite the npm payload it
# points at and leave a dangling link behind once that package is uninstalled.
rm -f "$S18_CLI"
printf '#!/bin/sh\necho "anolisa-managed tokenless"\n' > "$S18_CLI"
chmod +x "$S18_CLI"

run_script "$UNINSTALL_SH" codex-deregister
assert_eq "uninstall with a taken-over binary and a Codex adapter exits 0" "$RUN_STATUS" "0"
assert_file "keeps the binary another installation took over" "$S18_CLI"
assert_contains "the surviving binary is the foreign one" "$(cat "$S18_CLI")" "anolisa-managed"
assert_contains "still deregisters the Codex adapter" "$RUN_OUTPUT" "Deregistered the codex adapter"
assert_no_file "removes the adapter resources it owns" "$S18_ADAPTERS"

# The contract the Codex adapter script implements for that caller: with
# TOKENLESS_DEREGISTER_ONLY=1 the registration goes and the binary stays; an
# explicit --non-interactive run of the same script keeps removing it.
S18_HOME="$TEST_DIR/codex-contract/home"
mkdir -p "$S18_HOME/.local/bin"
printf '#!/bin/sh\necho kept\n' > "$S18_HOME/.local/bin/tokenless"
chmod +x "$S18_HOME/.local/bin/tokenless"
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S18_HOME" \
    SHELL=/bin/bash \
    TOKENLESS_DEREGISTER_ONLY=1 \
    bash "$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "deregistration-only mode exits 0" "$RUN_STATUS" "0"
assert_file "deregistration-only mode keeps the component binary" "$S18_HOME/.local/bin/tokenless"
assert_contains "deregistration-only mode says why it keeps the binary" "$RUN_OUTPUT" \
  "Deregistration only"

printf '#!/bin/sh\necho removed\n' > "$S18_HOME/.local/bin/tokenless"
chmod +x "$S18_HOME/.local/bin/tokenless"
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S18_HOME" \
    SHELL=/bin/bash \
    bash "$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh" --non-interactive 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "an explicit --non-interactive Codex uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "an explicit --non-interactive Codex uninstall still removes the binary" \
  "$S18_HOME/.local/bin/tokenless"

# =============================================================================
# Scenario 19 — a replacement that fails halfway keeps the previous install
# =============================================================================
# Retiring the previous npm install before the new one is verified used to leave
# a machine with nothing at all when the replacement failed: the old CLI, the rtk
# launcher and the global package were already gone, the run exited non-zero, and
# the old receipt still described an installation that no longer existed.
run_script "$INSTALL_SH" failed-upgrade "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0 before the failed upgrade" "$RUN_STATUS" "0"
S19_HOME="$TEST_DIR/failed-upgrade/home"
S19_NPM="$TEST_DIR/failed-upgrade/npm-prefix"
S19_ADAPTERS="$S19_HOME/.local/share/anolisa/adapters/tokenless"
assert_contains "the CLI works before the upgrade" \
  "$("$S19_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"

run_script "$INSTALL_SH" failed-upgrade \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_TAG_STATUS=22"
assert_eq "a source build whose tag is missing exits non-zero" "$RUN_STATUS" "1"
assert_file "keeps the CLI of the install it failed to replace" "$S19_HOME/.local/bin/tokenless"
assert_file "keeps the rtk launcher of the install it failed to replace" "$S19_HOME/.local/bin/rtk"
assert_contains "the kept CLI still runs" \
  "$("$S19_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
assert_file "keeps the npm global package" "$S19_NPM/lib/node_modules/anolisa-tokenless"
assert_file "keeps the adapter resources" "$S19_ADAPTERS/manifest.json"
assert_eq "the receipt still describes the install that is there" \
  "$(receipt_value failed-upgrade method)" "npm"
assert_contains "says the previous install was kept aside first" "$RUN_OUTPUT" \
  "Kept the previous npm install aside"
assert_contains "says it was put back" "$RUN_OUTPUT" "did not produce a working replacement"

# Same guarantee when the download works but the build does not.
run_script "$INSTALL_SH" failed-build "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0 before the failed build" "$RUN_STATUS" "0"
run_script "$INSTALL_SH" failed-build \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CARGO_STUB_FAIL=1"
assert_eq "a failing cargo build exits non-zero" "$RUN_STATUS" "1"
assert_contains "the kept CLI still runs after a failed build" \
  "$("$TEST_DIR/failed-build/home/.local/bin/tokenless" --version 2>&1)" "-npm"
assert_file "keeps the rtk launcher after a failed build" \
  "$TEST_DIR/failed-build/home/.local/bin/rtk"
assert_eq "the receipt still records npm after a failed build" \
  "$(receipt_value failed-build method)" "npm"

# A successful replacement still retires what it superseded.
run_script "$INSTALL_SH" failed-upgrade \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "the retry with a reachable tag exits 0" "$RUN_STATUS" "0"
assert_eq "receipt now records the source method" "$(receipt_value failed-upgrade method)" "source"
assert_contains "the CLI is the source build now" \
  "$("$S19_HOME/.local/bin/tokenless" --version 2>&1)" "-src"
assert_no_file "retires the rtk launcher once the replacement worked" "$S19_HOME/.local/bin/rtk"
assert_no_file "retires the npm global package once the replacement worked" \
  "$S19_NPM/lib/node_modules/anolisa-tokenless"
assert_no_file "retires the adapter tree once the replacement worked" "$S19_ADAPTERS"

# =============================================================================
# Scenario 20 — a readlink(1) without -f (BSD readlink, macOS before 12.3)
# =============================================================================
# `readlink -f` prints nothing and exits non-zero there. Every caller reads an
# empty result as "this is not the path we wrote", so without a portable
# resolution neither launcher would be recorded, the rollback could not identify
# them either, and the run would fail leaving dangling links behind.
run_script "$INSTALL_SH" bsd-readlink \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "READLINK_STUB_NO_F=1"
assert_eq "npm install exits 0 without readlink -f" "$RUN_STATUS" "0"
S20_HOME="$TEST_DIR/bsd-readlink/home"
S20_NPM="$TEST_DIR/bsd-readlink/npm-prefix"
assert_eq "receipt records the npm method" "$(receipt_value bsd-readlink method)" "npm"
assert_eq "both launchers are recorded without readlink -f" "$(receipt_files bsd-readlink)" \
  "$S20_HOME/.local/bin/tokenless
$S20_HOME/.local/bin/rtk"
assert_eq "the receipt records the resolved link target" \
  "$(receipt_value bsd-readlink file_target)" \
  "$S20_NPM/lib/node_modules/anolisa-tokenless/bin/tokenless"
assert_contains "the recorded CLI runs" \
  "$("$S20_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"

# The rollback path resolves links with the same helper, so a failed attempt is
# undone on BSD readlink too: npm "succeeds", the CLI it shipped does not run, and
# the rollback has to identify and remove both launchers. npm has run by then, so
# the run reports itself incomplete rather than switching to the source build.
run_script "$INSTALL_SH" bsd-rollback \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "READLINK_STUB_NO_F=1" \
  "NPM_STUB_BROKEN_BIN=1"
assert_eq "a broken npm CLI does not switch method automatically" "$RUN_STATUS" "1"
assert_contains "says it refuses to switch after npm ran" "$RUN_OUTPUT" \
  "Refusing to switch install method after npm ran"
S20B_HOME="$TEST_DIR/bsd-rollback/home"
assert_no_file "records no receipt for an install that did not complete" \
  "$(receipt_of bsd-rollback)"
assert_no_file "the rolled-back attempt left no rtk link" "$S20B_HOME/.local/bin/rtk"
assert_no_file "the rolled-back attempt's package is gone" \
  "$TEST_DIR/bsd-rollback/npm-prefix/lib/node_modules/anolisa-tokenless"
assert_file "the rolled-back attempt's adapter tree is kept, not blind-deleted" \
  "$S20B_HOME/.local/share/anolisa/adapters/tokenless"

# =============================================================================
# Scenario 21 — identical content placed by a newer install is not ours to delete
# =============================================================================
# Every copy of the same release is byte-identical, so a content hash cannot tell
# "still the file this receipt recorded" from "a newer anolisa or npm install put
# the same bytes here". Ownership is the recorded link target plus the marker the
# installer stamped into the artefacts that can carry one.
run_script "$INSTALL_SH" same-version "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install exits 0" "$RUN_STATUS" "0"
S21_HOME="$TEST_DIR/same-version/home"
S21_NPM="$TEST_DIR/same-version/npm-prefix"
S21_ADAPTERS="$S21_HOME/.local/share/anolisa/adapters/tokenless"
S21_PKG="$S21_NPM/lib/node_modules/anolisa-tokenless"
assert_eq "receipt claims the adapter tree" "$(receipt_value same-version adapters_dir)" "$S21_ADAPTERS"
assert_contains "the adapter tree carries this run's ownership marker" \
  "$(cat "$S21_ADAPTERS/.tokenless-owner")" "curl-installer:"
assert_eq "the npm package carries this run's ownership marker" \
  "$(head -n1 "$S21_PKG/.tokenless-owner")" "$(grep -m1 '^adapters_dir_owner=' "$(receipt_of same-version)" | cut -d= -f2-)"

# A newer installation of the same version takes the paths over: byte-identical
# launcher content at the recorded paths, and the same manifest in the adapter
# tree. Every recorded sha256 still matches.
rm -f "$S21_HOME/.local/bin/tokenless" "$S21_HOME/.local/bin/rtk"
cp "$S21_NPM/bin/tokenless" "$S21_HOME/.local/bin/tokenless"
cp "$S21_NPM/bin/rtk" "$S21_HOME/.local/bin/rtk"
printf 'npm:anolisa-tokenless@%s\n' "$FAKE_VERSION" > "$S21_ADAPTERS/.tokenless-owner"
printf 'npm:anolisa-tokenless@%s\n' "$FAKE_VERSION" > "$S21_PKG/.tokenless-owner"

run_script "$UNINSTALL_SH" same-version
assert_eq "uninstall over a same-version takeover exits 0" "$RUN_STATUS" "0"
assert_file "keeps the launcher whose bytes match but whose identity does not" \
  "$S21_HOME/.local/bin/tokenless"
assert_file "keeps the rtk launcher a newer install placed" "$S21_HOME/.local/bin/rtk"
assert_file "keeps the adapter tree a newer install replaced" "$S21_ADAPTERS/manifest.json"
assert_eq "keeps the newer adapter ownership marker" \
  "$(cat "$S21_ADAPTERS/.tokenless-owner")" "npm:anolisa-tokenless@$FAKE_VERSION"
assert_file "keeps the npm package a newer install owns" "$S21_PKG"
assert_contains "explains that the launcher identity changed" "$RUN_OUTPUT" \
  "it is no longer the artefact this receipt recorded"
assert_contains "explains that the adapter marker is newer" "$RUN_OUTPUT" \
  "ownership marker belongs to a newer"
assert_contains "explains that the npm package marker is newer" "$RUN_OUTPUT" \
  "Skipping the npm package in"
assert_not_contains "does not deregister frameworks it no longer owns" "$RUN_OUTPUT" \
  "Deregistered the"
assert_no_file "still removes the receipt" "$(receipt_of same-version)"

# A schema-2 receipt carries no identity beyond content, and must keep working.
S21_LEGACY="$TEST_DIR/legacy-schema2/home"
mkdir -p "$S21_LEGACY/.local/share/tokenless" "$S21_LEGACY/.local/bin"
printf '#!/bin/sh\necho legacy2\n' > "$S21_LEGACY/.local/bin/tokenless"
chmod +x "$S21_LEGACY/.local/bin/tokenless"
LEGACY_DIGEST="$(sha256sum "$S21_LEGACY/.local/bin/tokenless" | cut -d' ' -f1)"
cat > "$S21_LEGACY/.local/share/tokenless/install-receipt" <<LEGACY2
# Tokenless installer receipt (schema 2).
schema=2
method=source
version=$FAKE_VERSION
install_dir=$S21_LEGACY/.local/bin
file=$S21_LEGACY/.local/bin/tokenless
file_digest=$LEGACY_DIGEST
LEGACY2
run_script "$UNINSTALL_SH" legacy-schema2
assert_eq "schema-2 uninstall exits 0" "$RUN_STATUS" "0"
assert_contains "warns that a schema-2 receipt records no ownership" "$RUN_OUTPUT" \
  "records content but no install ownership"
assert_no_file "still removes the recorded path" "$S21_LEGACY/.local/bin/tokenless"

# =============================================================================
# Scenario 22 — a broken npm upgrade with a failing fallback keeps the old CLI
# =============================================================================
# The launcher links a rollback restores resolve *into* the npm package, so
# putting the links back without the payload they point at leaves a CLI that runs
# whatever the failed upgrade installed — broken, while the old receipt still
# describes a working install.
run_script "$INSTALL_SH" npm-upgrade-broken "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
S22_HOME="$TEST_DIR/npm-upgrade-broken/home"
S22_NPM="$TEST_DIR/npm-upgrade-broken/npm-prefix"
S22_CLI="$S22_HOME/.local/bin/tokenless"
assert_contains "the installed CLI works" "$("$S22_CLI" --version 2>&1)" "-npm"
S22_ID="$(receipt_value npm-upgrade-broken install_id)"

run_script "$INSTALL_SH" npm-upgrade-broken \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_BROKEN_BIN=1" \
  "CARGO_STUB_FAIL=1"
assert_eq "a broken upgrade whose fallback also fails exits non-zero" "$RUN_STATUS" "1"
assert_file "the launcher survives the failed upgrade" "$S22_CLI"
assert_contains "the restored CLI still reports the previous payload" \
  "$("$S22_CLI" --version 2>&1)" "-npm"
# The broken stub prints the same version string and then exits 1, so the exit
# status is what proves the *previous* payload is back rather than the one the
# failed upgrade installed and the launcher still resolves into.
if "$S22_CLI" --version >/dev/null 2>&1; then
  pass "the restored CLI actually runs"
else
  fail "the restored CLI does not run: the previous package payload was not put back"
fi
assert_file "the previous package payload was put back" \
  "$S22_NPM/lib/node_modules/anolisa-tokenless"
assert_contains "reports the package it restored" "$RUN_OUTPUT" \
  "Restored the anolisa-tokenless package the failed upgrade replaced"
assert_eq "the receipt still carries the previous install id" \
  "$(receipt_value npm-upgrade-broken install_id)" "$S22_ID"
assert_eq "the receipt still records the npm method" \
  "$(receipt_value npm-upgrade-broken method)" "npm"
run_script "$UNINSTALL_SH" npm-upgrade-broken
assert_eq "the surviving install still uninstalls cleanly" "$RUN_STATUS" "0"
assert_no_file "removes the CLI it recorded" "$S22_CLI"
assert_no_file "removes the package it recorded" "$S22_NPM/lib/node_modules/anolisa-tokenless"

# =============================================================================
# Scenario 23 — a receipt that cannot be updated must not leave a stale record
# =============================================================================
# Retiring the previous install before the new receipt is known to be writable
# left a stale receipt behind when the write failed. A same-version reinstall
# reproduces the recorded digests and link targets, so a later uninstall.sh would
# have deleted the *new* install's CLI as though it were the old one's.
if [ "$(id -u)" = "0" ]; then
  echo "SKIP the read-only receipt scenario cannot be exercised as root"
else
  run_script "$INSTALL_SH" stale-receipt "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
  S23_HOME="$TEST_DIR/stale-receipt/home"
  S23_CLI="$S23_HOME/.local/bin/tokenless"
  S23_RCDIR="$S23_HOME/.local/share/tokenless"
  S23_ID="$(receipt_value stale-receipt install_id)"
  assert_contains "the installed CLI works" "$("$S23_CLI" --version 2>&1)" "-npm"

  chmod 0555 "$S23_RCDIR"
  run_script "$INSTALL_SH" stale-receipt "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "a reinstall that cannot update the receipt exits non-zero" "$RUN_STATUS" "1"
  assert_contains "says why it refuses to leave the stale receipt behind" "$RUN_OUTPUT" \
    "previous receipt cannot be removed"
  assert_file "keeps the CLI of the install it could not record" "$S23_CLI"
  assert_contains "the kept CLI still runs" "$("$S23_CLI" --version 2>&1)" "-npm"
  assert_eq "the stale receipt was neither overwritten nor applied" \
    "$(receipt_value stale-receipt install_id)" "$S23_ID"
  chmod 0755 "$S23_RCDIR"

  # The receipt still describes exactly what is on disk, so the uninstaller
  # removes that install and nothing newer.
  run_script "$UNINSTALL_SH" stale-receipt
  assert_eq "uninstalling the surviving install exits 0" "$RUN_STATUS" "0"
  assert_no_file "removes the CLI it recorded" "$S23_CLI"
  assert_no_file "removes the receipt" "$(receipt_of stale-receipt)"
fi

# =============================================================================
# Scenario 24 — a failed restore keeps the only remaining copy
# =============================================================================
# Claiming "the previous file is kept at <staging dir>" and then deleting that
# directory unconditionally loses both the installation and its backup when the
# copy back fails (full disk, permissions changed underneath the run).
run_script "$INSTALL_SH" restore-fails "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the install before the failed restore exits 0" "$RUN_STATUS" "0"
S24_CLI="$TEST_DIR/restore-fails/home/.local/bin/tokenless"
run_script "$INSTALL_SH" restore-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CARGO_STUB_FAIL=1" \
  "CP_STUB_FAIL_DEST=$S24_CLI"
assert_eq "a failed build whose restore also fails exits non-zero" "$RUN_STATUS" "1"
assert_contains "reports that the only copy was kept" "$RUN_OUTPUT" \
  "the only remaining copy is kept at"
assert_contains "reports the staging directory it did not delete" "$RUN_OUTPUT" \
  "Recover them from"
S24_KEPT="$(kept_copy_of "$RUN_OUTPUT")"
[ -n "$S24_KEPT" ] || fail "could not read the kept copy path out of the output"
pass "the warning names the copy it kept"
assert_file "the staged copy of the CLI was not deleted" "$S24_KEPT"
assert_contains "says the snapshot directory is kept" "$RUN_OUTPUT" \
  "it still holds the only copy"

# A foreign adapter tree is a different case: there the copy that cannot be put
# back is the only one that exists anywhere, the npm postinstall has already
# replaced it, and framework registrations point into it. Reporting success would
# claim those resources and registrations are unchanged, which is the one thing
# they are not, so the run has to fail.
S24B_HOME="$TEST_DIR/foreign-restore-fails/home"
S24B_ADAPTERS="$S24B_HOME/.local/share/anolisa/adapters/tokenless"
S24B_LINK="$S24B_HOME/.qwen/extensions/tokenless"
mkdir -p "$S24B_ADAPTERS"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/qwencode" "$S24B_ADAPTERS/qwencode"
printf '{"component":"tokenless","version":"0.6.0-anolisa"}\n' > "$S24B_ADAPTERS/manifest.json"
# The pre-existing installation is enabled, so a registration points into the tree
# the npm postinstall is about to replace.
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S24B_HOME" \
    SHELL=/bin/bash \
    REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
    REAL_LN_BIN="$REAL_LN_BIN" \
    REAL_CP_BIN="$REAL_CP_BIN" \
    bash "$S24B_ADAPTERS/qwencode/scripts/install.sh" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "the pre-existing adapter can be enabled" "$RUN_STATUS" "0"
assert_eq "the registration points into the foreign tree" \
  "$(readlink "$S24B_LINK")" "$S24B_ADAPTERS/qwencode"

# The stub postinstall preserves a foreign tree the way the real one does, so the
# restore path only runs when something did replace it — which is what the
# override models.
run_script "$INSTALL_SH" foreign-restore-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_FORCE_ADAPTERS=1" \
  "CP_STUB_FAIL_DEST=$S24B_ADAPTERS"
assert_eq "an install that cannot put a foreign tree back exits non-zero" "$RUN_STATUS" "1"
assert_not_contains "does not claim a successful install" "$RUN_OUTPUT" "installed successfully"
assert_no_file "writes no receipt for the failed run" "$(receipt_of foreign-restore-fails)"
assert_no_file "leaves no launcher link behind" "$S24B_HOME/.local/bin/tokenless"
assert_contains "reports the foreign tree it could not put back" "$RUN_OUTPUT" \
  "the only remaining copy is kept at"
assert_contains "gives the command that puts it back" "$RUN_OUTPUT" \
  "cp -a "
S24B_KEPT="$(kept_copy_of "$RUN_OUTPUT")"
[ -n "$S24B_KEPT" ] || fail "could not read the kept adapter copy path out of the output"
pass "the error names the adapter copy it kept"
assert_file "the foreign adapter snapshot was not deleted" "$S24B_KEPT/manifest.json"
assert_eq "the kept snapshot is the foreign tree" \
  "$(cat "$S24B_KEPT/manifest.json")" '{"component":"tokenless","version":"0.6.0-anolisa"}'
assert_file "the kept snapshot holds the enabled adapter" "$S24B_KEPT/qwencode/scripts/uninstall.sh"

# The failed swap must not leave the shared path empty: the snapshot stays and the
# directory keeps whatever is in it, so the recovery the error message prescribes
# is the only thing that has to work.
assert_file "a failed swap leaves the shared directory in place" "$S24B_ADAPTERS/manifest.json"
# Recovery follows exactly what the error message prescribes.
rm -rf "$S24B_ADAPTERS"
cp -a "$S24B_KEPT" "$S24B_ADAPTERS"
assert_file "the registration resolves again once the snapshot is put back" "$S24B_LINK"
assert_eq "the registration points at the restored tree" \
  "$(readlink "$S24B_LINK")" "$S24B_ADAPTERS/qwencode"

# =============================================================================
# Scenario 25 — a successful run discards its rollback snapshot
# =============================================================================
# The snapshot is transaction scratch space, not an archive. Keeping it on the
# success path leaves a tokenless-rollback.* directory in TMPDIR after every
# install — a full copy of the previous package, scope directory and adapter tree
# on an upgrade — and reports it as somebody's only remaining copy, which after a
# verified install it is not.
run_script "$INSTALL_SH" snapshot-cleanup "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
assert_eq "a first install leaves nothing in TMPDIR" \
  "$(ls -A "$TEST_DIR/snapshot-cleanup/tmp" | wc -l | tr -d ' ')" "0"
assert_not_contains "a first install does not claim to have kept a copy" \
  "$RUN_OUTPUT" "only copy"

run_script "$INSTALL_SH" snapshot-cleanup "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the npm upgrade exits 0" "$RUN_STATUS" "0"
assert_eq "an upgrade leaves nothing in TMPDIR" \
  "$(ls -A "$TEST_DIR/snapshot-cleanup/tmp" | wc -l | tr -d ' ')" "0"
assert_not_contains "an upgrade does not claim to have kept a copy" \
  "$RUN_OUTPUT" "only copy"
assert_eq "the receipt still records the npm method" \
  "$(receipt_value snapshot-cleanup method)" "npm"
assert_file "the upgrade still leaves a working CLI" \
  "$TEST_DIR/snapshot-cleanup/home/.local/bin/tokenless"

# =============================================================================
# Scenario 26 — a same-version npm takeover must keep its own launchers
# =============================================================================
# The curl npm path with prefix/bin as the install directory records a launcher
# whose digest *and* link target describe the package it points into. A direct
# `npm install -g` of the same version into the same prefix recreates both
# byte-identically, so neither check can tell the two apart — only the ownership
# marker inside the module directory can. Deleting the launcher first and
# recognising the takeover afterwards leaves the global package installed with no
# command on PATH.
S26_HOME="$TEST_DIR/npm-takeover/home"
S26_PREFIX="$S26_HOME/.local"
S26_CLI="$S26_PREFIX/bin/tokenless"
S26_PKG="$S26_PREFIX/lib/node_modules/anolisa-tokenless"
run_script "$INSTALL_SH" npm-takeover \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S26_PREFIX" \
  "TOKENLESS_INSTALL_DIR=$S26_PREFIX/bin"
assert_eq "the curl npm install exits 0" "$RUN_STATUS" "0"
assert_file "the launcher is there" "$S26_CLI"
assert_contains "the ownership marker is the installer's" \
  "$(head -n1 "$S26_PKG/.tokenless-owner")" "curl-installer:"
S26_TARGET="$(readlink "$S26_CLI")"

# The takeover: a plain `npm install -g` of the same version into the same prefix.
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S26_HOME" \
    FAKE_VERSION="$FAKE_VERSION" \
    NPM_STUB_PREFIX="$S26_PREFIX" \
    NPM_STUB_SYMLINK_BINS=1 \
    REAL_LN_BIN="$REAL_LN_BIN" \
    REAL_CP_BIN="$REAL_CP_BIN" \
    npm install -g "anolisa-tokenless@$FAKE_VERSION" --prefix "$S26_PREFIX" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "the direct npm reinstall exits 0" "$RUN_STATUS" "0"
printf 'npm:anolisa-tokenless@%s\n' "$FAKE_VERSION" > "$S26_PKG/.tokenless-owner"
assert_eq "the takeover recreated the identical launcher" "$(readlink "$S26_CLI")" "$S26_TARGET"
assert_eq "the recorded digest still matches the taken-over payload" \
  "$(receipt_value npm-takeover file_digest)" "$(sha256sum "$S26_CLI" | cut -d' ' -f1)"

run_script "$UNINSTALL_SH" npm-takeover
assert_eq "uninstall after the takeover exits 0" "$RUN_STATUS" "0"
assert_file "keeps the launcher the newer npm install recreated" "$S26_CLI"
assert_file "keeps the global package it recognised as taken over" "$S26_PKG"
assert_contains "the kept launcher still runs" "$("$S26_CLI" --version 2>&1)" "-npm"
assert_contains "says the launcher belongs to the newer npm install" "$RUN_OUTPUT" \
  "a newer installation owns that package"
assert_contains "still skips the npm package" "$RUN_OUTPUT" "Skipping the npm package in"
assert_no_file "still removes the receipt" "$(receipt_of npm-takeover)"

# =============================================================================
# Scenario 27 — a failed deregistration keeps the resources and the receipt
# =============================================================================
# Deleting the adapter tree after a framework could not be deregistered leaves the
# registration pointing at a path that no longer exists, and takes the very script
# the warning tells the user to re-run.
run_script "$INSTALL_SH" deregister-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless"
assert_eq "install with a bundled adapter payload exits 0" "$RUN_STATUS" "0"
S27_ADAPTERS="$TEST_DIR/deregister-fails/home/.local/share/anolisa/adapters/tokenless"
mkdir -p "$S27_ADAPTERS/brokenfw/scripts"
printf '#!/usr/bin/env bash\necho "brokenfw: framework CLI failed" >&2\nexit 3\n' \
  > "$S27_ADAPTERS/brokenfw/scripts/uninstall.sh"
chmod +x "$S27_ADAPTERS/brokenfw/scripts/uninstall.sh"

run_script "$UNINSTALL_SH" deregister-fails
assert_eq "uninstall exits non-zero when a framework cannot be deregistered" "$RUN_STATUS" "1"
assert_contains "names the framework that failed" "$RUN_OUTPUT" "brokenfw"
assert_contains "says the resources are kept" "$RUN_OUTPUT" "Keeping"
assert_file "keeps the adapter resources for the retry" \
  "$S27_ADAPTERS/brokenfw/scripts/uninstall.sh"
assert_file "keeps the receipt so the retry can find them" "$(receipt_of deregister-fails)"
assert_contains "says the receipt was kept" "$RUN_OUTPUT" "so the uninstall can be retried"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"

# Once the framework is fixed, the same receipt finishes the job.
printf '#!/usr/bin/env bash\nexit 0\n' > "$S27_ADAPTERS/brokenfw/scripts/uninstall.sh"
run_script "$UNINSTALL_SH" deregister-fails
assert_eq "the retry exits 0" "$RUN_STATUS" "0"
assert_no_file "the retry removes the adapter resources" "$S27_ADAPTERS"
assert_no_file "the retry removes the receipt" "$(receipt_of deregister-fails)"
assert_contains "the retry reports a completed uninstall" "$RUN_OUTPUT" "uninstall complete"

# =============================================================================
# Scenario 28 — staging the previous install is fail-closed
# =============================================================================
# The staging copy is the only thing standing between a failed upgrade and a
# machine with no CLI at all, so a run that cannot make it must not start.
run_script "$INSTALL_SH" staging-fails "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the install before the staging failure exits 0" "$RUN_STATUS" "0"
S28_HOME="$TEST_DIR/staging-fails/home"
S28_CLI="$S28_HOME/.local/bin/tokenless"
S28_ID="$(receipt_value staging-fails install_id)"
assert_contains "the installed CLI works" "$("$S28_CLI" --version 2>&1)" "-npm"

# (a) no staging directory can be created at all.
run_script "$INSTALL_SH" staging-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "MKTEMP_STUB_FAIL_PATTERN=tokenless-staged"
assert_eq "a run that cannot create a staging directory exits non-zero" "$RUN_STATUS" "1"
assert_contains "says why it refuses to continue" "$RUN_OUTPUT" \
  "Cannot create a staging directory"
assert_file "the previous CLI is untouched" "$S28_CLI"
assert_contains "the previous CLI still runs" "$("$S28_CLI" --version 2>&1)" "-npm"
assert_eq "the previous receipt is untouched" "$(receipt_value staging-fails install_id)" "$S28_ID"
assert_eq "no rollback snapshot was left behind either" \
  "$(ls -A "$TEST_DIR/staging-fails/tmp" | wc -l | tr -d ' ')" "0"

# (b) a single recorded file cannot be copied aside.
run_script "$INSTALL_SH" staging-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "CP_STUB_FAIL_SRC=$S28_CLI"
assert_eq "a run that cannot stage a recorded file exits non-zero" "$RUN_STATUS" "1"
assert_contains "says which file it could not stage" "$RUN_OUTPUT" "Could not stage ${S28_CLI}"
assert_file "the previous CLI is still in place" "$S28_CLI"
assert_contains "the previous CLI still runs" "$("$S28_CLI" --version 2>&1)" "-npm"
assert_eq "the previous receipt is untouched" "$(receipt_value staging-fails install_id)" "$S28_ID"
assert_eq "nothing was left in TMPDIR" \
  "$(ls -A "$TEST_DIR/staging-fails/tmp" | wc -l | tr -d ' ')" "0"

# And with staging working again the same upgrade succeeds.
run_script "$INSTALL_SH" staging-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "the retry with working staging exits 0" "$RUN_STATUS" "0"
assert_contains "the CLI is the source build now" \
  "$("$S28_CLI" --version 2>&1)" "-src"

# =============================================================================
# Scenario 29 — a framework CLI that refuses to deregister must stop the removal
# =============================================================================
# The adapter scripts swallow the framework CLI's status, because "was not
# registered" and "refused" are indistinguishable by exit code alone. They now
# re-ask the CLI instead, so a registration that survived is a real failure — and
# the top-level uninstaller keeps the resources and the receipt instead of
# deleting a tree that something is still registered against.
run_script "$INSTALL_SH" codex-refuses "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install with a Codex adapter in the tree exits 0" "$RUN_STATUS" "0"
S29_ADAPTERS="$TEST_DIR/codex-refuses/home/.local/share/anolisa/adapters/tokenless"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/codex" "$S29_ADAPTERS/codex"
S29_STATE="$TEST_DIR/codex-refuses/home/.codex-stub"
mkdir -p "$S29_STATE"
: > "$S29_STATE/plugin"
: > "$S29_STATE/marketplace"

run_script "$UNINSTALL_SH" codex-refuses "CODEX_STUB_REMOVE_FAILS=1"
assert_eq "uninstall exits non-zero when the framework CLI refuses" "$RUN_STATUS" "1"
assert_contains "the real Codex script reports the registration survived" "$RUN_OUTPUT" \
  "still lists the tokenless plugin"
assert_contains "the top-level names the framework that failed" "$RUN_OUTPUT" \
  "Could not deregister the codex adapter"
assert_file "keeps the adapter resources for the retry" "$S29_ADAPTERS/codex/scripts/uninstall.sh"
assert_file "keeps the receipt so the retry can find them" "$(receipt_of codex-refuses)"
assert_file "the framework registration is still in place" "$S29_STATE/plugin"

run_script "$UNINSTALL_SH" codex-refuses
assert_eq "the retry with a cooperating CLI exits 0" "$RUN_STATUS" "0"
assert_contains "the retry deregisters the Codex adapter" "$RUN_OUTPUT" "Deregistered the codex adapter"
assert_no_file "the retry removes the adapter resources" "$S29_ADAPTERS"
assert_no_file "the retry removes the receipt" "$(receipt_of codex-refuses)"
assert_no_file "the registration is gone after the retry" "$S29_STATE/plugin"

# =============================================================================
# Scenario 30 — a snapshot that cannot be taken stops the npm route
# =============================================================================
# `npm install -g` replaces the adapter tree and `ln -sf` overwrites the launcher
# paths. A snapshot that failed is not a small gap: rollback would treat the
# partial copy as complete and "restore" an incomplete tree over the real one, or
# find no copy at all and simply delete the new link.
run_script "$INSTALL_SH" owned-snapshot-fails "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
S30_HOME="$TEST_DIR/owned-snapshot-fails/home"
S30_ADAPTERS="$S30_HOME/.local/share/anolisa/adapters/tokenless"
S30_MANIFEST_BEFORE="$(cat "$S30_ADAPTERS/manifest.json")"
S30_OWNER_BEFORE="$(cat "$S30_ADAPTERS/.tokenless-owner")"
run_script "$INSTALL_SH" owned-snapshot-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "CARGO_STUB_FAIL=1" \
  "CP_STUB_FAIL_DEST_BASE=adapters-tokenless"
assert_eq "a run that cannot snapshot the owned adapter tree exits non-zero" "$RUN_STATUS" "1"
assert_contains "says the adapter tree could not be snapshotted" "$RUN_OUTPUT" "Cannot snapshot"
assert_eq "the owned adapter tree is byte-identical" \
  "$(cat "$S30_ADAPTERS/manifest.json")" "$S30_MANIFEST_BEFORE"
assert_eq "the adapter ownership marker is untouched" \
  "$(cat "$S30_ADAPTERS/.tokenless-owner")" "$S30_OWNER_BEFORE"
assert_contains "the previous CLI still runs" \
  "$("$S30_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
assert_eq "the receipt still describes the previous install" \
  "$(receipt_value owned-snapshot-fails method)" "npm"
assert_eq "nothing was left in TMPDIR" \
  "$(ls -A "$TEST_DIR/owned-snapshot-fails/tmp" | wc -l | tr -d ' ')" "0"

# A foreign file parked at a launcher path counts too: the snapshot is what keeps
# it byte-identical, and `ln -sf` would otherwise replace it unconditionally.
run_script "$INSTALL_SH" launcher-snapshot-fails "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the install before the launcher snapshot failure exits 0" "$RUN_STATUS" "0"
S31_HOME="$TEST_DIR/launcher-snapshot-fails/home"
rm -f "$S31_HOME/.local/bin/rtk"
printf '#!/bin/sh\necho "foreign rtk"\n' > "$S31_HOME/.local/bin/rtk"
chmod +x "$S31_HOME/.local/bin/rtk"
S31_FOREIGN_BEFORE="$(cat "$S31_HOME/.local/bin/rtk")"
run_script "$INSTALL_SH" launcher-snapshot-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "CARGO_STUB_FAIL=1" \
  "CP_STUB_FAIL_DEST_BASE=link-rtk"
assert_eq "a run that cannot snapshot a launcher exits non-zero" "$RUN_STATUS" "1"
assert_contains "says which launcher it could not snapshot" "$RUN_OUTPUT" \
  "Cannot snapshot ${S31_HOME}/.local/bin/rtk"
assert_eq "the foreign launcher is byte-identical" \
  "$(cat "$S31_HOME/.local/bin/rtk")" "$S31_FOREIGN_BEFORE"
assert_contains "the previous CLI still runs" \
  "$("$S31_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
assert_eq "nothing was left in TMPDIR" \
  "$(ls -A "$TEST_DIR/launcher-snapshot-fails/tmp" | wc -l | tr -d ' ')" "0"

# =============================================================================
# Scenario 31 — a marker that cannot be written means "not owned"
# =============================================================================
# An empty owner on a schema-3 receipt is a statement, not an omission: without
# the marker nothing distinguishes this install's package from one a newer
# `npm install -g` of the same version put there, so the uninstaller has to refuse
# rather than assume.
if [ "$(id -u)" = "0" ]; then
  echo "SKIP the unwritable-marker scenario cannot be exercised as root"
else
  run_script "$INSTALL_SH" marker-fails "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
  S32_HOME="$TEST_DIR/marker-fails/home"
  S32_PREFIX="$TEST_DIR/marker-fails/npm-prefix"
  S32_PKG="$S32_PREFIX/lib/node_modules/anolisa-tokenless"
  S32_ADAPTERS="$S32_HOME/.local/share/anolisa/adapters/tokenless"
  assert_contains "the first receipt records the package owner" \
    "$(receipt_value marker-fails npm_pkg_owner)" "curl-installer:"

  # Make the marker path unwritable by turning it into a directory.
  rm -f "$S32_PKG/.tokenless-owner"
  mkdir -p "$S32_PKG/.tokenless-owner"
  run_script "$INSTALL_SH" marker-fails "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "the reinstall still succeeds" "$RUN_STATUS" "0"
  assert_contains "says the receipt records no ownership of the package" "$RUN_OUTPUT" \
    "NOT owning that npm package"
  assert_eq "the receipt records an empty package owner" \
    "$(receipt_value marker-fails npm_pkg_owner)" ""

  # A newer direct npm install of the same version takes the package over.
  rmdir "$S32_PKG/.tokenless-owner"
  printf 'npm:anolisa-tokenless@%s\n' "$FAKE_VERSION" > "$S32_PKG/.tokenless-owner"
  # ... and the adapter marker could not be written either.
  sed -i 's|^adapters_dir_owner=.*|adapters_dir_owner=|' "$(receipt_of marker-fails)"

  run_script "$UNINSTALL_SH" marker-fails
  assert_eq "uninstall over unproven ownership exits 0" "$RUN_STATUS" "0"
  assert_file "keeps the npm package it cannot prove it owns" "$S32_PKG"
  assert_file "keeps the adapter tree it cannot prove it owns" "$S32_ADAPTERS/manifest.json"
  assert_file "keeps the launcher that resolves into that package" \
    "$S32_HOME/.local/bin/tokenless"
  assert_contains "the kept launcher still runs" \
    "$("$S32_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
  assert_contains "says the package ownership was never proven" "$RUN_OUTPUT" \
    "records no ownership"
  assert_contains "says the adapter ownership was never proven" "$RUN_OUTPUT" \
    "records no ownership marker for it"
  assert_no_file "still removes the receipt" "$(receipt_of marker-fails)"
fi

# =============================================================================
# Scenario 32 — a foreign tree the postinstall preserved is not rebuilt
# =============================================================================
# The real postinstall leaves a tree it does not own exactly where it is, so the
# installer restoring its snapshot over that directory was a destructive no-op:
# an rm -rf plus a copy of a directory nobody touched, and a copy that failed
# would dangle every registration pointing into it for no reason at all.
run_script "$INSTALL_SH" foreign-preserved "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the install next to a foreign tree exits 0" "$RUN_STATUS" "0"
S32_HOME="$TEST_DIR/foreign-preserved/home"
S32_ADAPTERS="$S32_HOME/.local/share/anolisa/adapters/tokenless"
S32_FOREIGN="$TEST_DIR/foreign-preserved/foreign-tree"
mkdir -p "$S32_FOREIGN/qwencode/scripts"
printf '{"component":"tokenless","version":"0.6.0-anolisa"}\n' > "$S32_FOREIGN/manifest.json"
printf '#!/usr/bin/env bash\nexit 0\n' > "$S32_FOREIGN/qwencode/scripts/uninstall.sh"
rm -rf "$S32_ADAPTERS"
cp -a "$S32_FOREIGN/." "$S32_ADAPTERS/"
S32_INODE_BEFORE="$(ls -di "$S32_ADAPTERS/manifest.json" | awk '{print $1}')"

run_script "$INSTALL_SH" foreign-preserved "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the second install exits 0" "$RUN_STATUS" "0"
assert_eq "the foreign manifest is untouched" \
  "$(cat "$S32_ADAPTERS/manifest.json")" '{"component":"tokenless","version":"0.6.0-anolisa"}'
assert_eq "the foreign tree was not deleted and rebuilt" \
  "$(ls -di "$S32_ADAPTERS/manifest.json" | awk '{print $1}')" "$S32_INODE_BEFORE"
assert_no_file "no marker was written into a tree this run does not own" \
  "$S32_ADAPTERS/.tokenless-owner"
assert_contains "says the resources were left exactly as they were" "$RUN_OUTPUT" \
  "Left the adapter resources in"
assert_eq "the receipt still claims no adapter directory" \
  "$(receipt_value foreign-preserved adapters_dir)" ""

# The restore path still has to work when something *did* replace the tree, and
# still has to fail the run when the copy back cannot be completed.
run_script "$INSTALL_SH" foreign-replaced \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_FORCE_ADAPTERS=1"
S32B_ADAPTERS="$TEST_DIR/foreign-replaced/home/.local/share/anolisa/adapters/tokenless"
rm -rf "$S32B_ADAPTERS"
cp -a "$S32_FOREIGN/." "$S32B_ADAPTERS/"
S32B_INODE_BEFORE="$(ls -di "$S32B_ADAPTERS/manifest.json" | awk '{print $1}')"
run_script "$INSTALL_SH" foreign-replaced \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_FORCE_ADAPTERS=1"
assert_eq "an install that had to put a replaced tree back exits 0" "$RUN_STATUS" "0"
assert_eq "the replaced foreign tree was restored" \
  "$(cat "$S32B_ADAPTERS/manifest.json")" '{"component":"tokenless","version":"0.6.0-anolisa"}'
assert_contains "says it put the resources back" "$RUN_OUTPUT" \
  "Put the adapter resources that were already in"
if [ "$(ls -di "$S32B_ADAPTERS/manifest.json" | awk '{print $1}')" = "$S32B_INODE_BEFORE" ]; then
  fail "the foreign tree was left alone even though the postinstall replaced it"
fi
pass "the restored tree is a fresh copy, not the one the postinstall wrote"

# =============================================================================
# Scenario 33 — real adapter scripts must report a deregistration that failed
# =============================================================================
# Hermes: the registration lives in plugins.enabled inside config.yaml, so the
# filesystem is not evidence about it. A CLI that refuses has to surface.
run_script "$INSTALL_SH" hermes-refuses "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the hermes refusal exits 0" "$RUN_STATUS" "0"
S33_ADAPTERS="$TEST_DIR/hermes-refuses/home/.local/share/anolisa/adapters/tokenless"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/hermes" "$S33_ADAPTERS/hermes"
S33_HOME="$TEST_DIR/hermes-refuses/home/.hermes"
mkdir -p "$S33_HOME/plugins/tokenless"
printf 'plugins:\n  enabled:\n    - tokenless\n' > "$S33_HOME/config.yaml"
# Pin the interpreter this scenario will be judged against. Without a parser the
# production behaviour is "cannot confirm" and the retry also fails closed — which
# is correct, and is what the assertions below expect on that branch.
if [ "$HOST_YAML" = "1" ]; then
  pin_hermes_python "$TEST_DIR/hermes-refuses/home" "$S33_HOME" yaml
else
  pin_hermes_python "$TEST_DIR/hermes-refuses/home" "$S33_HOME" noyaml
fi
printf 'name: tokenless\n' > "$S33_HOME/plugins/tokenless/plugin.yaml"
run_script "$UNINSTALL_SH" hermes-refuses "HERMES_STUB_REFUSE=1"
assert_eq "uninstall exits non-zero when hermes refuses to disable" "$RUN_STATUS" "1"
# The wording differs by branch: with a parser the entry is provably still under
# plugins.enabled, without one the state is "unknown". Both are a refusal, and what
# this scenario is really about is what the refusal leaves behind.
if [ "$HOST_YAML" = "1" ]; then
  assert_contains "the real hermes script reports the surviving registration" "$RUN_OUTPUT" \
    "still lists tokenless under plugins.enabled"
else
  assert_contains "the real hermes script reports it could not determine the state" \
    "$RUN_OUTPUT" "could not be"
fi
assert_file "keeps the plugin files so the retry can work" "$S33_HOME/plugins/tokenless/plugin.yaml"
assert_file "keeps the adapter resources for the retry" "$S33_ADAPTERS/hermes/scripts/uninstall.sh"
assert_file "keeps the receipt so the retry can find them" "$(receipt_of hermes-refuses)"
run_script "$UNINSTALL_SH" hermes-refuses
if [ "$HOST_YAML" = "1" ]; then
  assert_eq "the hermes retry with a cooperating CLI exits 0" "$RUN_STATUS" "0"
  assert_no_file "the retry removes the adapter resources" "$S33_ADAPTERS"
  assert_no_file "the retry removes the receipt" "$(receipt_of hermes-refuses)"
else
  # The cooperating CLI cleared the entry, but without a parser that cannot be
  # read back, so the run has to stop and keep everything rather than assume.
  assert_eq "without a parser the same retry fails closed" "$RUN_STATUS" "1"
  assert_file "and keeps the adapter resources" "$S33_ADAPTERS/hermes/scripts/uninstall.sh"
  assert_file "and keeps the receipt" "$(receipt_of hermes-refuses)"
fi

# Claude Code with no jq: "cannot verify" is not "verified", so the script falls
# back to a literal search for the names it wrote itself.
run_script "$INSTALL_SH" claude-nojq "TOKENLESS_VERSION=$FAKE_VERSION"
S34_ADAPTERS="$TEST_DIR/claude-nojq/home/.local/share/anolisa/adapters/tokenless"
# The npm stub already creates a stub claude-code directory, and `cp -R src dst`
# copies *into* an existing dst — so clear it first or the real script lands one
# level too deep and the deregistration loop never sees it.
rm -rf "$S34_ADAPTERS/claude-code"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/claude-code" "$S34_ADAPTERS/claude-code"
assert_file "the real claude-code adapter is in the tree" \
  "$S34_ADAPTERS/claude-code/scripts/uninstall.sh"
mkdir -p "$TEST_DIR/claude-nojq/home/.claude"
printf '{"enabledPlugins":{"tokenless@anolisa-tokenless":true},"extraKnownMarketplaces":{"anolisa-tokenless":{"x":1}}}\n' \
  > "$TEST_DIR/claude-nojq/home/.claude/settings.json"
RUN_WITHOUT="jq"
run_script "$UNINSTALL_SH" claude-nojq "CLAUDE_STUB_REFUSE=1"
RUN_WITHOUT=""
assert_eq "uninstall exits non-zero when claude refuses and jq is missing" "$RUN_STATUS" "1"
assert_contains "verifies without jq by matching the names it wrote" "$RUN_OUTPUT" \
  "jq unavailable, matched literally"
assert_file "keeps the adapter resources for the retry" \
  "$S34_ADAPTERS/claude-code/scripts/uninstall.sh"
assert_file "keeps the receipt so the retry can find them" "$(receipt_of claude-nojq)"

# Codex: the marketplace directory must outlive a registration that points at it.
run_script "$INSTALL_SH" codex-marketplace "TOKENLESS_VERSION=$FAKE_VERSION"
S35_ADAPTERS="$TEST_DIR/codex-marketplace/home/.local/share/anolisa/adapters/tokenless"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/codex" "$S35_ADAPTERS/codex"
S35_MARKET="$TEST_DIR/codex-marketplace/home/.local/share/anolisa/codex-marketplace/tokenless"
mkdir -p "$S35_MARKET"
printf '{"name":"tokenless"}\n' > "$S35_MARKET/plugin.json"
S35_STATE="$TEST_DIR/codex-marketplace/home/.codex-stub"
mkdir -p "$S35_STATE"
: > "$S35_STATE/plugin"
: > "$S35_STATE/marketplace"
run_script "$UNINSTALL_SH" codex-marketplace "CODEX_STUB_REMOVE_FAILS=1"
assert_eq "uninstall exits non-zero when codex refuses" "$RUN_STATUS" "1"
assert_file "keeps the marketplace directory the surviving registration points at" \
  "$S35_MARKET/plugin.json"
assert_contains "says why the marketplace directory was kept" "$RUN_OUTPUT" \
  "Keeping marketplace directory"
assert_file "keeps the adapter resources for the retry" "$S35_ADAPTERS/codex/scripts/uninstall.sh"

# =============================================================================
# Scenario 36 — no npm on PATH must not cost the receipt
# =============================================================================
# Without npm the global module directory and the prefix's own bin links are all
# still there and Tokenless still runs from them. Printing a manual command and
# then deleting the receipt reported a completed uninstall while leaving the
# installation in place — and with the receipt gone there is no record of the
# prefix or its owner, so a re-run cannot finish the job either.
run_script "$INSTALL_SH" npm-absent "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the install before npm disappears exits 0" "$RUN_STATUS" "0"
S36_NPM="$TEST_DIR/npm-absent/npm-prefix"
assert_file "the global package is installed" "$S36_NPM/lib/node_modules/anolisa-tokenless"
RUN_WITHOUT="npm"
run_script "$UNINSTALL_SH" npm-absent
RUN_WITHOUT=""
assert_eq "uninstall without npm exits non-zero" "$RUN_STATUS" "1"
assert_file "keeps the receipt so the job can be finished" "$(receipt_of npm-absent)"
assert_file "keeps the global package it could not remove" \
  "$S36_NPM/lib/node_modules/anolisa-tokenless"
assert_contains "says the receipt was kept for a retry" "$RUN_OUTPUT" \
  "so the uninstall can be retried"
assert_contains "gives the command that finishes the job" "$RUN_OUTPUT" \
  "npm uninstall -g anolisa-tokenless --prefix"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"
run_script "$UNINSTALL_SH" npm-absent
assert_eq "the retry with npm back exits 0" "$RUN_STATUS" "0"
assert_no_file "the retry removes the global package" \
  "$S36_NPM/lib/node_modules/anolisa-tokenless"
assert_no_file "the retry removes the receipt" "$(receipt_of npm-absent)"

# =============================================================================
# Scenario 37 — the source build checks its own temporaries
# =============================================================================
# try_source_build runs as an `if`/`elif` condition, where Bash disables errexit
# for the whole body, so an unchecked mktemp carries an empty path into every
# download and extraction path built from it, and an unchecked tar builds on top
# of a partially extracted tree.
run_script "$INSTALL_SH" src-tmpdir-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "MKTEMP_STUB_FAIL_PATTERN=tokenless-src"
assert_eq "a source build with no build directory exits non-zero" "$RUN_STATUS" "1"
assert_contains "says it cannot create a build directory" "$RUN_OUTPUT" \
  "Cannot create a build directory"
assert_eq "never reaches cargo" "$(cargo_log src-tmpdir-fails)" ""
assert_no_file "wrote no CLI" "$TEST_DIR/src-tmpdir-fails/home/.local/bin/tokenless"
assert_no_file "wrote no receipt" "$(receipt_of src-tmpdir-fails)"

run_script "$INSTALL_SH" src-tar-fails \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "TAR_STUB_FAIL=1"
assert_eq "a source build whose archive will not extract exits non-zero" "$RUN_STATUS" "1"
assert_contains "says the archive could not be extracted" "$RUN_OUTPUT" "Failed to extract"
assert_eq "never reaches cargo" "$(cargo_log src-tar-fails)" ""
assert_no_file "wrote no CLI" "$TEST_DIR/src-tar-fails/home/.local/bin/tokenless"
assert_eq "cleaned the build directory up" \
  "$(ls -A "$TEST_DIR/src-tar-fails/tmp" | wc -l | tr -d ' ')" "0"

# =============================================================================
# Scenario 38 — moving the install directory retires the old PATH entry
# =============================================================================
# The receipt names one rc file and one install directory. Installing into a
# second directory used to append a second marker block while the new receipt only
# recorded the new one, so the entry for the first directory survived every
# uninstall forever.
S38_RC="$TEST_DIR/path-move/home/.bashrc"
run_script "$INSTALL_SH" path-move \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_INSTALL_DIR=$TEST_DIR/path-move/home/.local/bin"
assert_eq "the first install exits 0" "$RUN_STATUS" "0"
assert_eq "the first rc file carries one installer marker" \
  "$(grep -cF '# Added by tokenless installer' "$S38_RC")" "1"
run_script "$INSTALL_SH" path-move \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_INSTALL_DIR=$TEST_DIR/path-move/home/other/bin"
assert_eq "the install into the second directory exits 0" "$RUN_STATUS" "0"
assert_eq "exactly one installer marker survives the move" \
  "$(grep -cF '# Added by tokenless installer' "$S38_RC")" "1"
assert_contains "the surviving entry is the new directory" "$(cat "$S38_RC")" "other/bin"
assert_not_contains "the stale entry for the first directory is gone" "$(cat "$S38_RC")" ".local/bin"
assert_contains "says it removed the stale entry" "$RUN_OUTPUT" "Removed the stale PATH entry for"
run_script "$UNINSTALL_SH" path-move
assert_eq "uninstall after the move exits 0" "$RUN_STATUS" "0"
assert_not_contains "uninstall leaves no installer PATH entry behind" \
  "$(cat "$S38_RC")" "tokenless installer"

# =============================================================================
# Scenario 39 — npm owns its dependency layout; a standalone sibling is not ours
# =============================================================================
# Verified against real npm (10.9.4, packed tarballs): a global install nests the
# @anolisa platform dependency under the root package's own node_modules, and
# `npm uninstall -g` reports "removed 2 packages" — root and nested payload — while
# leaving a separately installed global package of the same name alone. So removing
# the root module directory is sufficient, and a sibling @anolisa scope is never
# this installer's to delete. Deleting it because the name matched this machine's
# platform key destroyed a user's unrelated global install and still returned 0.
platform_layout_case() {
  local uname_arch="$1" plat="$2" sc="$3"
  local home="$TEST_DIR/$sc/home"
  local prefix="$home/.local"
  local root="$prefix/lib/node_modules/anolisa-tokenless"
  local nested="$root/node_modules/@anolisa/tokenless-$plat"
  local standalone="$prefix/lib/node_modules/@anolisa/tokenless-$plat"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix" "TOKENLESS_INSTALL_DIR=$home/.local/bin"
  assert_eq "[$plat] the npm install exits 0" "$RUN_STATUS" "0"
  assert_file "[$plat] the native payload is nested under the root package" \
    "$nested/bin/tokenless"
  assert_no_file "[$plat] real npm creates no sibling scope" \
    "$prefix/lib/node_modules/@anolisa"
  # A standalone global install of the platform package — the only thing a sibling
  # @anolisa scope can actually be. Nothing this installer did put it there.
  mkdir -p "$standalone/bin"
  printf 'standalone-global-install\n' > "$standalone/bin/tokenless"
  assert_contains "[$plat] the installed CLI works" \
    "$("$home/.local/bin/tokenless" --version 2>&1)" "-npm"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix" "TOKENLESS_INSTALL_DIR=$home/.local/bin" \
    "TOKENLESS_FORCE_BUILD=1"
  assert_eq "[$plat] retiring it by switching to a source build exits 0" "$RUN_STATUS" "0"
  assert_no_file "[$plat] the root package is gone" "$root"
  assert_no_file "[$plat] the nested payload went with it" "$nested"
  assert_file "[$plat] the standalone global platform package SURVIVES" \
    "$standalone/bin/tokenless"
  assert_eq "[$plat] and is byte-for-byte untouched" \
    "$(cat "$standalone/bin/tokenless")" "standalone-global-install"
  assert_contains "[$plat] the source-built CLI runs" \
    "$("$home/.local/bin/tokenless" --version 2>&1)" "-src"
}
platform_layout_case x86_64 linux-x64 platform-layout-x64
platform_layout_case aarch64 linux-arm64 platform-layout-arm64

# A rolled-back first attempt and a failed upgrade must treat a sibling the same
# way, and the nested payload has to come back inside the restored module dir.
platform_rollback_case() {
  local uname_arch="$1" plat="$2" sc="$3"
  local prefix="$TEST_DIR/$sc/npm-prefix"
  local standalone="$prefix/lib/node_modules/@anolisa/tokenless-$plat"
  mkdir -p "$standalone/bin"
  printf 'standalone-global-install\n' > "$standalone/bin/tokenless"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix" "LN_STUB_FAIL=1"
  assert_eq "[$plat] a failed first attempt reports incomplete, no method switch" \
    "$RUN_STATUS" "1"
  assert_no_file "[$plat] the rolled-back attempt leaves no root package behind" \
    "$prefix/lib/node_modules/anolisa-tokenless"
  assert_file "[$plat] a rollback leaves the standalone sibling alone" \
    "$standalone/bin/tokenless"
}
platform_rollback_case x86_64 linux-x64 platform-rollback-x64
platform_rollback_case aarch64 linux-arm64 platform-rollback-arm64

platform_upgrade_case() {
  local uname_arch="$1" plat="$2" sc="$3"
  local home="$TEST_DIR/$sc/home"
  local prefix="$TEST_DIR/$sc/npm-prefix"
  local nested="$prefix/lib/node_modules/anolisa-tokenless/node_modules/@anolisa/tokenless-$plat"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix"
  assert_eq "[$plat] the first npm install exits 0" "$RUN_STATUS" "0"
  assert_file "[$plat] the nested payload is in place" "$nested/bin/tokenless"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix" "NPM_STUB_BROKEN_BIN=1" "CARGO_STUB_FAIL=1"
  assert_eq "[$plat] a broken upgrade whose fallback fails exits non-zero" "$RUN_STATUS" "1"
  assert_file "[$plat] the rollback restored the root package" \
    "$prefix/lib/node_modules/anolisa-tokenless"
  assert_file "[$plat] and the nested payload came back inside it" "$nested/bin/tokenless"
  assert_contains "[$plat] the previous CLI still runs" \
    "$("$home/.local/bin/tokenless" --version 2>&1)" "-npm"
}
platform_upgrade_case x86_64 linux-x64 platform-upgrade-x64
platform_upgrade_case aarch64 linux-arm64 platform-upgrade-arm64

# =============================================================================
# Scenario 40 — a probe that cannot answer is not an answer of "nothing there"
# =============================================================================
# Every adapter script verifies its deregistration, but a verification that cannot
# run used to read as success: a CLI whose query fails, a settings file that does
# not exist, a config that does not parse. The caller then deletes the adapter
# resources and the receipt, leaving a registration nobody can see any more.
run_script "$INSTALL_SH" probe-fails "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the probe failures exits 0" "$RUN_STATUS" "0"
S40_ADAPTERS="$TEST_DIR/probe-fails/home/.local/share/anolisa/adapters/tokenless"
for fw in codex claude-code hermes; do
  rm -rf "${S40_ADAPTERS:?}/$fw"
  cp -R "$TOKENLESS_ROOT/adapters/tokenless/$fw" "$S40_ADAPTERS/$fw"
done

# (a) codex `plugin list` fails outright.
S40_STATE="$TEST_DIR/probe-fails/home/.codex-stub"
mkdir -p "$S40_STATE"
: > "$S40_STATE/plugin"
run_script "$UNINSTALL_SH" probe-fails "CODEX_STUB_LIST_FAILS=1"
assert_eq "uninstall exits non-zero when the codex query fails" "$RUN_STATUS" "1"
assert_contains "says the registration cannot be confirmed" "$RUN_OUTPUT"   "cannot be confirmed whether the plugin is registered"
assert_file "keeps the adapter resources" "$S40_ADAPTERS/codex/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of probe-fails)"

# (b) claude CLI is unusable and there is no settings.json to check against.
run_script "$UNINSTALL_SH" probe-fails "CLAUDE_BIN=/bin/false"
assert_eq "uninstall exits non-zero when claude cannot answer and settings are absent" "$RUN_STATUS" "1"
assert_contains "says nothing confirms the claude removal" "$RUN_OUTPUT"   "nothing confirms the plugin was removed"
assert_file "keeps the adapter resources" "$S40_ADAPTERS/claude-code/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of probe-fails)"

# (c) settings.json exists but does not parse.
mkdir -p "$TEST_DIR/probe-fails/home/.claude"
printf '{"enabledPlugins": this is not json
' > "$TEST_DIR/probe-fails/home/.claude/settings.json"
run_script "$UNINSTALL_SH" probe-fails
assert_eq "uninstall exits non-zero when settings.json cannot be parsed" "$RUN_STATUS" "1"
assert_contains "says the settings could not be parsed" "$RUN_OUTPUT" "could not be parsed"
assert_file "keeps the receipt for the retry" "$(receipt_of probe-fails)"
rm -f "$TEST_DIR/probe-fails/home/.claude/settings.json"

# (d) hermes CLI is unusable and there is no config.yaml to check against.
run_script "$UNINSTALL_SH" probe-fails "HERMES_BIN=/bin/false"
assert_eq "uninstall exits non-zero when hermes cannot answer and config is absent" "$RUN_STATUS" "1"
assert_contains "says nothing confirms the hermes removal" "$RUN_OUTPUT"   "nothing confirms the plugin was disabled"
assert_file "keeps the receipt for the retry" "$(receipt_of probe-fails)"

# With every framework able to answer, the same receipt finishes the job.
rm -rf "${S40_STATE:?}"
run_script "$UNINSTALL_SH" probe-fails
assert_eq "the retry with working CLIs exits 0" "$RUN_STATUS" "0"
assert_no_file "the retry removes the adapter resources" "$S40_ADAPTERS"
assert_no_file "the retry removes the receipt" "$(receipt_of probe-fails)"

# =============================================================================
# Scenario 41 — no npm, overlapping prefix: the launchers must survive
# =============================================================================
# With `--prefix ~/.local` and `~/.local/bin` as the install directory, the
# recorded launchers *are* the prefix's own bin links. Deleting them in step 1 and
# only then finding out that npm is gone removed the working command while keeping
# the package that needs it — and still claimed every launcher was in place.
S41_HOME="$TEST_DIR/npm-absent-overlap/home"
run_script "$INSTALL_SH" npm-absent-overlap \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S41_HOME/.local" \
  "TOKENLESS_INSTALL_DIR=$S41_HOME/.local/bin"
assert_eq "the overlapping-prefix install exits 0" "$RUN_STATUS" "0"
assert_contains "the installed CLI works" "$("$S41_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
RUN_WITHOUT="npm"
run_script "$UNINSTALL_SH" npm-absent-overlap
RUN_WITHOUT=""
assert_eq "uninstall without npm exits non-zero" "$RUN_STATUS" "1"
assert_file "keeps the launcher that is also the prefix bin link" "$S41_HOME/.local/bin/tokenless"
assert_file "keeps the rtk launcher too" "$S41_HOME/.local/bin/rtk"
assert_contains "the kept CLI still runs" "$("$S41_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
assert_file "keeps the global package" "$S41_HOME/.local/lib/node_modules/anolisa-tokenless"
assert_file "keeps the receipt so the job can be finished" "$(receipt_of npm-absent-overlap)"
assert_contains "says the launcher links are still in place" "$RUN_OUTPUT" \
  "launcher links are still in place"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"
run_script "$UNINSTALL_SH" npm-absent-overlap
assert_eq "the retry with npm back exits 0" "$RUN_STATUS" "0"
assert_no_file "the retry removes the launcher" "$S41_HOME/.local/bin/tokenless"
assert_no_file "the retry removes the receipt" "$(receipt_of npm-absent-overlap)"

# =============================================================================
# Scenario 42 — an incomplete rollback must stop the run, not fall back to cargo
# =============================================================================
# A broken npm upgrade rolls the previous install back. When one of those copies
# back fails — a full disk, a permission change — the old launcher survives only
# inside the rollback snapshot, and the adapter tree the existing framework
# registrations point at is in the same state. Cargo can still succeed there, and
# letting it would report a working install on a machine that is half restored.
S42_HOME="$TEST_DIR/rollback-incomplete/home"
run_script "$INSTALL_SH" rollback-incomplete "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
assert_contains "the installed CLI works" \
  "$("$S42_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
# Break the new payload so verify_cli trips after the links were written, and make
# the copy-back fail while leaving cargo able to build.
run_script "$INSTALL_SH" rollback-incomplete \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_BROKEN_BIN=1" "CP_STUB_FAIL_DEST_BASE=tokenless"
assert_eq "an incomplete rollback exits non-zero" "$RUN_STATUS" "1"
assert_contains "says the rollback was not complete" "$RUN_OUTPUT" "NOT fully rolled back"
# Wrapped across two lines in the script's own output, so match within one line.
assert_contains "says the source-build fallback is refused" "$RUN_OUTPUT" \
  "fallback is refused"
assert_not_contains "does not report a successful install" "$RUN_OUTPUT" "Installed to"
# The snapshot holding what could not be copied back must survive the run, or the
# refusal would leave the user with nothing to recover from.
S42_ROLLBACK="$(printf '%s\n' "$RUN_OUTPUT" \
  | sed -n 's/.*could not be copied back is kept at: \(.*\)$/\1/p' | head -1)"
if [ -z "$S42_ROLLBACK" ]; then
  fail "the refusal did not name the snapshot holding the unrestored copy"
fi
pass "names the snapshot holding the unrestored copy"
assert_file "that rollback snapshot is still on disk" "$S42_ROLLBACK"
# The point of the refusal: cargo is never reached, so nothing can report a
# successful install over the half-restored state.
assert_not_contains "never starts the source build" "$RUN_OUTPUT" "Building from source"
# And the previous receipt survives, so the machine is still describable.
assert_file "keeps the previous install receipt" "$(receipt_of rollback-incomplete)"

# =============================================================================
# Scenario 43 — npm present but refusing: the launchers must survive
# =============================================================================
# `npm uninstall` can come back non-zero with npm perfectly reachable — permissions,
# a corrupt store, a failing lifecycle script. The recorded launchers were deleted
# before that command ran, so errexit stopped the run with the global package and
# the receipt still installed and no command left on PATH. Both layouts are
# covered: a separate install directory whose launchers are symlinks resolving
# into the prefix, and a prefix whose bin directory *is* the install directory.
npm_refusal_case() {
  local sc="$1" prefix="$2" installdir="$3"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" "NPM_STUB_SYMLINK_BINS=1" \
    "NPM_STUB_PREFIX=$prefix" "TOKENLESS_INSTALL_DIR=$installdir"
  assert_eq "[$sc] the npm install exits 0" "$RUN_STATUS" "0"
  assert_contains "[$sc] the installed CLI works" \
    "$("$installdir/tokenless" --version 2>&1)" "-npm"
  run_script "$UNINSTALL_SH" "$sc" \
    "NPM_STUB_UNINSTALL_FAIL=1" "NPM_STUB_PREFIX=$prefix"
  assert_eq "[$sc] uninstall exits non-zero when npm refuses" "$RUN_STATUS" "1"
  assert_contains "[$sc] says npm could not remove the package" "$RUN_OUTPUT" \
    "npm could not remove"
  assert_file "[$sc] keeps the tokenless launcher" "$installdir/tokenless"
  assert_file "[$sc] keeps the rtk launcher" "$installdir/rtk"
  assert_contains "[$sc] the kept launcher still runs" \
    "$("$installdir/tokenless" --version 2>&1)" "-npm"
  assert_file "[$sc] keeps the global package" "$prefix/lib/node_modules/anolisa-tokenless"
  assert_file "[$sc] keeps the receipt for the retry" "$(receipt_of "$sc")"
  assert_not_contains "[$sc] does not report a completed uninstall" "$RUN_OUTPUT" \
    "uninstall complete"
  run_script "$UNINSTALL_SH" "$sc" "NPM_STUB_PREFIX=$prefix"
  assert_eq "[$sc] the retry with npm cooperating exits 0" "$RUN_STATUS" "0"
  assert_no_file "[$sc] the retry removes the launcher" "$installdir/tokenless"
  assert_no_file "[$sc] the retry removes the package" \
    "$prefix/lib/node_modules/anolisa-tokenless"
  assert_no_file "[$sc] the retry removes the receipt" "$(receipt_of "$sc")"
}
S43_HOME="$TEST_DIR/npm-refuses-separate/home"
npm_refusal_case npm-refuses-separate "$S43_HOME/.npm-global" "$S43_HOME/.local/bin"
S43B_HOME="$TEST_DIR/npm-refuses-overlap/home"
npm_refusal_case npm-refuses-overlap "$S43B_HOME/.local" "$S43B_HOME/.local/bin"

# =============================================================================
# Scenario 44 — a receipt uninstall takes the nested payload, never a sibling
# =============================================================================
# The receipt-driven uninstall removes the root package, and with it the nested
# platform dependency: npm's own removal takes both, and the manual fallback
# removes the root module directory, which carries the dependency inside it. What
# it must never do is delete a sibling @anolisa scope, because that can only be a
# standalone global install this receipt knows nothing about. There is deliberately
# no npm_platform_pkg field in the receipt — recording one is what made deleting a
# path nobody proved we own look like bookkeeping.
receipt_uninstall_case() {
  local uname_arch="$1" plat="$2" sc="$3"
  local home="$TEST_DIR/$sc/home"
  local prefix="$home/.local"
  local root="$prefix/lib/node_modules/anolisa-tokenless"
  local nested="$root/node_modules/@anolisa/tokenless-$plat"
  local standalone="$prefix/lib/node_modules/@anolisa/tokenless-$plat"
  run_script "$INSTALL_SH" "$sc" \
    "TOKENLESS_VERSION=$FAKE_VERSION" \
    "UNAME_STUB_OS=Linux" "UNAME_STUB_ARCH=$uname_arch" \
    "NPM_STUB_SYMLINK_BINS=1" "NPM_STUB_PLATFORM_PKG=1" "NPM_STUB_PLATFORM_KEY=$plat" \
    "NPM_STUB_PREFIX=$prefix" "TOKENLESS_INSTALL_DIR=$home/.local/bin"
  assert_eq "[$plat] the npm install exits 0" "$RUN_STATUS" "0"
  assert_eq "[$plat] the receipt records no platform-package field" \
    "$(receipt_value "$sc" npm_platform_pkg)" ""
  mkdir -p "$standalone/bin"
  printf 'standalone-global-install\n' > "$standalone/bin/tokenless"
  run_script "$UNINSTALL_SH" "$sc" "NPM_STUB_PREFIX=$prefix"
  assert_eq "[$plat] the receipt uninstall exits 0" "$RUN_STATUS" "0"
  assert_no_file "[$plat] the root package is gone" "$root"
  assert_no_file "[$plat] the nested payload went with it" "$nested"
  assert_file "[$plat] the standalone global platform package SURVIVES" \
    "$standalone/bin/tokenless"
  assert_eq "[$plat] and is byte-for-byte untouched" \
    "$(cat "$standalone/bin/tokenless")" "standalone-global-install"
  assert_no_file "[$plat] the launcher is gone" "$home/.local/bin/tokenless"
  assert_no_file "[$plat] the receipt is gone" "$(receipt_of "$sc")"
}
receipt_uninstall_case x86_64 linux-x64 receipt-uninstall-x64
receipt_uninstall_case aarch64 linux-arm64 receipt-uninstall-arm64

# =============================================================================
# Scenario 45 — a CLI that is missing is not proof the registration is gone
# =============================================================================
# codex, hermes and qoder keep their registration in the framework's own store, so
# the CLI is the only witness to it. It being unresolvable right now — a different
# shell, a PATH that has not been loaded — says nothing about that store, and the
# caller deletes the shared adapter resources and the receipt the moment the script
# returns 0. A registration that outlives them points at a path nobody can restore,
# and the script that would have retried it is gone too.
run_script "$INSTALL_SH" cli-missing "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the missing-CLI cases exits 0" "$RUN_STATUS" "0"
S45_ADAPTERS="$TEST_DIR/cli-missing/home/.local/share/anolisa/adapters/tokenless"
S45_HOME="$TEST_DIR/cli-missing/home"
for fw in codex hermes qoder; do
  rm -rf "${S45_ADAPTERS:?}/$fw"
  cp -R "$TOKENLESS_ROOT/adapters/tokenless/$fw" "$S45_ADAPTERS/$fw"
done

# The CLIs are pinned to a path that does not exist rather than hidden from PATH:
# resolve_codex also probes absolute system paths such as /usr/bin/codex, so PATH
# hiding alone would make this scenario depend on what the host happens to have.
S45_ABSENT="$S45_HOME/.no-such-cli"

# (a) codex is unusable and its own config still names the plugin.
mkdir -p "$S45_HOME/.codex"
printf '[plugins]\ntokenless = { marketplace = "anolisa-tokenless" }\n' \
  > "$S45_HOME/.codex/config.toml"
run_script "$UNINSTALL_SH" cli-missing "CODEX_BIN=$S45_ABSENT"
assert_eq "a missing codex CLI with a live config fails the uninstall" "$RUN_STATUS" "1"
assert_contains "says the codex registration cannot be confirmed gone" "$RUN_OUTPUT" \
  "codex CLI not found and"
assert_file "keeps the codex adapter resources" "$S45_ADAPTERS/codex/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of cli-missing)"
rm -f "$S45_HOME/.codex/config.toml"

# (b) hermes is unusable and config.yaml still enables the plugin.
mkdir -p "$S45_HOME/.hermes"
printf 'plugins:\n  enabled:\n    - tokenless\n' > "$S45_HOME/.hermes/config.yaml"
run_script "$UNINSTALL_SH" cli-missing "HERMES_BIN=$S45_ABSENT"
assert_eq "a missing hermes CLI with a live config fails the uninstall" "$RUN_STATUS" "1"
assert_contains "says the hermes registration cannot be confirmed removed" "$RUN_OUTPUT" \
  "hermes CLI not found and"
assert_file "keeps the hermes plugin files for the retry" \
  "$S45_ADAPTERS/hermes/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of cli-missing)"
rm -f "$S45_HOME/.hermes/config.yaml"

# (c) qodercli is absent and Qoder's own store still lists the plugin.
mkdir -p "$S45_HOME/.qoder/plugins"
printf '{"plugins":[{"id":"tokenless@local","scope":"user"}]}\n' \
  > "$S45_HOME/.qoder/plugins/registry.json"
run_script "$UNINSTALL_SH" cli-missing \
  "CODEX_BIN=$S45_ABSENT" "HERMES_BIN=$S45_ABSENT"
assert_eq "a missing qodercli with a live registration fails the uninstall" "$RUN_STATUS" "1"
assert_contains "says the qoder registration cannot be confirmed removed" "$RUN_OUTPUT" \
  "qodercli not found and a tokenless@local registration"
assert_file "keeps the qoder adapter resources" "$S45_ADAPTERS/qoder/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of cli-missing)"
rm -rf "$S45_HOME/.qoder"

# (d) With no registration left anywhere, the same missing CLIs are a clean state:
#     a framework that was never installed here registered nothing to remove.
run_script "$UNINSTALL_SH" cli-missing \
  "CODEX_BIN=$S45_ABSENT" "HERMES_BIN=$S45_ABSENT"
assert_eq "missing CLIs with nothing registered exit 0" "$RUN_STATUS" "0"
assert_no_file "the adapter resources are removed" "$S45_ADAPTERS"
assert_no_file "the receipt is removed" "$(receipt_of cli-missing)"

# =============================================================================
# Scenario 46 — a taken-over launcher under the prefix blocks `npm uninstall`
# =============================================================================
# The ownership check only ever constrained this script's own rm. With the install
# directory inside the npm prefix, `npm uninstall -g --prefix` deletes the prefix's
# bin entries by itself, so the script could print "another installation has taken
# over that path" and then hand that very path to npm — which removed the foreign
# file and still reported a successful uninstall. The verdict has to gate the
# delegated removal as well as the direct one.
S46_HOME="$TEST_DIR/prefix-takeover/home"
S46_PREFIX="$S46_HOME/.local"
run_script "$INSTALL_SH" prefix-takeover \
  "TOKENLESS_VERSION=$FAKE_VERSION" "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S46_PREFIX" "TOKENLESS_INSTALL_DIR=$S46_HOME/.local/bin"
assert_eq "the overlapping-prefix install exits 0" "$RUN_STATUS" "0"
assert_contains "the installed CLI works" \
  "$("$S46_HOME/.local/bin/tokenless" --version 2>&1)" "-npm"
# The user replaces that launcher with their own executable. The package directory
# still carries this receipt's owner marker, so step 2 would otherwise proceed.
rm -f "$S46_HOME/.local/bin/tokenless"
printf '#!/bin/sh\necho foreign-executable\n' > "$S46_HOME/.local/bin/tokenless"
chmod +x "$S46_HOME/.local/bin/tokenless"
run_script "$UNINSTALL_SH" prefix-takeover "NPM_STUB_PREFIX=$S46_PREFIX"
assert_eq "uninstall exits non-zero rather than deleting the foreign file" \
  "$RUN_STATUS" "1"
assert_contains "says another installation took that path over" "$RUN_OUTPUT" \
  "another installation has taken over that path"
assert_contains "says npm uninstall was not run" "$RUN_OUTPUT" "Not running npm uninstall"
assert_file "the foreign executable is still there" "$S46_HOME/.local/bin/tokenless"
assert_eq "and still runs as the file the user put there" \
  "$("$S46_HOME/.local/bin/tokenless" 2>&1)" "foreign-executable"
assert_file "the global package is kept" \
  "$S46_PREFIX/lib/node_modules/anolisa-tokenless"
assert_file "the receipt is kept for a retry" "$(receipt_of prefix-takeover)"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" \
  "uninstall complete"

# =============================================================================
# Scenario 47 — "cannot confirm" is a third state, not a success
# =============================================================================
# Two production paths reported a clean uninstall while a registration was still
# live. hermes: a legal flow-sequence config (`enabled: [tokenless]`) that the
# shape-specific regex did not match, so a persisted entry read as "absent".
# claude-code: no CLI and no jq, where the entire manual cleanup block was guarded
# on jq — it changed nothing and still printed "manual cleanup complete".
run_script "$INSTALL_SH" unconfirmed "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the unconfirmed cases exits 0" "$RUN_STATUS" "0"
S47_ADAPTERS="$TEST_DIR/unconfirmed/home/.local/share/anolisa/adapters/tokenless"
S47_HOME="$TEST_DIR/unconfirmed/home"
for fw in hermes claude-code; do
  rm -rf "${S47_ADAPTERS:?}/$fw"
  cp -R "$TOKENLESS_ROOT/adapters/tokenless/$fw" "$S47_ADAPTERS/$fw"
done
S47_ABSENT="$S47_HOME/.no-such-cli"

# (a) hermes: legal flow-sequence YAML, CLI unusable.
mkdir -p "$S47_HOME/.hermes"
printf 'plugins:\n  enabled: [tokenless]\n' > "$S47_HOME/.hermes/config.yaml"
run_script "$UNINSTALL_SH" unconfirmed \
  "HERMES_BIN=$S47_ABSENT" "CLAUDE_BIN=$S47_ABSENT"
assert_eq "a flow-sequence registration cannot be confirmed away" "$RUN_STATUS" "1"
# Wording differs between "provably still enabled" and "cannot be parsed", but
# both are a refusal, so assert the part that holds on either path.
assert_contains "says the registration cannot be confirmed" "$RUN_OUTPUT" \
  "cannot be confirmed"
assert_file "keeps the hermes adapter resources" \
  "$S47_ADAPTERS/hermes/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of unconfirmed)"
rm -f "$S47_HOME/.hermes/config.yaml"

# (b) claude-code: standard settings, no CLI and no jq anywhere on PATH.
mkdir -p "$S47_HOME/.claude"
printf '{"enabledPlugins":{"tokenless@anolisa-tokenless":true}}\n' \
  > "$S47_HOME/.claude/settings.json"
RUN_WITHOUT="jq"
run_script "$UNINSTALL_SH" unconfirmed \
  "HERMES_BIN=$S47_ABSENT" "CLAUDE_BIN=$S47_ABSENT"
RUN_WITHOUT=""
assert_eq "no CLI and no jq is not a clean removal" "$RUN_STATUS" "1"
assert_contains "says jq is unavailable so it cannot be removed or verified" \
  "$RUN_OUTPUT" "jq is unavailable"
assert_file "keeps the claude-code adapter resources" \
  "$S47_ADAPTERS/claude-code/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of unconfirmed)"
assert_eq "settings.json was left exactly as it was" \
  "$(cat "$S47_HOME/.claude/settings.json")" \
  '{"enabledPlugins":{"tokenless@anolisa-tokenless":true}}'

# (c) With nothing registered anywhere, the same unusable CLIs are a clean state:
#     failing here would make Tokenless uninstallable on a machine that simply
#     never had those frameworks.
rm -f "$S47_HOME/.claude/settings.json"
run_script "$UNINSTALL_SH" unconfirmed \
  "HERMES_BIN=$S47_ABSENT" "CLAUDE_BIN=$S47_ABSENT"
assert_eq "missing CLIs with nothing registered exit 0" "$RUN_STATUS" "0"
assert_no_file "the adapter resources are removed" "$S47_ADAPTERS"
assert_no_file "the receipt is removed" "$(receipt_of unconfirmed)"

# =============================================================================
# Scenario 48 — a package a newer install owns keeps its launchers with it
# =============================================================================
# A direct `npm install -g` of the same version reproduces the payload and the link
# targets byte for byte, so the only evidence of the takeover is the owner marker it
# replaced. Staging compared digest and link target alone and therefore moved those
# launchers aside — and commit deleted them — while retirement read the marker and
# correctly kept the package and the adapter tree. The result was a working package
# with no `rtk` on PATH, reported as a successful install.
S48_HOME="$TEST_DIR/staging-takeover/home"
S48_PREFIX="$S48_HOME/.npm-global"
run_script "$INSTALL_SH" staging-takeover \
  "TOKENLESS_VERSION=$FAKE_VERSION" "NPM_STUB_SYMLINK_BINS=1" \
  "NPM_STUB_PREFIX=$S48_PREFIX" "TOKENLESS_INSTALL_DIR=$S48_HOME/.local/bin"
assert_eq "the npm install exits 0" "$RUN_STATUS" "0"
assert_file "the rtk launcher is installed" "$S48_HOME/.local/bin/rtk"
assert_file "and the package carries this run's owner marker" \
  "$S48_PREFIX/lib/node_modules/anolisa-tokenless/.tokenless-owner"
S48_OWNER="$(head -n1 "$S48_PREFIX/lib/node_modules/anolisa-tokenless/.tokenless-owner")"
# The takeover: same version, so identical bytes and identical link targets. Only
# the marker changes.
printf 'npm:somebody-else\n' \
  > "$S48_PREFIX/lib/node_modules/anolisa-tokenless/.tokenless-owner"
# Re-run as a source build so the old prefix is not reused and the retirement path
# has to decide what still belongs to the previous receipt.
run_script "$INSTALL_SH" staging-takeover \
  "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1" \
  "NPM_STUB_PREFIX=$S48_PREFIX" "TOKENLESS_INSTALL_DIR=$S48_HOME/.local/bin"
assert_eq "the source-build re-run exits 0" "$RUN_STATUS" "0"
assert_contains "says the package belongs to a newer installation" "$RUN_OUTPUT" \
  "belongs to a newer installation"
assert_file "the newer installation's package is kept" \
  "$S48_PREFIX/lib/node_modules/anolisa-tokenless"
assert_eq "and its owner marker was not touched" \
  "$(head -n1 "$S48_PREFIX/lib/node_modules/anolisa-tokenless/.tokenless-owner")" \
  "npm:somebody-else"
assert_file "its rtk launcher survived both staging and retirement" \
  "$S48_HOME/.local/bin/rtk"
assert_contains "and still runs the kept package's payload" \
  "$("$S48_HOME/.local/bin/rtk" --version 2>&1)" "-npm"
assert_contains "the new source-built CLI took its own path" \
  "$("$S48_HOME/.local/bin/tokenless" --version 2>&1)" "-src"
assert_ne() { [ "$2" != "$3" ] || fail "$1: '$2' should differ from '$3'"; pass "$1"; }
assert_ne "the takeover really did change the marker" "$S48_OWNER" "npm:somebody-else"

# =============================================================================
# Scenario 49 — unproven ownership is a third state, not permission to delete
# =============================================================================
# A schema-3 receipt with an empty npm_pkg_owner means the marker write failed, so
# ownership was never established. scripts/uninstall.sh has always kept that
# package; the installer's retirement path read the same empty field as "no
# takeover" and deleted the package and its launchers instead. One verdict, decided
# before anything is written, now covers staging and retirement alike.
if [ "$(id -u)" = "0" ]; then
  echo "SKIP the unproven-owner install scenario cannot be exercised as root"
else
  run_script "$INSTALL_SH" unproven-owner "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "the first npm install exits 0" "$RUN_STATUS" "0"
  S49_HOME="$TEST_DIR/unproven-owner/home"
  S49_PREFIX="$TEST_DIR/unproven-owner/npm-prefix"
  S49_PKG="$S49_PREFIX/lib/node_modules/anolisa-tokenless"
  assert_contains "the first receipt records the package owner" \
    "$(receipt_value unproven-owner npm_pkg_owner)" "curl-installer:"
  # Make the marker unwritable so the next receipt records an empty owner.
  rm -f "$S49_PKG/.tokenless-owner"
  mkdir -p "$S49_PKG/.tokenless-owner"
  run_script "$INSTALL_SH" unproven-owner "TOKENLESS_VERSION=$FAKE_VERSION"
  assert_eq "the reinstall still succeeds" "$RUN_STATUS" "0"
  assert_eq "and records an empty package owner" \
    "$(receipt_value unproven-owner npm_pkg_owner)" ""
  # A newer direct npm install takes the package over, then a source rebuild has
  # to decide what the previous receipt still owns.
  rmdir "$S49_PKG/.tokenless-owner"
  printf 'npm:somebody-else\n' > "$S49_PKG/.tokenless-owner"
  run_script "$INSTALL_SH" unproven-owner \
    "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1"
  assert_eq "the source rebuild exits 0" "$RUN_STATUS" "0"
  assert_contains "says ownership was never proven" "$RUN_OUTPUT" \
    "never proved ownership"
  assert_file "keeps the package it cannot prove it owns" "$S49_PKG"
  assert_eq "and does not touch the newer owner's marker" \
    "$(head -n1 "$S49_PKG/.tokenless-owner")" "npm:somebody-else"
  assert_file "keeps the rtk launcher that points into it" "$S49_HOME/.local/bin/rtk"
  assert_contains "and that launcher still reaches the kept package" \
    "$("$S49_HOME/.local/bin/rtk" --version 2>&1)" "-npm"
fi

# =============================================================================
# Scenario 50 — the delegated removal is judged over what npm really deletes
# =============================================================================
# With a separate install directory the receipt records the installer's own
# entries, which point straight at the payload inside the package — it never lists
# npm's own <prefix>/bin links. Judging only recorded paths therefore missed a
# foreign file sitting at one of them: npm deleted it and the run reported success.
S50_HOME="$TEST_DIR/separate-prefix-conflict/home"
S50_PREFIX="$S50_HOME/.npm-global"
run_script "$INSTALL_SH" separate-prefix-conflict \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_PREFIX=$S50_PREFIX" "TOKENLESS_INSTALL_DIR=$S50_HOME/.local/bin"
assert_eq "the separate-layout install exits 0" "$RUN_STATUS" "0"
assert_file "npm created its own bin entry in the prefix" "$S50_PREFIX/bin/tokenless"
rm -f "$S50_PREFIX/bin/tokenless"
printf '#!/bin/sh\necho foreign-prefix-entry\n' > "$S50_PREFIX/bin/tokenless"
chmod +x "$S50_PREFIX/bin/tokenless"
run_script "$UNINSTALL_SH" separate-prefix-conflict "NPM_STUB_PREFIX=$S50_PREFIX"
assert_eq "the separate layout also refuses the delegated removal" "$RUN_STATUS" "1"
assert_contains "says npm uninstall was not run" "$RUN_OUTPUT" "Not running npm uninstall"
assert_file "the foreign prefix entry survives" "$S50_PREFIX/bin/tokenless"
assert_eq "and still runs as the file the user put there" \
  "$("$S50_PREFIX/bin/tokenless" 2>&1)" "foreign-prefix-entry"
assert_file "the global package is kept" "$S50_PREFIX/lib/node_modules/anolisa-tokenless"
assert_file "the receipt is kept for a retry" "$(receipt_of separate-prefix-conflict)"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"

# =============================================================================
# Scenario 51 — npm's exit status cannot prove its postinstall did nothing
# =============================================================================
# A postinstall can write a framework registration and then fail. npm returns
# non-zero either way, so once npm has been invoked the run must report itself
# incomplete rather than switch method and announce success over a machine it
# cannot fully account for.
S51_HOME="$TEST_DIR/postinstall-side-effect/home"
S51_REG="$S51_HOME/.fake-framework/registration"
run_script "$INSTALL_SH" postinstall-side-effect \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_POSTINSTALL_REGISTRATION=$S51_REG" \
  "NPM_STUB_FAIL_AFTER_POSTINSTALL=1"
assert_eq "a postinstall that wrote and then failed exits non-zero" "$RUN_STATUS" "1"
assert_contains "reports the installation is incomplete" "$RUN_OUTPUT" \
  "this installation is incomplete"
assert_not_contains "never starts the source build" "$RUN_OUTPUT" "Building from source"
assert_not_contains "does not report a successful install" "$RUN_OUTPUT" "Installed to"
assert_file "the registration the postinstall wrote is still on disk" "$S51_REG"
assert_no_file "records no receipt for an install that did not complete" \
  "$(receipt_of postinstall-side-effect)"
run_script "$INSTALL_SH" postinstall-side-effect \
  "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1"
assert_eq "the source build remains available as an explicit choice" "$RUN_STATUS" "0"
assert_eq "and records the source method" \
  "$(receipt_value postinstall-side-effect method)" "source"

# =============================================================================
# Scenario 52 — full lifecycle postconditions, not just the exit status
# =============================================================================
# Two ways a script could return the right status and still leave the wrong state:
# hermes with a CLI that is present and executable but refuses (so "the CLI ran"
# is not "the deregistration succeeded", and a flow-sequence config defeats a
# shape-specific regex), and claude-code deleting its plugin cache before deciding
# to fail — which made "resources were left in place" untrue.
run_script "$INSTALL_SH" lifecycle-post "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the lifecycle cases exits 0" "$RUN_STATUS" "0"
S52_ADAPTERS="$TEST_DIR/lifecycle-post/home/.local/share/anolisa/adapters/tokenless"
S52_HOME="$TEST_DIR/lifecycle-post/home"
for fw in hermes claude-code; do
  rm -rf "${S52_ADAPTERS:?}/$fw"
  cp -R "$TOKENLESS_ROOT/adapters/tokenless/$fw" "$S52_ADAPTERS/$fw"
done
S52_ABSENT="$S52_HOME/.no-such-cli"

# (a) hermes CLI present and executable, but every operation fails; flow-sequence
#     YAML that the block-form regex does not match.
mkdir -p "$S52_HOME/.hermes"
printf 'plugins:\n  enabled: [tokenless]\n' > "$S52_HOME/.hermes/config.yaml"
run_script "$UNINSTALL_SH" lifecycle-post \
  "HERMES_BIN=/bin/false" "CLAUDE_BIN=$S52_ABSENT"
assert_eq "an executable CLI that refuses is not a confirmed deregistration" \
  "$RUN_STATUS" "1"
# The exact wording depends on whether a YAML parser is importable here (see
# scenario 55, which covers both paths explicitly); the refusal does not.
assert_contains "says the deregistration could not be confirmed" "$RUN_OUTPUT" \
  "ERROR"
assert_file "keeps the hermes adapter resources" \
  "$S52_ADAPTERS/hermes/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of lifecycle-post)"
rm -f "$S52_HOME/.hermes/config.yaml"

# (b) claude-code: no CLI, no jq, settings still enabling the plugin, and a
#     populated plugin cache the registration points at.
mkdir -p "$S52_HOME/.claude/plugins/cache/anolisa-tokenless"
printf 'installed-plugin-payload\n' \
  > "$S52_HOME/.claude/plugins/cache/anolisa-tokenless/plugin.yaml"
printf '{"enabledPlugins":{"tokenless@anolisa-tokenless":true}}\n' \
  > "$S52_HOME/.claude/settings.json"
RUN_WITHOUT="jq"
run_script "$UNINSTALL_SH" lifecycle-post \
  "HERMES_BIN=$S52_ABSENT" "CLAUDE_BIN=$S52_ABSENT"
RUN_WITHOUT=""
assert_eq "no CLI and no jq is not a clean removal" "$RUN_STATUS" "1"
assert_file "the plugin cache the registration points at was NOT deleted" \
  "$S52_HOME/.claude/plugins/cache/anolisa-tokenless/plugin.yaml"
assert_eq "and its contents are untouched" \
  "$(cat "$S52_HOME/.claude/plugins/cache/anolisa-tokenless/plugin.yaml")" \
  "installed-plugin-payload"
assert_eq "settings.json was left exactly as it was" \
  "$(cat "$S52_HOME/.claude/settings.json")" \
  '{"enabledPlugins":{"tokenless@anolisa-tokenless":true}}'
assert_file "keeps the claude-code adapter resources" \
  "$S52_ADAPTERS/claude-code/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of lifecycle-post)"

# (c) With nothing registered anywhere the same run finishes, and only then is the
#     cache removed — the cleanup is a consequence of success, not a precondition
#     of the failure message.
rm -f "$S52_HOME/.claude/settings.json"
run_script "$UNINSTALL_SH" lifecycle-post \
  "HERMES_BIN=$S52_ABSENT" "CLAUDE_BIN=$S52_ABSENT"
assert_eq "nothing registered anywhere exits 0" "$RUN_STATUS" "0"
assert_no_file "the cache is removed once the run really succeeded" \
  "$S52_HOME/.claude/plugins/cache/anolisa-tokenless"
assert_no_file "the adapter resources are removed" "$S52_ADAPTERS"
assert_no_file "the receipt is removed" "$(receipt_of lifecycle-post)"

# =============================================================================
# Scenario 53 — a rollback must not deregister frameworks it never registered
# =============================================================================
# The bundled adapter uninstall scripts are *full* uninstallers. Running them all
# because this attempt happened to create the shared directory deregistered
# frameworks the install never touched: the Qwen one removes any extension whose
# manifest says `name: tokenless`, with no check that it points at this install's
# resources, so a hand-installed extension that predates the run was deleted and
# reported as "Deregistered the qwencode adapter".
S53_HOME="$TEST_DIR/rollback-foreign-reg/home"
S53_EXT="$S53_HOME/.qwen/extensions/tokenless-manual"
mkdir -p "$S53_EXT"
printf '{"name":"tokenless","version":"1.2.3"}\n' > "$S53_EXT/qwen-extension.json"
printf 'pre-existing manual install\n' > "$S53_EXT/README"
assert_no_file "the shared adapter directory does not exist yet" \
  "$S53_HOME/.local/share/anolisa/adapters/tokenless"
run_script "$INSTALL_SH" rollback-foreign-reg \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless" \
  "NPM_STUB_BROKEN_BIN=1"
assert_eq "the failed attempt exits non-zero" "$RUN_STATUS" "1"
assert_file "the pre-existing Qwen extension survives" "$S53_EXT/qwen-extension.json"
assert_eq "and its manifest is untouched" \
  "$(cat "$S53_EXT/qwen-extension.json")" '{"name":"tokenless","version":"1.2.3"}'
assert_file "and so is the rest of it" "$S53_EXT/README"
assert_not_contains "never claims to have deregistered a framework it did not register" \
  "$RUN_OUTPUT" "Deregistered the qwencode adapter"
assert_file "the adapter tree it created is kept for the user to clean up" \
  "$S53_HOME/.local/share/anolisa/adapters/tokenless"

# =============================================================================
# Scenario 54 — containment is a directory boundary, not a string prefix
# =============================================================================
# `${pkg_dir}*` also matched a *sibling* directory whose name merely starts with
# the package name, so a launcher pointing at an identical payload in somebody
# else's directory was judged ours: npm deleted the entry point and the run
# reported a successful uninstall. The payload survived, the command did not.
S54_HOME="$TEST_DIR/sibling-boundary/home"
S54_PREFIX="$S54_HOME/.npm-global"
run_script "$INSTALL_SH" sibling-boundary \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_PREFIX=$S54_PREFIX" "TOKENLESS_INSTALL_DIR=$S54_HOME/.local/bin"
assert_eq "the npm install exits 0" "$RUN_STATUS" "0"
# A sibling package directory holding a byte-identical payload, and the prefix bin
# entry repointed at it — exactly what a backup or a second checkout looks like.
S54_SIBLING="$S54_PREFIX/lib/node_modules/anolisa-tokenless-backup"
mkdir -p "$S54_SIBLING/bin"
cp "$S54_PREFIX/lib/node_modules/anolisa-tokenless/bin/tokenless" "$S54_SIBLING/bin/tokenless"
ln -sfn "$S54_SIBLING/bin/tokenless" "$S54_PREFIX/bin/tokenless"
run_script "$UNINSTALL_SH" sibling-boundary "NPM_STUB_PREFIX=$S54_PREFIX"
assert_eq "uninstall refuses instead of deleting the external entry" "$RUN_STATUS" "1"
assert_contains "says npm uninstall was not run" "$RUN_OUTPUT" "Not running npm uninstall"
assert_file "the external prefix entry survives" "$S54_PREFIX/bin/tokenless"
assert_eq "and still resolves into the sibling, not the package" \
  "$(readlink "$S54_PREFIX/bin/tokenless")" "$S54_SIBLING/bin/tokenless"
assert_file "the sibling payload is untouched" "$S54_SIBLING/bin/tokenless"
assert_file "the receipt is kept for a retry" "$(receipt_of sibling-boundary)"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"

# =============================================================================
# Scenario 55 — hermes registration state, with and without a YAML parser
# =============================================================================
# "Does the file mention tokenless?" is not the question, and neither is a
# hand-written YAML subset: one was tried and removed, because legal YAML it did
# not model — sequence entries level with `enabled:`, a quoted `"enabled"` key, an
# unrelated deeper `enabled` under `plugins.settings` — was answered as "absent",
# and "absent" is followed by irreversible deletion of the plugin files, the shared
# adapter resources and the receipt. The config is now read with a real YAML
# parser, and with no parser available the answer is "unknown", which fails closed.
#
# Both paths are covered explicitly. The no-parser path is *forced* with a
# `python3 -S` wrapper, so it is deterministic on any host; the with-parser path
# runs wherever a parser is importable. Neither is a silent skip: when no parser
# is available the reason is printed, and the assertions that would need one are
# replaced by the fail-closed ones that do not.
S55_DIR="$TEST_DIR/hermes-states"
S55_GOOD="$S55_DIR/hermes-good"
mkdir -p "$S55_DIR"
cat > "$S55_GOOD" <<'GOODCLI'
#!/bin/sh
# Stands in for a real hermes CLI: disable empties plugins.enabled and records the
# name under plugins.disabled, which is what the real CLI writes.
[ -n "$HERMES_HOME" ] || exit 1
case "$1 $2" in
  "--version ") echo "hermes 1.0"; exit 0 ;;
  "plugins disable")
    printf 'plugins:\n  enabled: []\n  disabled: [tokenless]\n' > "$HERMES_HOME/config.yaml"
    exit 0 ;;
  "plugins remove") exit 0 ;;
esac
exit 0
GOODCLI
chmod +x "$S55_GOOD"
S55_ABSENT="$S55_DIR/no-such-hermes"

S55_YAML="$HOST_YAML"   # both branches below pin the interpreter they assert about
# The no-parser path is forced with `python3 -S` (site-packages disabled), which is
# how it is reached on a minimal machine. pin_hermes_python needs a real python3 to
# wrap; without one there is nothing to force.
if [ -n "$HERMES_REAL_PY3" ]; then
  S55_NOYAML_OK=1
else
  S55_NOYAML_OK=0
  echo "NOTE no python3 on this host: the forced no-parser path cannot be exercised"
fi
if [ "$S55_YAML" = "1" ]; then
  echo "NOTE a YAML parser is importable here: covering the with-parser path"
else
  echo "NOTE no YAML parser importable here: the with-parser matrix is not exercised"
  echo "NOTE on this host; the fail-closed no-parser path below still is"
fi

# hermes_state_case <yaml|noyaml> <label> <config|NONE> <cli> <expected-exit> <files>
#
# The no-parser path is forced by putting a `python3 -S` wrapper in
# $HERMES_HOME/bin, because the adapter script prepends that directory to PATH
# itself -- ahead of /usr/local/bin, which on some hosts carries a python3 with a
# pip-installed PyYAML. Hiding python3 only in the caller's PATH would therefore
# not be reliable, and the case would silently exercise the other branch. The
# self-check below asserts the wrapper really does hide yaml.
hermes_state_case() {
  local mode="$1" label="$2" cfg="$3" cli="$4" want="$5" wantfiles="$6"
  local h="$S55_DIR/case-$$-$RANDOM"
  rm -rf "$h"; mkdir -p "$h/home" "$h/hhome/plugins/tokenless" "$h/hhome/bin"
  printf 'plugin-payload\n' > "$h/hhome/plugins/tokenless/plugin.yaml"
  if [ "$cfg" != "NONE" ]; then printf '%b' "$cfg" > "$h/hhome/config.yaml"; fi
  # Both modes pin the interpreter, so neither depends on what the adapter would
  # otherwise happen to resolve; pin_hermes_python self-checks the result.
  pin_hermes_python "$h/home" "$h/hhome" "$mode"
  local st=0
  env -i PATH="/usr/bin:/bin" HOME="$h/home" HERMES_HOME="$h/hhome" \
    HERMES_BIN="$cli" \
    bash "$TOKENLESS_ROOT/adapters/tokenless/hermes/scripts/uninstall.sh" >/dev/null 2>&1 || st=$?
  assert_eq "hermes: $label" "$st" "$want"
  if [ "$wantfiles" = "kept" ]; then
    assert_file "hermes: $label - plugin files kept" "$h/hhome/plugins/tokenless/plugin.yaml"
  else
    assert_no_file "hermes: $label - plugin files removed" "$h/hhome/plugins/tokenless/plugin.yaml"
  fi
  rm -rf "$h"
}

# ---- path A: no YAML parser. Every config-present case must fail closed ------
if [ "$S55_NOYAML_OK" = "1" ]; then
  hermes_state_case noyaml "no parser: same-indent sequence entries" \
    'plugins:\n  enabled:\n  - tokenless\n' /bin/false 1 kept
  hermes_state_case noyaml "no parser: quoted enabled key" \
    'plugins:\n  "enabled": [tokenless]\n' /bin/false 1 kept
  hermes_state_case noyaml "no parser: deeper unrelated enabled" \
    'plugins:\n  enabled: [tokenless]\n  settings:\n    enabled: []\n' /bin/false 1 kept
  hermes_state_case noyaml "no parser: flow enabled" \
    'plugins:\n  enabled: [tokenless]\n' /bin/false 1 kept
  hermes_state_case noyaml "no parser: normal disabled shape is still unknown" \
    'plugins:\n  enabled: []\n  disabled: [tokenless]\n' "$S55_GOOD" 1 kept
  # No config file needs no parser, so these stay decided either way.
  hermes_state_case noyaml "no parser: no config and no CLI is clean" \
    NONE "$S55_ABSENT" 0 removed
  hermes_state_case noyaml "no parser: no config and a dead CLI cannot confirm" \
    NONE /bin/false 1 kept
fi

# ---- path B: a real YAML parser. Structural answers, both directions ---------
if [ "$S55_YAML" = "1" ]; then
  # The three legal structures the removed hand-written parser misread as absent.
  hermes_state_case yaml "parser: same-indent sequence entries are still enabled" \
    'plugins:\n  enabled:\n  - tokenless\n' /bin/false 1 kept
  hermes_state_case yaml "parser: a quoted enabled key is still read" \
    'plugins:\n  "enabled": [tokenless]\n' /bin/false 1 kept
  hermes_state_case yaml "parser: a deeper unrelated enabled does not shadow it" \
    'plugins:\n  enabled: [tokenless]\n  settings:\n    enabled: []\n' /bin/false 1 kept
  # The success path that must not regress: disable moves the name to disabled.
  hermes_state_case yaml "parser: a successful disable is a success" \
    'plugins:\n  enabled: [tokenless]\n' "$S55_GOOD" 0 removed
  hermes_state_case yaml "parser: flow disabled is not a registration" \
    'plugins:\n  enabled: []\n  disabled: [tokenless]\n' /bin/false 0 removed
  hermes_state_case yaml "parser: block disabled is not a registration" \
    'plugins:\n  enabled: []\n  disabled:\n    - tokenless\n' /bin/false 0 removed
  hermes_state_case yaml "parser: block enabled with a refusing CLI" \
    'plugins:\n  enabled:\n    - tokenless\n' /bin/false 1 kept
  hermes_state_case yaml "parser: a comment mention is not a registration" \
    '# tokenless was enabled here once\nplugins:\n  enabled: []\n' /bin/false 0 removed
  hermes_state_case yaml "parser: an unrelated key mention is not a registration" \
    'other: tokenless\nplugins:\n  enabled: []\n' /bin/false 0 removed
  hermes_state_case yaml "parser: no plugins block is not a registration" \
    'logging:\n  level: info\n' /bin/false 0 removed
  hermes_state_case yaml "parser: an unparsable enabled value is unknown" \
    'plugins:\n  enabled: |\n' /bin/false 1 kept
  hermes_state_case yaml "parser: a scalar enabled value is unknown" \
    'plugins:\n  enabled: tokenless\n' /bin/false 1 kept
fi

# ---- end to end, through the receipt-driven uninstaller ----------------------
# Deterministic on both paths: a config that still enables the plugin (or that
# cannot be parsed) must keep the plugin files, the shared adapter resources and
# the receipt, and exit non-zero.
run_script "$INSTALL_SH" hermes-e2e "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "install before the hermes end-to-end cases exits 0" "$RUN_STATUS" "0"
S55_ADAPTERS="$TEST_DIR/hermes-e2e/home/.local/share/anolisa/adapters/tokenless"
S55_HOME="$TEST_DIR/hermes-e2e/home"
rm -rf "${S55_ADAPTERS:?}/hermes"
cp -R "$TOKENLESS_ROOT/adapters/tokenless/hermes" "$S55_ADAPTERS/hermes"
mkdir -p "$S55_HOME/.hermes/plugins/tokenless"
printf 'plugin-payload\n' > "$S55_HOME/.hermes/plugins/tokenless/plugin.yaml"
printf 'plugins:\n  enabled:\n  - tokenless\n' > "$S55_HOME/.hermes/config.yaml"
if [ "$HOST_YAML" = "1" ]; then
  pin_hermes_python "$S55_HOME" "$S55_HOME/.hermes" yaml
else
  pin_hermes_python "$S55_HOME" "$S55_HOME/.hermes" noyaml
fi
run_script "$UNINSTALL_SH" hermes-e2e "HERMES_BIN=/bin/false" "CLAUDE_BIN=$S55_ABSENT"
assert_eq "a still-enabled registration blocks the whole uninstall" "$RUN_STATUS" "1"
assert_file "keeps the hermes plugin files" "$S55_HOME/.hermes/plugins/tokenless/plugin.yaml"
assert_file "keeps the shared adapter resources" "$S55_ADAPTERS/hermes/scripts/uninstall.sh"
assert_file "keeps the receipt for the retry" "$(receipt_of hermes-e2e)"
assert_not_contains "does not report a completed uninstall" "$RUN_OUTPUT" "uninstall complete"

# A config that positively shows "not enabled" lets the same receipt finish — but
# only a parser can establish that, so this half runs where one is importable.
if [ "$S55_YAML" = "1" ]; then
  printf 'plugins:\n  enabled: []\n  disabled: [tokenless]\n' > "$S55_HOME/.hermes/config.yaml"
  run_script "$UNINSTALL_SH" hermes-e2e "HERMES_BIN=$S55_ABSENT" "CLAUDE_BIN=$S55_ABSENT"
  assert_eq "a normal disabled entry lets the uninstall finish" "$RUN_STATUS" "0"
  assert_no_file "the plugin files are removed" "$S55_HOME/.hermes/plugins/tokenless/plugin.yaml"
  assert_no_file "the adapter resources are removed" "$S55_ADAPTERS"
  assert_no_file "and so is the receipt" "$(receipt_of hermes-e2e)"
else
  # Without a parser the same config cannot be read, so the run must stop and keep
  # everything — asserted instead of skipped, because that is the real behaviour.
  printf 'plugins:\n  enabled: []\n  disabled: [tokenless]\n' > "$S55_HOME/.hermes/config.yaml"
  run_script "$UNINSTALL_SH" hermes-e2e "HERMES_BIN=$S55_ABSENT" "CLAUDE_BIN=$S55_ABSENT"
  assert_eq "without a parser the same config fails closed" "$RUN_STATUS" "1"
  assert_file "and keeps the shared adapter resources" "$S55_ADAPTERS/hermes/scripts/uninstall.sh"
  assert_file "and keeps the receipt" "$(receipt_of hermes-e2e)"
fi

# =============================================================================
# Scenario 56 — a postinstall's external registration must not be orphaned
# =============================================================================
# The real package postinstall does not stop at copying resources: it runs the
# claude-code adapter's own install.sh, which registers the plugin with the Claude
# CLI. That registration lives in the framework's own config, outside the npm prefix
# and outside the adapter tree, so a file-level rollback cannot see it. If the
# attempt then fails, the run must not delete the resources that registration points
# at, must not deregister frameworks it never registered, and must not switch method
# and report success.
S56_DIR="$TEST_DIR/postinstall-registration"
S56_HOME="$S56_DIR/home"
mkdir -p "$S56_DIR" "$S56_HOME/.qwen/extensions/tokenless-manual" "$S56_HOME/.claude"
printf '{"name":"tokenless","version":"1.2.3"}\n' \
  > "$S56_HOME/.qwen/extensions/tokenless-manual/qwen-extension.json"
printf 'pre-existing manual install\n' > "$S56_HOME/.qwen/extensions/tokenless-manual/README"
printf '{"theme":"dark"}\n' > "$S56_HOME/.claude/settings.json"
# A Claude CLI double that records the registration the real CLI would write,
# keeping whatever else was already in the config.
cat > "$S56_DIR/claude-double" <<'CLAUSECLI'
#!/bin/sh
case "$1 $2" in
  "plugin validate") exit 0 ;;
  "plugin marketplace") exit 0 ;;
  "plugin install")
    mkdir -p "$HOME/.claude"
    printf '{"theme":"dark","enabledPlugins":{"tokenless@anolisa-tokenless":true}}\n' \
      > "$HOME/.claude/settings.json"
    exit 0 ;;
esac
exit 0
CLAUSECLI
chmod +x "$S56_DIR/claude-double"
run_script "$INSTALL_SH" postinstall-registration \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless" \
  "NPM_STUB_ENABLE_CLAUDE=1" "NPM_STUB_BROKEN_BIN=1" \
  "CLAUDE_BIN=$S56_DIR/claude-double"
assert_eq "a failed attempt after the postinstall registered exits non-zero" \
  "$RUN_STATUS" "1"
assert_not_contains "never starts the source build" "$RUN_OUTPUT" "Building from source"
assert_not_contains "does not report a successful install" "$RUN_OUTPUT" "Installed to"
S56_ADAPTERS="$S56_HOME/.local/share/anolisa/adapters/tokenless"
assert_file "the pre-existing Qwen registration survives" \
  "$S56_HOME/.qwen/extensions/tokenless-manual/qwen-extension.json"
assert_eq "and is byte-for-byte unchanged" \
  "$(cat "$S56_HOME/.qwen/extensions/tokenless-manual/qwen-extension.json")" \
  '{"name":"tokenless","version":"1.2.3"}'
assert_file "as does the rest of it" "$S56_HOME/.qwen/extensions/tokenless-manual/README"
assert_not_contains "never claims to have deregistered a framework it did not register" \
  "$RUN_OUTPUT" "Deregistered the qwencode adapter"
assert_contains "the pre-existing Claude config is preserved" \
  "$(cat "$S56_HOME/.claude/settings.json")" '"theme"'
assert_contains "and so is the registration the postinstall wrote" \
  "$(cat "$S56_HOME/.claude/settings.json")" "tokenless@anolisa-tokenless"
# The registration points at the adapter resources, so they must still be there.
assert_file "the adapter resources that registration points at are kept" \
  "$S56_ADAPTERS/claude-code/.claude-plugin/plugin.json"
assert_file "and the adapter tree itself is kept" \
  "$S56_ADAPTERS/claude-code/scripts/uninstall.sh"

# =============================================================================
# Scenario 57 — a `not installed` row is a deregistration, not a registration
# =============================================================================
# codex-cli 0.154.0 keeps listing a plugin that a registered marketplace
# offers after `plugin remove`, with its STATUS flipped to `not installed`; the
# row only disappears together with the marketplace, which the adapter script
# removes immediately afterwards. A deregistration check that just looks for
# "tokenless" in `plugin list` read that row as a surviving registration — as it
# read the `Marketplace` header above it, which is printed even when the plugin
# was never added — so every successful uninstall exited 1, kept the marketplace
# directory, and stopped the component uninstaller from dropping the adapter
# resources and the receipt. Scenario 29's retry covers the same row through the
# receipt-driven uninstaller; this one pins the adapter contract directly. A
# registration that really did survive still has to fail (scenarios 29 and 33).
S57_HOME="$TEST_DIR/codex-residual/home"
S57_STATE="$S57_HOME/.codex-stub"
S57_MARKET="$S57_HOME/.local/share/anolisa/codex-marketplace"
mkdir -p "$S57_HOME/.local/bin" "$S57_STATE" "$S57_MARKET/tokenless"
printf '#!/bin/sh\necho kept\n' > "$S57_HOME/.local/bin/tokenless"
chmod +x "$S57_HOME/.local/bin/tokenless"
printf '{"name":"tokenless"}\n' > "$S57_MARKET/tokenless/plugin.json"
: > "$S57_STATE/plugin"
: > "$S57_STATE/marketplace"
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S57_HOME" \
    SHELL=/bin/bash \
    TOKENLESS_DEREGISTER_ONLY=1 \
    bash "$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh" --non-interactive 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "a deregistration codex confirms exits 0" "$RUN_STATUS" "0"
assert_not_contains "does not report a registration that is already gone" "$RUN_OUTPUT" \
  "still lists the tokenless plugin"
assert_contains "reports the run as complete" "$RUN_OUTPUT" "Uninstall complete"
assert_no_file "the plugin registration is gone" "$S57_STATE/plugin"
assert_no_file "and so is the marketplace registration" "$S57_STATE/marketplace"
assert_no_file "so the marketplace directory it pointed at is dropped" "$S57_MARKET"
assert_file "deregistration-only mode still keeps the component binary" \
  "$S57_HOME/.local/bin/tokenless"

echo "install-script test passed"
