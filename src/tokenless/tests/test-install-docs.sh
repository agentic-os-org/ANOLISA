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

has()    { grep -qF -- "$2" "$1" || fail "$3: expected to find '$2' in $(basename "$1")"; pass "$3"; }
has_re() { grep -qE -- "$2" "$1" || fail "$3: expected pattern '$2' in $(basename "$1")"; pass "$3"; }
hasnt()  { if grep -qF -- "$2" "$1"; then fail "$3: unexpected '$2' in $(basename "$1")"; fi; pass "$3"; }

# --- QUICKSTART: curl prerequisites must match the execution path -------------
hasnt "$DOC_EN_QUICKSTART" "One-liner, no prerequisites" "en QUICKSTART does not claim curl has no prerequisites"
hasnt "$DOC_ZH_QUICKSTART" "无需前置依赖" "zh QUICKSTART does not claim curl has no prerequisites"
has "$DOC_EN_QUICKSTART" "Node.js 16+" "en QUICKSTART states the npm-path prerequisite"
has "$DOC_EN_QUICKSTART" "Rust toolchain" "en QUICKSTART states the source-build prerequisite"
has "$DOC_ZH_QUICKSTART" "Node.js 16+" "zh QUICKSTART states the npm-path prerequisite"
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

for fw in openclaw hermes; do
  check_bundle "src/os-skills/component.toml" "$BUNDLE_COMPONENT" "$fw"
  check_bundle "manifests os-skills component.toml" "$BUNDLE_DISTRIBUTION" "$fw"
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
has "$BUNDLE_DISTRIBUTION" 'source = "{datadir}/skills/install-tokenless/"' \
  "the distribution bundle points at the flattened RPM skill path"
has "$OS_SKILLS_INDEX_EN" "**install-tokenless**" "en OS Skills index lists the new skill"
has "$OS_SKILLS_INDEX_ZH" "**install-tokenless**" "zh OS Skills index lists the new skill"

# --- installer script: the documented contracts are the implemented ones ------
has_re "$INSTALL_SH" '-maxdepth ([4-9]|[1-9][0-9]+) ' "install.sh searches deep enough for src/tokenless/Cargo.toml"
hasnt "$INSTALL_SH" "archive/refs/heads/main" "install.sh never downloads the main branch archive"
hasnt "$INSTALL_SH" "local tmpdir" "install.sh keeps the temp dir in a variable the EXIT trap can read"
has "$INSTALL_SH" "trap cleanup_src_tmpdir EXIT" "install.sh registers its cleanup trap at top level"
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
has "$INSTALL_SH" "retire_previous_npm_package" "install.sh retires the previous package before writing new files"
has "$INSTALL_SH" "remove_npm_package" "install.sh can retire a package whose prefix overlaps the install directory"
has "$INSTALL_SH" "drop_links_into" "install.sh drops recorded links before their target package goes"

# --- installer: a shared adapter directory is not claimed for existing -------
has "$INSTALL_SH" "shared_adapters_dir" "install.sh names the shared adapter directory once"
has "$INSTALL_SH" "adapters_owned_by_previous_receipt" \
  "install.sh proves adapter ownership before claiming the tree"
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

echo "install-docs test passed"
