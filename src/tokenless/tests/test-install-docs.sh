#!/usr/bin/env bash
# Documentation regression check for the Tokenless install methods.
#
# The install docs describe four production paths (anolisa CLI, npm, curl, Skill)
# whose prerequisites, adapter-enablement steps and platform support differ.
# These assertions keep the docs tied to what scripts/install.sh actually does,
# so a doc cannot drift back into claiming prerequisites, adapter commands or
# platform support that the execution path does not provide.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TOKENLESS_ROOT="$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)"
REPO_ROOT="$(CDPATH='' cd "$TOKENLESS_ROOT/../.." && pwd -P)"

DOC_EN_QUICKSTART="$REPO_ROOT/docs/user-guide/en/token-saving/tokenless/QUICKSTART.md"
DOC_ZH_QUICKSTART="$REPO_ROOT/docs/user-guide/zh/token-saving/tokenless/QUICKSTART.md"
DOC_EN_MANUAL="$REPO_ROOT/docs/user-guide/en/token-saving/tokenless/user-manual.md"
DOC_ZH_MANUAL="$REPO_ROOT/docs/user-guide/zh/token-saving/tokenless/user-manual.md"
DOC_SKILL="$REPO_ROOT/src/os-skills/ai/install-tokenless/SKILL.md"
DOC_EN_TROUBLE="$REPO_ROOT/docs/user-guide/en/token-saving/tokenless/troubleshooting.md"
DOC_ZH_TROUBLE="$REPO_ROOT/docs/user-guide/zh/token-saving/tokenless/troubleshooting.md"
DOC_README_EN="$TOKENLESS_ROOT/README.md"
DOC_README_ZH="$TOKENLESS_ROOT/README_zh.md"
PACKAGE_NPM_JS="$TOKENLESS_ROOT/npm/scripts/package-npm.js"
INSTALL_SH="$TOKENLESS_ROOT/scripts/install.sh"
UNINSTALL_SH="$TOKENLESS_ROOT/scripts/uninstall.sh"

# Managed Skill bundle. `anolisa adapter enable os-skills <framework>` deploys
# only the skills a component manifest declares, and the RPM flattens
# src/os-skills/<category>/<skill>/ into {datadir}/skills/<skill>/, so a
# SKILL.md that is missing from the bundle lists ships but is never deployed.
OS_SKILLS_ROOT="$REPO_ROOT/src/os-skills"
BUNDLE_COMPONENT="$OS_SKILLS_ROOT/component.toml"
BUNDLE_DISTRIBUTION="$REPO_ROOT/src/anolisa/manifests/components/os-skills/component.toml"
OS_SKILLS_INDEX_EN="$OS_SKILLS_ROOT/README.md"
OS_SKILLS_INDEX_ZH="$OS_SKILLS_ROOT/README_zh.md"
SKILL_NAME="install-tokenless"

for f in "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART" "$DOC_EN_MANUAL" "$DOC_ZH_MANUAL" \
         "$DOC_SKILL" "$DOC_EN_TROUBLE" "$DOC_ZH_TROUBLE" "$INSTALL_SH" "$UNINSTALL_SH" \
         "$DOC_README_EN" "$DOC_README_ZH" "$PACKAGE_NPM_JS" \
         "$BUNDLE_COMPONENT" "$BUNDLE_DISTRIBUTION" \
         "$OS_SKILLS_INDEX_EN" "$OS_SKILLS_INDEX_ZH" "$DOC_SKILL"; do
  [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }

# An empty pattern would make has()/has_re() pass against any file and hasnt()
# fail against every file, so all three reject it outright. That is not paranoia:
# a search string written in double quotes with a backtick in it reaches these
# helpers as "" after Bash tries to run the backticked text as a command, and the
# assertion then "passes" while checking nothing.
has() {
  [ -n "$2" ] || fail "$3: empty search pattern (an unquoted backtick in the caller does this)"
  grep -qF -- "$2" "$1" || fail "$3: expected to find '$2' in $(basename "$1")"
  pass "$3"
}
has_re() {
  [ -n "$2" ] || fail "$3: empty pattern (an unquoted backtick in the caller does this)"
  grep -qE -- "$2" "$1" || fail "$3: expected pattern '$2' in $(basename "$1")"
  pass "$3"
}
hasnt() {
  [ -n "$2" ] || fail "$3: empty search pattern (an unquoted backtick in the caller does this)"
  if grep -qF -- "$2" "$1"; then fail "$3: unexpected '$2' in $(basename "$1")"; fi
  pass "$3"
}

# --- QUICKSTART: curl prerequisites must match the execution path -------------
hasnt "$DOC_EN_QUICKSTART" "One-liner, no prerequisites" "en QUICKSTART does not claim curl has no prerequisites"
hasnt "$DOC_ZH_QUICKSTART" "无需前置依赖" "zh QUICKSTART does not claim curl has no prerequisites"
has "$DOC_EN_QUICKSTART" "Node.js 16.7+" "en QUICKSTART states the npm-path prerequisite"
has "$DOC_EN_QUICKSTART" "Rust toolchain" "en QUICKSTART states the source-build prerequisite"
has "$DOC_ZH_QUICKSTART" "Node.js 16.7+" "zh QUICKSTART states the npm-path prerequisite"
has "$DOC_ZH_QUICKSTART" "Rust 工具链" "zh QUICKSTART states the source-build prerequisite"

# --- QUICKSTART: Windows and musl are separate platform rows ------------------
for doc in "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART"; do
  lang=$(basename "$(dirname "$(dirname "$(dirname "$doc")")")")
  has_re "$doc" '^\| Linux with musl|^\| 使用 musl 的 Linux' "$lang QUICKSTART splits musl Linux into its own row"
  has_re "$doc" '^\| Windows \|' "$lang QUICKSTART splits Windows into its own row"
  hasnt "$doc" "Windows or Linux with musl" "$lang QUICKSTART no longer merges Windows with musl"
  hasnt "$doc" "Windows 或使用 musl" "$lang QUICKSTART no longer merges Windows with musl (zh)"
done
has_re "$DOC_EN_QUICKSTART" '^\| Windows \|.*Not supported, use WSL2' "en QUICKSTART marks Windows unsupported for curl"
has_re "$DOC_ZH_QUICKSTART" '^\| Windows \|.*暂不支持，请使用 WSL2' "zh QUICKSTART marks Windows unsupported for curl"
has_re "$DOC_EN_QUICKSTART" '^\| Linux with musl.*Source build only' "en QUICKSTART marks musl as source-build only"
has_re "$DOC_ZH_QUICKSTART" '^\| 使用 musl 的 Linux.*仅源码构建' "zh QUICKSTART marks musl as source-build only"
has "$DOC_EN_QUICKSTART" "source-build fallback is validated on Linux only" "en QUICKSTART states macOS has no source-build path"
has "$DOC_ZH_QUICKSTART" "源码构建回退只在 Linux 上验证过" "zh QUICKSTART states macOS has no source-build path"

# --- QUICKSTART + user manual: adapter enablement per install source ----------
for doc in "$DOC_EN_QUICKSTART" "$DOC_EN_MANUAL"; do
  name=$(basename "$doc")
  has "$doc" "anolisa adapter enable tokenless" "$name keeps the anolisa CLI enable path"
  has "$doc" "adapters/tokenless/claude-code/scripts/install.sh" "$name gives the npm enable path"
  has "$doc" "no anolisa component record" "$name explains why npm cannot use adapter enable"
  has "$doc" "CLI-only" "$name marks the source-build path as CLI-only"
done
for doc in "$DOC_ZH_QUICKSTART" "$DOC_ZH_MANUAL"; do
  name=$(basename "$doc")
  has "$doc" "anolisa adapter enable tokenless" "$name keeps the anolisa CLI enable path"
  has "$doc" "adapters/tokenless/claude-code/scripts/install.sh" "$name gives the npm enable path"
  has "$doc" "anolisa 组件记录" "$name explains why npm cannot use adapter enable"
  has "$doc" "CLI-only" "$name marks the source-build path as CLI-only"
done

# --- QUICKSTART: retired binaries are not advertised --------------------------
hasnt "$DOC_EN_QUICKSTART" '`rtk`, `toon`' "en QUICKSTART does not advertise a toon binary"
hasnt "$DOC_ZH_QUICKSTART" '`rtk`、`toon`' "zh QUICKSTART does not advertise a toon binary"
hasnt "$DOC_SKILL" '`rtk`, and `toon`' "SKILL does not advertise a toon binary"

# --- SKILL: uninstall is symmetric with the install and ownership-scoped ------
has "$DOC_SKILL" "scripts/uninstall.sh" "SKILL points at the receipt-driven uninstaller"
has "$DOC_SKILL" ".local/share/tokenless/install-receipt" "SKILL documents the install receipt"
has "$DOC_SKILL" "--purge" "SKILL documents the opt-in runtime-data purge"
hasnt "$DOC_SKILL" "rm -f ~/.local/bin/tokenless ~/.local/bin/rtk ~/.local/bin/toon" "SKILL drops the blanket curl uninstall block"
hasnt "$DOC_SKILL" "rm -rf ~/.tokenless" "SKILL no longer deletes runtime data unconditionally"
has "$DOC_SKILL" "anolisa uninstall tokenless" "SKILL keeps the anolisa CLI uninstall path"
has "$DOC_SKILL" "Windows is not supported" "SKILL states the Windows boundary"

# --- troubleshooting: the curl method has an upgrade/uninstall story too -------
# "Upgrade and uninstall" is the reference page the Quick Start links to, and it
# already carries one subsection per install method. The curl method must be
# there as well, and it must describe the receipt-driven uninstaller rather than
# a fixed rm list that ignores TOKENLESS_INSTALL_DIR and the npm global package.
has "$DOC_EN_TROUBLE" "### curl standalone installation" "en troubleshooting has a curl standalone section"
has "$DOC_ZH_TROUBLE" "### curl 独立安装" "zh troubleshooting has a curl standalone section"
for doc in "$DOC_EN_TROUBLE" "$DOC_ZH_TROUBLE"; do
  name=$(basename "$(dirname "$(dirname "$(dirname "$doc")")")")/$(basename "$doc")
  has "$doc" ".local/share/tokenless/install-receipt" "$name documents the install receipt"
  has "$doc" "scripts/uninstall.sh" "$name points at the receipt-driven uninstaller"
  has "$doc" "npm uninstall -g anolisa-tokenless" "$name documents the npm global package removal"
  has "$doc" "--dry-run" "$name documents the removal preview"
  has "$doc" "--purge" "$name documents the opt-in runtime-data purge"
  hasnt "$doc" "rm -f ~/.local/bin/tokenless ~/.local/bin/rtk ~/.local/bin/toon" "$name drops the blanket bin rm list"
done

# --- component README: the public install routes it advertises are real -------
# npm/scripts/package-npm.js copies this README straight into the published npm
# package, so a README that denies the npm route contradicts the installation
# the reader just completed.
has "$PACKAGE_NPM_JS" "join(rootPkgDir, 'README.md')" \
  "package-npm.js still ships the component README inside the npm package"
for doc in "$DOC_README_EN" "$DOC_README_ZH"; do
  name=$(basename "$doc")
  has "$doc" "npm install -g anolisa-tokenless" "$name documents the public npm install route"
  has "$doc" "src/tokenless/scripts/install.sh" "$name documents the standalone curl installer"
  has "$doc" ".local/share/tokenless/install-receipt" "$name documents the install receipt"
  has "$doc" "scripts/uninstall.sh" "$name points at the receipt-driven uninstaller"
  has "$doc" "$SKILL_NAME" "$name names the Agent-facing install Skill"
  has "$doc" "@anolisa/tokenless-darwin-x64" "$name keeps the Intel macOS boundary explicit"
done
hasnt "$DOC_README_EN" "are not a public" "en README no longer denies the npm route"
hasnt "$DOC_README_ZH" "目前不能通过公开的" "zh README no longer denies the npm route"
has "$DOC_README_EN" "still has no published package" "en README keeps the accurate Intel macOS caveat"
has "$DOC_README_ZH" "Intel Mac" "zh README keeps the accurate Intel macOS caveat"
has_re "$DOC_EN_QUICKSTART" 'tokenless-darwin-x64`.*not published yet' \
  "en QUICKSTART marks the darwin-x64 platform package as unpublished"
has_re "$DOC_ZH_QUICKSTART" 'tokenless-darwin-x64`.*尚未发布' \
  "zh QUICKSTART marks the darwin-x64 platform package as unpublished"
has "$DOC_SKILL" "Intel macOS (x86_64) has no published platform package" \
  "SKILL states the Intel macOS boundary for the npm method"

# --- Skill bundle: the new Skill must be deployable through the managed path --
# declared_skills <manifest> <framework> prints one declared skill name per line.
declared_skills() {
  awk -v fw="$2" '
    $0 == "[[adapters." fw ".skills]]" { want = 1; next }
    want == 1 && /^name = "/ {
      sub(/^name = "/, ""); sub(/".*$/, ""); print; want = 0
    }
  ' "$1"
}

check_bundle() {
  local label="$1" bundle="$2" fw="$3" name found
  declared_skills "$bundle" "$fw" | grep -qxF "$SKILL_NAME" \
    || fail "$label [$fw] does not declare the $SKILL_NAME skill"
  pass "$label [$fw] declares the $SKILL_NAME skill"
  # A declared skill that has no SKILL.md deploys nothing at all.
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    found=$(find "$OS_SKILLS_ROOT" -mindepth 3 -maxdepth 3 -type f -name SKILL.md \
              -path "*/$name/SKILL.md" | head -1)
    [ -n "$found" ] || fail "$label [$fw] declares '$name', which has no SKILL.md under src/os-skills"
  done < <(declared_skills "$bundle" "$fw")
  pass "$label [$fw] declares only skills that exist on disk"
}

# The two manifests are not interchangeable. src/os-skills/component.toml is the
# source-tree manifest the *next* os-skills artifact is built from, so a new skill
# belongs there immediately. src/anolisa/manifests/components/os-skills/
# component.toml is the distribution contract for an artifact that is *already
# published*: index.toml pins its version to a sha256 and a byte size. Declaring a
# skill here that the pinned artifact does not contain makes `anolisa adapter
# enable os-skills <framework>` fail while copying a source that does not exist,
# so this one may only gain the skill together with a version bump and a new
# index.toml entry.
INDEX_TOML="$REPO_ROOT/src/anolisa/manifests/components/index.toml"
[ -f "$INDEX_TOML" ] || { echo "FAIL missing file: $INDEX_TOML" >&2; exit 1; }
DIST_VERSION=$(awk -F'"' '/^version = /{ print $2; exit }' "$BUNDLE_DISTRIBUTION")
[ -n "$DIST_VERSION" ] || fail "cannot read the os-skills distribution contract version"
pass "the os-skills distribution contract declares version $DIST_VERSION"
grep -A2 '^component = "os-skills"$' "$INDEX_TOML" | grep -qF "version = \"$DIST_VERSION\"" \
  || fail "os-skills $DIST_VERSION is not pinned in index.toml, so the contract describes no published artifact"
pass "the os-skills distribution contract version is pinned to a published artifact"

for fw in openclaw hermes; do
  check_bundle "src/os-skills/component.toml" "$BUNDLE_COMPONENT" "$fw"
  # The distribution contract must stay consistent with the artifact it is pinned
  # to: it may not declare the new skill yet, and everything it does declare must
  # still exist on disk.
  if declared_skills "$BUNDLE_DISTRIBUTION" "$fw" | grep -qxF "$SKILL_NAME"; then
    fail "manifests os-skills component.toml [$fw] declares $SKILL_NAME, which the pinned $DIST_VERSION artifact does not ship"
  fi
  pass "manifests os-skills component.toml [$fw] does not declare $SKILL_NAME ahead of its artifact"
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    found=$(find "$OS_SKILLS_ROOT" -mindepth 3 -maxdepth 3 -type f -name SKILL.md \
              -path "*/$name/SKILL.md" | head -1)
    [ -n "$found" ] || fail "manifests os-skills component.toml [$fw] declares '$name', which has no SKILL.md under src/os-skills"
  done < <(declared_skills "$BUNDLE_DISTRIBUTION" "$fw")
  pass "manifests os-skills component.toml [$fw] declares only skills that exist on disk"
done

# The component manifest is the authoritative bundle: every skill on disk must be
# reachable through it, or the RPM installs a directory no agent ever loads.
missing_bundle_entries=0
while IFS= read -r skill_md; do
  name=$(basename "$(dirname "$skill_md")")
  for fw in openclaw hermes; do
    declared_skills "$BUNDLE_COMPONENT" "$fw" | grep -qxF "$name" || {
      echo "FAIL src/os-skills/component.toml [$fw] is missing the on-disk skill '$name'" >&2
      missing_bundle_entries=1
    }
  done
done < <(find "$OS_SKILLS_ROOT" -mindepth 3 -maxdepth 3 -type f -name SKILL.md | sort)
[ "$missing_bundle_entries" = "0" ] || exit 1
pass "every on-disk OS Skill is declared in both bundle lists of the component manifest"

has "$BUNDLE_COMPONENT" 'source = "{datadir}/skills/install-tokenless/"' \
  "the component bundle points at the flattened RPM skill path"
hasnt "$BUNDLE_DISTRIBUTION" 'source = "{datadir}/skills/install-tokenless/"' \
  "the distribution bundle does not point at a skill its pinned artifact lacks"
has "$OS_SKILLS_INDEX_EN" "**install-tokenless**" "en OS Skills index lists the new skill"
has "$OS_SKILLS_INDEX_ZH" "**install-tokenless**" "zh OS Skills index lists the new skill"

# --- installer script: the documented contracts are the implemented ones ------
has_re "$INSTALL_SH" '-maxdepth ([4-9]|[1-9][0-9]+) ' "install.sh searches deep enough for src/tokenless/Cargo.toml"
hasnt "$INSTALL_SH" "archive/refs/heads/main" "install.sh never downloads the main branch archive"
hasnt "$INSTALL_SH" "local tmpdir" "install.sh keeps the temp dir in a variable the EXIT trap can read"
has "$INSTALL_SH" "trap on_exit EXIT" "install.sh registers its cleanup trap at top level"
has "$INSTALL_SH" "write_receipt" "install.sh records what it created"
has "$INSTALL_SH" "Windows is not supported" "install.sh rejects Windows as documented"
hasnt "$INSTALL_SH" "for bin in tokenless rtk toon" "install.sh does not link the retired toon binary"
has "$UNINSTALL_SH" "No install receipt found" "uninstall.sh refuses to guess without a receipt"

# --- scripts: write failures and ownership are checked, not assumed -----------
has "$INSTALL_SH" "install_status" "install.sh checks the install(1) exit status explicitly"
has "$INSTALL_SH" "verify_cli" "install.sh verifies the binary before recording it"
has "$INSTALL_SH" "retire_previous_receipt" "install.sh retires the previous method's artefacts"
has "$INSTALL_SH" "file_digest" "install.sh records a verifiable file identity"
has "$INSTALL_SH" "deregister_framework_adapters" "install.sh deregisters frameworks before dropping an adapter tree"
has "$UNINSTALL_SH" "deregister_framework_adapters" "uninstall.sh deregisters frameworks before deleting adapter resources"
has "$UNINSTALL_SH" "another installation has taken over that path" \
  "uninstall.sh refuses a recorded path another installer replaced"
has "$UNINSTALL_SH" "file_digest" "uninstall.sh verifies the recorded file identity"

# --- Intel macOS: no route may be advertised that the release cannot deliver --
# `@anolisa/tokenless-darwin-x64` is not on the registry, and the installer
# refuses its source-build fallback on macOS, so every column of the Intel macOS
# row has to read "not supported" until that package is published.
has_re "$DOC_EN_QUICKSTART" \
  '^\| macOS x86_64 \| Not currently supported \| Not currently supported \| Not currently supported \| Not currently supported \|$' \
  "en QUICKSTART marks Intel macOS unsupported for every method"
has_re "$DOC_ZH_QUICKSTART" \
  '^\| macOS x86_64 \| 暂不支持 \| 暂不支持 \| 暂不支持 \| 暂不支持 \|$' \
  "zh QUICKSTART marks Intel macOS unsupported for every method"
hasnt "$DOC_EN_QUICKSTART" "npm and curl ship prebuilt binaries on macOS x86_64" \
  "en QUICKSTART no longer claims prebuilt Intel macOS binaries"
hasnt "$DOC_ZH_QUICKSTART" "npm 和 curl 方式在 macOS x86_64 上提供预编译二进制" \
  "zh QUICKSTART no longer claims prebuilt Intel macOS binaries"
hasnt "$DOC_EN_QUICKSTART" 'Use Method C with `TOKENLESS_FORCE_BUILD=1` there' \
  "en QUICKSTART does not send Intel macOS to a forced source build"
hasnt "$DOC_ZH_QUICKSTART" '该平台请使用方式 C 并设置 `TOKENLESS_FORCE_BUILD=1`' \
  "zh QUICKSTART does not send Intel macOS to a forced source build"
hasnt "$DOC_SKILL" 'Method C with `TOKENLESS_FORCE_BUILD=1` there' \
  "SKILL does not send Intel macOS to a forced source build"
hasnt "$DOC_README_EN" '(`TOKENLESS_FORCE_BUILD=1`) or build from source' \
  "en README does not send Intel macOS to a forced source build"
hasnt "$DOC_README_ZH" '（`TOKENLESS_FORCE_BUILD=1`）或自行从源码构建' \
  "zh README does not send Intel macOS to a forced source build"
# Every page that mentions the boundary must also say the script enforces it.
has "$INSTALL_SH" "Source builds are not supported on macOS" \
  "install.sh refuses the source build on macOS"
has "$DOC_EN_QUICKSTART" 'never invokes `cargo`' "en QUICKSTART states macOS never reaches cargo"
has "$DOC_ZH_QUICKSTART" '绝不会调用 `cargo`' "zh QUICKSTART states macOS never reaches cargo"
has "$DOC_SKILL" 'never runs `cargo`' "SKILL states macOS never reaches cargo"
has "$DOC_README_EN" 'instead of running `cargo`' "en README states macOS never reaches cargo"
has "$DOC_README_ZH" '而不执行 `cargo`' "zh README states macOS never reaches cargo"
has "$DOC_EN_TROUBLE" 'it never runs `cargo`' "en troubleshooting states macOS never reaches cargo"
has "$DOC_ZH_TROUBLE" '绝不会执行 `cargo`' "zh troubleshooting states macOS never reaches cargo"

# --- installer: the npm route is a transaction, not a best effort ------------
has "$INSTALL_SH" "begin_npm_attempt" "install.sh snapshots the npm route before changing anything"
has "$INSTALL_SH" "rollback_npm_attempt" "install.sh rolls a failed npm attempt back"
has "$INSTALL_SH" "remove_npm_package" "install.sh can retire a package whose prefix overlaps the install directory"
# A replacement is only allowed to retire what it superseded once it works, so a
# failed upgrade cannot leave a machine with no CLI and a receipt describing one.
has "$INSTALL_SH" "stage_previous_install" "install.sh keeps the previous install aside while it replaces it"
has "$INSTALL_SH" "commit_previous_install" "install.sh retires the previous install only after the new one verified"
has "$INSTALL_SH" "restore_previous_install" "install.sh puts the previous install back when the run fails"
has "$INSTALL_SH" "trap on_exit EXIT" "install.sh restores the staged install on every exit path"
hasnt "$INSTALL_SH" "retire_previous_npm_package" "install.sh no longer retires the previous package up front"

# --- installer: a shared adapter directory is not claimed for existing -------
has "$INSTALL_SH" "shared_adapters_dir" "install.sh names the shared adapter directory once"
# Identical content is what a same-version reinstall by anolisa or npm leaves
# behind, so ownership has to be proven by something a reinstall removes.
for script in "$INSTALL_SH" "$UNINSTALL_SH"; do
  name=$(basename "$script")
  has "$script" "OWNER_MARKER_FILE" "$name knows the ownership marker file"
  has "$script" "owned_by_receipt" "$name checks recorded identity, not just recorded content"
  has "$script" "resolve_path" "$name resolves symlinks without depending on GNU readlink"
  hasnt "$script" 'readlink -f "' "$name does not call readlink -f directly"
done
has "$INSTALL_SH" "new_install_id" "install.sh stamps a per-run ownership id"
has "$INSTALL_SH" "RECEIPT_SCHEMA=3" "install.sh writes the ownership-aware receipt schema"
has "$UNINSTALL_SH" "npm_pkg_owner" "uninstall.sh checks the npm package ownership marker"
has "$UNINSTALL_SH" "adapters_dir_owner" "uninstall.sh checks the adapter tree ownership marker"
has "$UNINSTALL_SH" "file_target" "uninstall.sh checks the recorded launcher link target"
POSTINSTALL_JS="$TOKENLESS_ROOT/npm/scripts/postinstall.js"
has "$POSTINSTALL_JS" "foreignAdapterOwner" "postinstall.js identifies the adapter tree's owner before replacing it"
has "$POSTINSTALL_JS" "components', 'tokenless', 'component.toml'" \
  "postinstall.js recognises an anolisa-managed component install"
has "$POSTINSTALL_JS" "ANOLISA_TOKENLESS_FORCE_ADAPTERS" "postinstall.js documents the takeover override"

# --- docs: the shared adapter directory and the ownership model --------------
for doc in "$DOC_EN_TROUBLE" "$DOC_ZH_TROUBLE" "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART"; do
  name="$(basename "$(dirname "$(dirname "$(dirname "$doc")")")")/$(basename "$doc")"
  has "$doc" "ANOLISA_TOKENLESS_FORCE_ADAPTERS=1" "$name documents the adapter takeover override"
done
for doc in "$DOC_SKILL" "$DOC_README_EN" "$DOC_README_ZH" "$DOC_EN_TROUBLE" "$DOC_ZH_TROUBLE"; do
  name=$(basename "$doc")
  has "$doc" ".tokenless-owner" "$name names the ownership marker"
done
# The docs must not still promise the round-3 behaviour, where the previous
# package was retired before the replacement was known to work.
hasnt "$DOC_EN_TROUBLE" "retired **before** the new files are written" \
  "en troubleshooting no longer promises an up-front retirement"
hasnt "$DOC_ZH_TROUBLE" "会在写入新文件**之前**回收" \
  "zh troubleshooting no longer promises an up-front retirement"
has "$DOC_EN_TROUBLE" "moved aside first and put back if the new one fails" \
  "en troubleshooting documents that a failed replacement restores the old install"
has "$DOC_ZH_TROUBLE" "先把原安装挪到一边" \
  "zh troubleshooting documents that a failed replacement restores the old install"
has "$DOC_SKILL" "moved aside first and put back if the new one fails" \
  "SKILL documents that a failed replacement restores the old install"
has "$DOC_SKILL" "kept unchanged and reported" \
  "SKILL documents that npm preserves an adapter tree it cannot prove it owns"
has "$DOC_SKILL" '.tokenless-owner' "SKILL names the marker that proves adapter ownership"
has "$INSTALL_SH" "adapters_owned_by_previous_receipt" \
  "install.sh proves adapter ownership before claiming the tree"
has "$INSTALL_SH" "adapters_owned_by_us" "install.sh recognises its own family's adapter tree"
has "$INSTALL_SH" "anolisa_component_contract" "install.sh detects an anolisa-managed component install"
has "$INSTALL_SH" "ADAPTERS_FOREIGN" "install.sh tells a foreign adapter tree from its own"
has "$DOC_SKILL" "shared with the anolisa CLI" "SKILL says the adapter directory is shared"
has "$DOC_EN_TROUBLE" "shared with the anolisa CLI" "en troubleshooting says the adapter directory is shared"
has "$DOC_ZH_TROUBLE" '与 anolisa CLI 以及直接执行的 `npm install -g` 共享' \
  "zh troubleshooting says the adapter directory is shared"

# --- adapter deregistration never touches component files --------------------
CODEX_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh"
[ -f "$CODEX_ADAPTER_UNINSTALL" ] || { echo "FAIL missing file: $CODEX_ADAPTER_UNINSTALL" >&2; exit 1; }
for script in "$INSTALL_SH" "$UNINSTALL_SH"; do
  name=$(basename "$script")
  has "$script" "TOKENLESS_DEREGISTER_ONLY=1" "$name limits adapter scripts to deregistration"
  has "$script" "</dev/null" "$name closes the adapter script's stdin so a prompt cannot block"
done
has "$CODEX_ADAPTER_UNINSTALL" "TOKENLESS_DEREGISTER_ONLY" "the Codex adapter honours deregistration-only mode"
has "$CODEX_ADAPTER_UNINSTALL" "Deregistration only" "the Codex adapter says why it keeps the component binary"
has "$CODEX_ADAPTER_UNINSTALL" "No terminal to confirm on" \
  "the Codex adapter does not delete a binary it cannot ask about"

# --- SKILL: the npm route installs, enables and uninstalls in that order -----
# A framework registration points into the adapter directory, so the resources
# have to outlive the registration: deregister, then `npm uninstall -g`, then
# delete the directory. The troubleshooting page already prescribes that order
# for an npm installation; the Skill must not contradict it.
has "$DOC_SKILL" "npm install -g anolisa-tokenless" "SKILL documents the npm install step"
has "$DOC_SKILL" "adapters/tokenless/claude-code/scripts/install.sh" "SKILL documents the npm enable step"
has "$DOC_SKILL" "adapters/tokenless/<framework>/scripts/uninstall.sh" \
  "SKILL documents the npm deregistration step"
has "$DOC_SKILL" "npm uninstall -g anolisa-tokenless" "SKILL documents the npm uninstall step"
skill_line() { grep -nF -- "$1" "$DOC_SKILL" | head -1 | cut -d: -f1; }
skill_last_line() { grep -nF -- "$1" "$DOC_SKILL" | tail -1 | cut -d: -f1; }
SKILL_FW_LINE=$(skill_line 'adapters/tokenless/<framework>/scripts/uninstall.sh')
SKILL_NPM_LINE=$(skill_last_line 'npm uninstall -g anolisa-tokenless')
SKILL_RM_LINE=$(skill_last_line 'rm -rf ~/.local/share/anolisa/adapters/tokenless')
[ -n "$SKILL_FW_LINE" ] && [ -n "$SKILL_NPM_LINE" ] && [ -n "$SKILL_RM_LINE" ] \
  || fail "SKILL is missing one of the three npm uninstall steps"
pass "SKILL documents all three npm uninstall steps"
[ "$SKILL_FW_LINE" -lt "$SKILL_NPM_LINE" ] \
  || fail "SKILL deregisters the framework after 'npm uninstall -g' (line ${SKILL_FW_LINE} vs ${SKILL_NPM_LINE})"
pass "SKILL deregisters the framework before 'npm uninstall -g'"
[ "$SKILL_NPM_LINE" -lt "$SKILL_RM_LINE" ] \
  || fail "SKILL deletes the adapter resources before 'npm uninstall -g' (line ${SKILL_RM_LINE} vs ${SKILL_NPM_LINE})"
pass "SKILL deletes the adapter resources last"
has "$DOC_SKILL" "before** removing the adapter resources" \
  "SKILL says the registration has to be removed while its resources still exist"

# --- installer: an upgrade is a transaction end to end ----------------------
has "$INSTALL_SH" "snapshot_npm_package" "install.sh snapshots the package payload an upgrade replaces"
has "$INSTALL_SH" "restore_npm_package" "install.sh puts the previous package payload back"
has "$INSTALL_SH" "drop_snapshot_dir" "install.sh keeps a snapshot it could not restore"
has "$INSTALL_SH" "the only remaining copy is kept at" \
  "install.sh reports where an unrestorable copy is instead of deleting it"
has "$INSTALL_SH" "plan_carried_path_rc" "install.sh plans the receipt before retiring anything"
has "$INSTALL_SH" 'mv -f "$receipt_tmp" "$RECEIPT_FILE"' "install.sh puts the receipt in place atomically"
has "$INSTALL_SH" "previous receipt cannot be removed" \
  "install.sh fails rather than leaving a stale receipt behind"
# Retirement has to follow the durable receipt write: a stale receipt describing a
# replaced install makes a later uninstall delete the new one.
MV_LINE=$(grep -nF 'mv -f "$receipt_tmp" "$RECEIPT_FILE"' "$INSTALL_SH" | head -1 | cut -d: -f1)
RETIRE_LINE=$(awk -v start="${MV_LINE:-0}" \
  'NR > start && /^[[:space:]]*retire_previous_receipt$/ { print NR; exit }' "$INSTALL_SH")
[ -n "$MV_LINE" ] && [ -n "$RETIRE_LINE" ] && [ "$MV_LINE" -lt "$RETIRE_LINE" ] \
  || fail "install.sh must retire the previous install only after the new receipt is in place"
pass "install.sh retires the previous install only after the new receipt is durable"

# --- the postinstall ownership test has to actually run ---------------------
MAKEFILE="$TOKENLESS_ROOT/Makefile"
TEST_NPM_POSTINSTALL="$TOKENLESS_ROOT/tests/test-npm-postinstall.sh"
for f in "$MAKEFILE" "$TEST_NPM_POSTINSTALL"; do
  [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done
has "$MAKEFILE" "test-npm-postinstall-strict" "the Makefile has a strict postinstall test target"
has "$MAKEFILE" "test-install-script test-install-docs test-npm-postinstall-strict" \
  "npm packaging runs the install-method suite where a usable node is guaranteed"
has "$TEST_NPM_POSTINSTALL" "TOKENLESS_REQUIRE_NODE" \
  "the postinstall test can require a usable interpreter instead of skipping"
has "$TEST_NPM_POSTINSTALL" "node_candidates" \
  "the postinstall test looks past PATH for an interpreter that can run the script"

# --- docs: the transaction is described the way it is implemented -----------
has "$DOC_EN_TROUBLE" "to a temporary in the same directory, moved into place, then flushed" \
  "en troubleshooting documents the durable receipt write"
has "$DOC_ZH_TROUBLE" "先写到同目录下的临时文件，再移动到位并落盘" \
  "zh troubleshooting documents the durable receipt write"
has "$DOC_SKILL" "written to a temporary, moved into place and flushed" \
  "SKILL documents the durable receipt write"
has "$DOC_EN_TROUBLE" "a copy of the previous module directory" \
  "en troubleshooting documents the npm payload snapshot"
has "$DOC_ZH_TROUBLE" "保留旧模块目录" "zh troubleshooting documents the npm payload snapshot"
has "$DOC_SKILL" "keeps a copy of the previous" "SKILL documents the npm payload snapshot"

# --- an untouched foreign tree is not rebuilt, and failures stay retryable ---
has "$INSTALL_SH" "adapters_unchanged" \
  "install.sh compares the foreign tree against its snapshot before restoring"
has "$INSTALL_SH" "Left the adapter resources in" \
  "install.sh reports when it left a foreign tree alone"
has "$INSTALL_SH" "Cannot create a build directory" \
  "install.sh checks the source build's mktemp instead of assuming it"
has "$INSTALL_SH" "Failed to extract" \
  "install.sh checks the source build's tar instead of assuming it"
has "$INSTALL_SH" "strip_path_rc" \
  "install.sh retires the PATH entry of a previous install directory"
has "$UNINSTALL_SH" "NPM_LEFT_BEHIND" \
  "uninstall.sh keeps the receipt when it could not remove the npm package"
HERMES_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/hermes/scripts/uninstall.sh"
CLAUDE_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/claude-code/scripts/uninstall.sh"
for f in "$HERMES_ADAPTER_UNINSTALL" "$CLAUDE_ADAPTER_UNINSTALL" "$CODEX_ADAPTER_UNINSTALL"; do
  [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done
has "$HERMES_ADAPTER_UNINSTALL" "still lists tokenless under plugins.enabled" \
  "the Hermes adapter verifies the config entry is gone"
has "$CLAUDE_ADAPTER_UNINSTALL" "jq unavailable, matched literally" \
  "the Claude Code adapter verifies without jq instead of skipping the check"
has "$CODEX_ADAPTER_UNINSTALL" "Keeping marketplace directory" \
  "the Codex adapter does not delete the marketplace a surviving registration needs"
has "$DOC_EN_TROUBLE" "stops short rather than half-finishing" \
  "en troubleshooting documents the retryable uninstall"
has "$DOC_ZH_TROUBLE" "宁可不做完也不做一半" \
  "zh troubleshooting documents the retryable uninstall"
has "$DOC_SKILL" "stops short rather than half-finishing" \
  "SKILL documents the retryable uninstall"
has "$DOC_EN_TROUBLE" "only puts the snapshot back if something really did replace it" \
  "en troubleshooting documents that an untouched foreign tree is not rebuilt"
has "$DOC_ZH_TROUBLE" "只有确实被替换过才恢复" \
  "zh troubleshooting documents that an untouched foreign tree is not rebuilt"

# --- QUICKSTART: the install matrix keeps the documented priority order ------
# specs/documentation-standard.md fixes the installation priority every component
# doc has to follow: anolisa CLI first, the RPM package second, a source build
# last. A method matrix that lists npm and curl ahead of RPM steers Alinux users
# away from the managed package and towards routes that create no component record.
SPEC_DOC_STANDARD="$REPO_ROOT/specs/documentation-standard.md"
[ -f "$SPEC_DOC_STANDARD" ] || { echo "FAIL missing file: $SPEC_DOC_STANDARD" >&2; exit 1; }
has "$SPEC_DOC_STANDARD" 'RPM package (`yum install`)' \
  "the documentation standard still fixes the priority this asserts against"

matrix_line() { grep -nF -- "$2" "$1" | head -1 | cut -d: -f1; }
check_matrix_order() {
  local lang="$1" doc="$2" anolisa_row="$3" rpm_row="$4" npm_row="$5" a r n
  a=$(matrix_line "$doc" "$anolisa_row")
  r=$(matrix_line "$doc" "$rpm_row")
  n=$(matrix_line "$doc" "$npm_row")
  [ -n "$a" ] && [ -n "$r" ] && [ -n "$n" ] \
    || fail "$lang QUICKSTART install matrix is missing the anolisa, RPM or npm row"
  pass "$lang QUICKSTART install matrix lists the anolisa, RPM and npm rows"
  [ "$a" -lt "$r" ] || fail "$lang QUICKSTART lists RPM before the anolisa CLI (line $r vs $a)"
  pass "$lang QUICKSTART lists the anolisa CLI first"
  [ "$r" -lt "$n" ] || fail "$lang QUICKSTART lists npm before the RPM route (line $n vs $r)"
  pass "$lang QUICKSTART lists the RPM route right after the anolisa CLI"
}
check_matrix_order "en" "$DOC_EN_QUICKSTART" \
  '| [anolisa CLI](#method-a-anolisa-cli-recommended) |' \
  '| [RPM](#rpm-alinux) |' \
  '| [npm](#method-b-npm) |'
check_matrix_order "zh" "$DOC_ZH_QUICKSTART" \
  '| [anolisa CLI](#方式-aanolisa-cli推荐) |' \
  '| [RPM](#rpm-alinux) |' \
  '| [npm](#方式-bnpm) |'

has "$DOC_EN_QUICKSTART" "### RPM (Alinux) {#rpm-alinux}" \
  "en QUICKSTART has the RPM section its matrix row links to"
has "$DOC_ZH_QUICKSTART" "### RPM（Alinux） {#rpm-alinux}" \
  "zh QUICKSTART has the RPM section its matrix row links to"
for doc in "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART" "$DOC_SKILL"; do
  name=$(basename "$doc")
  has "$doc" "sudo yum install anolisa tokenless" "$name gives the RPM install command"
  has "$doc" "sudo anolisa --install-mode system adopt tokenless" "$name gives the RPM adopt command"
done

# --- ownership has to be proven, and failures have to stop short --------------
has "$POSTINSTALL_JS" "an installation that left no ownership marker" \
  "postinstall.js treats an unmarked adapter tree as somebody else's"
has "$UNINSTALL_SH" "so the uninstall can be retried" \
  "uninstall.sh keeps the receipt when it could not finish"
has "$UNINSTALL_SH" "NPM_PKG_TAKEN_OVER" \
  "uninstall.sh settles the npm ownership verdict before deleting launchers"
has "$INSTALL_SH" "Cannot create a staging directory" \
  "install.sh refuses to start when it cannot stage the previous install"
has "$INSTALL_SH" "refusing to replace an install that" \
  "install.sh refuses to replace a file it could not stage"
has "$DOC_EN_TROUBLE" "carries none and is kept as well" \
  "en troubleshooting documents that an unmarked tree is preserved"
has "$DOC_ZH_TROUBLE" "都没有标记，同样会被保留" \
  "zh troubleshooting documents that an unmarked tree is preserved"
has "$DOC_EN_TROUBLE" "the receipt are both kept and the script exits non-zero" \
  "en troubleshooting documents the retryable uninstall"
has "$DOC_ZH_TROUBLE" "都会保留**，脚本以非 0 退出" \
  "zh troubleshooting documents the retryable uninstall"
has "$DOC_SKILL" "stops short" "SKILL documents the retryable uninstall"
has "$DOC_SKILL" "fail-closed" "SKILL documents that staging is fail-closed"
# The Skill is declared in the source-tree manifest, which the next os-skills
# artifact is built from — not in the distribution contract, which is pinned to an
# artifact that already exists and therefore cannot contain it yet.
has "$DOC_EN_QUICKSTART" "does not list it yet" \
  "en QUICKSTART does not claim the published os-skills bundle already ships the Skill"
has "$DOC_ZH_QUICKSTART" "还没有它" \
  "zh QUICKSTART does not claim the published os-skills bundle already ships the Skill"

# --- the Node floor is the one the postinstall script actually needs ---------
NPM_PACKAGE_JSON="$TOKENLESS_ROOT/npm/package.json"
[ -f "$NPM_PACKAGE_JSON" ] || { echo "FAIL missing file: $NPM_PACKAGE_JSON" >&2; exit 1; }
has "$POSTINSTALL_JS" "cpSync" "postinstall.js still uses fs.cpSync"
has "$NPM_PACKAGE_JSON" '">=16.7.0"' "the package engine floor covers fs.cpSync"
has "$PACKAGE_NPM_JS" "NODE_ENGINE_MIN = '16.7.0'" \
  "the packer stamps the published package with the same floor"
hasnt "$PACKAGE_NPM_JS" "engines: { node: '>=16.0.0' }" \
  "the packer no longer advertises a floor the postinstall cannot run on"
for doc in "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART" "$DOC_SKILL" "$DOC_README_EN" "$DOC_README_ZH"; do
  name=$(basename "$doc")
  has "$doc" "16.7" "$name states the Node floor the postinstall needs"
  hasnt "$doc" "Node.js 16+" "$name no longer promises plain Node.js 16+"
done

# --- adapter deregistration is verified, not assumed -------------------------
CODEX_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh"
CLAUDE_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/claude-code/scripts/uninstall.sh"
OPENCLAW_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/openclaw/scripts/uninstall.sh"
QWENCODE_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/qwencode/scripts/uninstall.sh"
QWENPAW_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/qwenpaw/scripts/uninstall.sh"
for f in "$CODEX_ADAPTER_UNINSTALL" "$CLAUDE_ADAPTER_UNINSTALL" \
         "$OPENCLAW_ADAPTER_UNINSTALL" "$QWENCODE_ADAPTER_UNINSTALL" \
         "$QWENPAW_ADAPTER_UNINSTALL"; do
  [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done
# Every one of these swallows the framework CLI's status on purpose — "was not
# registered" and "refused" both come back non-zero — so each has to re-check the
# registration afterwards and fail when it survived. Without that the top-level
# uninstaller reads exit 0 as "deregistered" and deletes the adapter resources the
# surviving registration points at.
has "$CODEX_ADAPTER_UNINSTALL" "still lists the tokenless plugin" \
  "the Codex adapter re-asks the CLI instead of trusting its exit status"
has "$CLAUDE_ADAPTER_UNINSTALL" "is still in .enabledPlugins" \
  "the Claude Code adapter verifies the plugin entry is gone"
has "$OPENCLAW_ADAPTER_UNINSTALL" "is still there after 'plugins uninstall'" \
  "the OpenClaw adapter verifies its plugin directories are gone"
has "$QWENCODE_ADAPTER_UNINSTALL" "is still there after 'extensions uninstall'" \
  "the Qwen Code adapter verifies the extension is gone"
has "$QWENPAW_ADAPTER_UNINSTALL" "did not remove" \
  "the QwenPaw adapter keeps verifying its plugin directory"

# --- unproven ownership is not ownership ------------------------------------
has "$UNINSTALL_SH" "NPM_PKG_UNPROVEN" \
  "uninstall.sh distinguishes 'ownership never proven' from 'taken over'"
has "$INSTALL_SH" "NOT owning that npm package" \
  "install.sh says so when it could not stamp the npm package"
has "$INSTALL_SH" "NOT owning that adapter tree" \
  "install.sh says so when it could not stamp the adapter tree"

# --- the Makefile joins `test` without editing the shared aggregate line ------
has "$MAKEFILE" "test: test-install-methods" \
  "the Makefile joins the install-method suite to the test aggregate by its own rule"

# --- a probe that cannot answer is not an answer of "nothing there" ---------
has "$CODEX_ADAPTER_UNINSTALL" "cannot be confirmed whether the plugin is registered" \
  "the Codex adapter treats a failed plugin list as unconfirmed"
has "$CODEX_ADAPTER_UNINSTALL" "cannot be confirmed whether the marketplace is registered" \
  "the Codex adapter treats a failed marketplace list as unconfirmed"
has "$CLAUDE_ADAPTER_UNINSTALL" "nothing confirms the plugin was removed" \
  "the Claude Code adapter fails when it has no way to verify"
has "$CLAUDE_ADAPTER_UNINSTALL" "could not be parsed" \
  "the Claude Code adapter treats unparsable settings as unconfirmed"
has "$HERMES_ADAPTER_UNINSTALL" "nothing confirms the plugin was disabled" \
  "the Hermes adapter fails when it has no way to verify"
has "$UNINSTALL_SH" "NPM_PKG_KEEP_REASON" \
  "uninstall.sh settles the npm verdict before deleting any launcher"
has "$UNINSTALL_SH" "launcher links are still in place" \
  "uninstall.sh only claims the launchers survive when it kept them"

# --- adapter replacement is a verified swap, not a delete-then-copy ---------
has "$POSTINSTALL_JS" "renameSync" "postinstall.js swaps the adapter tree instead of overwriting it"
has "$POSTINSTALL_JS" "were left in place" \
  "postinstall.js reports that a failed copy kept the existing tree"
has "$POSTINSTALL_JS" "The previous adapter resources are kept at" \
  "postinstall.js reports where the old tree is when the swap could not finish"
has "$INSTALL_SH" "restore_adapters_snapshot" \
  "install.sh restores the adapter tree through a verified swap"
has "$INSTALL_SH" 'tokenless-restore.$$' \
  "install.sh stages the swap in a per-run directory, not a fixed name"

# --- the platform-package fixture follows the detected platform -------------
TEST_INSTALL_SCRIPT="$TOKENLESS_ROOT/tests/test-install-script.sh"
[ -f "$TEST_INSTALL_SCRIPT" ] || { echo "FAIL missing file: $TEST_INSTALL_SCRIPT" >&2; exit 1; }
has "$TEST_INSTALL_SCRIPT" "platform_layout_case" \
  "the platform-package scenario is parameterised by detected platform"
has "$TEST_INSTALL_SCRIPT" "platform_layout_case aarch64 linux-arm64" \
  "the platform-package scenario covers Linux aarch64 as well as x86_64"
has "$TEST_INSTALL_SCRIPT" "standalone global platform package SURVIVES" \
  "the scenario asserts a standalone sibling package survives, not that it is deleted"
hasnt "$TEST_INSTALL_SCRIPT" 'S39_SCOPE="$S39_HOME/.local/lib/node_modules/@anolisa/tokenless-linux-x64"' \
  "no scenario pins the platform package to linux-x64"

# --- an incomplete rollback stops the run instead of building over it --------
has "$INSTALL_SH" "NPM_ROLLBACK_INCOMPLETE" \
  "install.sh aggregates whether the rollback copied everything back"
has "$INSTALL_SH" "Refusing to continue after an incomplete npm rollback" \
  "install.sh refuses the source-build fallback after an incomplete rollback"

# --- a refused npm uninstall keeps the launchers -----------------------------
has "$UNINSTALL_SH" "DEFERRED_NPM_LAUNCHERS" \
  "uninstall.sh defers the prefix launchers until the removal is confirmed"
has "$UNINSTALL_SH" "NPM_REMOVAL_FAILED" \
  "uninstall.sh keeps the receipt when npm is present but refuses"

# --- npm owns its dependency layout; a sibling @anolisa scope is never ours ---
hasnt "$INSTALL_SH" "remove_npm_platform_package" \
  "install.sh no longer deletes a sibling @anolisa platform package"
hasnt "$UNINSTALL_SH" "remove_npm_platform_package" \
  "uninstall.sh no longer deletes a sibling @anolisa platform package"
hasnt "$UNINSTALL_SH" "npm_platform_pkg" \
  "the receipt no longer records a platform package for the uninstaller to delete"
hasnt "$INSTALL_SH" "npm-scope-absent" \
  "a rollback no longer rm -rf's an @anolisa scope it never owned"

# --- the ownership verdict gates the delegated npm removal too ---------------
has "$UNINSTALL_SH" "npm_delegated_conflict" \
  "uninstall.sh judges the paths npm actually deletes, not just the receipt's list"
has "$UNINSTALL_SH" "Not running npm uninstall" \
  "uninstall.sh says so instead of delegating the deletion anyway"

# --- the npm doc promise is bounded by the release that actually ships it -----
# Verified against the published tarball: anolisa-tokenless@0.8.2's postinstall.js
# does rmSync(dest, {recursive, force}) then cpSync, with no occurrence of
# .tokenless-owner, component.toml or ANOLISA_TOKENLESS_FORCE_ADAPTERS. Telling a
# user who runs `npm install -g` today that a managed tree is preserved would be a
# promise the published package does not keep.
has "$DOC_EN_QUICKSTART" 'published packages up to and including `0.8.2` predate it' \
  "the EN quick start bounds the managed-adapter promise to the release that ships it"
has "$DOC_ZH_QUICKSTART" "0.8.2" \
  "the ZH quick start bounds the managed-adapter promise to the release that ships it"

# --- a failed attempt's rollback deregisters nothing --------------------------
# The bundled adapter uninstall scripts are full uninstallers: running them all
# because this attempt created the shared directory removed a hand-installed Qwen
# extension that predated the run.
has "$INSTALL_SH" "it deregisters nothing and deletes" \
  "install.sh's rollback refuses to deregister frameworks it cannot attribute"
has "$INSTALL_SH" "Deregister any framework you enabled against it" \
  "install.sh tells the user what to do with the adapter tree it kept"

# --- containment is a directory boundary, not a string prefix ----------------
has "$UNINSTALL_SH" '"${pkg_dir}/"*' \
  "uninstall.sh matches the package directory with a trailing separator"
hasnt "$UNINSTALL_SH" '"${pkg_dir}"*) ;;' \
  "uninstall.sh no longer matches a sibling directory sharing the package's prefix"
has "$INSTALL_SH" '"${pkg_dir}/"*' \
  "install.sh applies the same directory boundary when retiring bin links"

# --- the two uninstall entry points are documented as different --------------
has "$DOC_EN_QUICKSTART" "No install receipt found" \
  "the EN quick start says the receipt-driven uninstaller is not the npm entry point"
has "$DOC_ZH_QUICKSTART" "No install receipt found" \
  "the ZH quick start says the receipt-driven uninstaller is not the npm entry point"

# --- one ownership verdict, applied before anything is moved -----------------
has "$INSTALL_SH" "decide_previous_npm_owner" \
  "install.sh decides the previous npm package's owner once, before any write"
has "$INSTALL_SH" "previous_npm_launcher" \
  "staging and retirement share that verdict for the previous launchers"

# --- one ownership verdict, three states, decided before any write -----------
has "$INSTALL_SH" "OLD_NPM_PKG_STATE" \
  "install.sh distinguishes ours / foreign / unproven instead of ours / not-ours"
has "$INSTALL_SH" "never proved ownership" \
  "install.sh keeps a package whose ownership was never proven"

# --- no automatic method switch once npm has run -----------------------------
has "$INSTALL_SH" "NPM_INSTALL_STARTED" \
  "install.sh records that npm was invoked, which no exit status can undo"
has "$INSTALL_SH" "Refusing to switch install method after npm ran" \
  "install.sh reports an incomplete install instead of switching method"

# --- "cannot confirm" is a third state, not a success ------------------------
CLAUDE_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/claude-code/scripts/uninstall.sh"
[ -f "$CLAUDE_ADAPTER_UNINSTALL" ] || { echo "FAIL missing file: $CLAUDE_ADAPTER_UNINSTALL" >&2; exit 1; }
has "$HERMES_ADAPTER_UNINSTALL" "hermes_registration_state" \
  "hermes reads its registration state structurally, not by matching one YAML shape"
hasnt "$HERMES_ADAPTER_UNINSTALL" "hermes_config_mentions_tokenless" \
  "hermes no longer treats any mention of the name as a live registration"
has "$HERMES_ADAPTER_UNINSTALL" "plugins.disabled" \
  "hermes knows a disabled entry is what a successful disable writes"
has "$CLAUDE_ADAPTER_UNINSTALL" "MANUAL_FAILED" \
  "claude-code's manual fallback reports failure instead of a silent success"
has "$CLAUDE_ADAPTER_UNINSTALL" "jq is unavailable" \
  "claude-code fails closed when neither the CLI nor jq can verify settings.json"
has "$CLAUDE_ADAPTER_UNINSTALL" "settings and the plugin cache" \
  "claude-code keeps the plugin cache when it reports resources were left in place"

# --- a missing framework CLI is not proof its registration is gone -----------
CODEX_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/codex/scripts/uninstall.sh"
HERMES_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/hermes/scripts/uninstall.sh"
QODER_ADAPTER_UNINSTALL="$TOKENLESS_ROOT/adapters/tokenless/qoder/scripts/uninstall.sh"
for f in "$CODEX_ADAPTER_UNINSTALL" "$HERMES_ADAPTER_UNINSTALL" "$QODER_ADAPTER_UNINSTALL"; do
  [ -f "$f" ] || { echo "FAIL missing file: $f" >&2; exit 1; }
done
has "$CODEX_ADAPTER_UNINSTALL" 'CODEX_HOME:-$HOME/.codex' \
  "codex checks its own config when the CLI cannot be queried"
has "$CODEX_ADAPTER_UNINSTALL" "registration cannot be confirmed gone" \
  "codex fails closed rather than reporting a missing CLI as deregistered"
has "$HERMES_ADAPTER_UNINSTALL" "cannot be confirmed removed" \
  "hermes fails closed when the CLI is missing and the registration persists"
hasnt "$HERMES_ADAPTER_UNINSTALL" \
  'WARN: hermes CLI not found' \
  "hermes no longer downgrades a persisted registration to a warning"
has "$QODER_ADAPTER_UNINSTALL" "qoder_registration_present" \
  "qoder checks its own store when the CLI cannot be queried"
hasnt "$QODER_ADAPTER_UNINSTALL" "WARNING: qodercli not found, cannot unregister plugin" \
  "qoder no longer treats a missing CLI as a successful unregister"

echo "install-docs test passed"
