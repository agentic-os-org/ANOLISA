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
INSTALL_SH="$TOKENLESS_ROOT/scripts/install.sh"
UNINSTALL_SH="$TOKENLESS_ROOT/scripts/uninstall.sh"

for f in "$DOC_EN_QUICKSTART" "$DOC_ZH_QUICKSTART" "$DOC_EN_MANUAL" "$DOC_ZH_MANUAL" \
         "$DOC_SKILL" "$DOC_EN_TROUBLE" "$DOC_ZH_TROUBLE" "$INSTALL_SH" "$UNINSTALL_SH"; do
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

# --- installer script: the documented contracts are the implemented ones ------
has_re "$INSTALL_SH" '-maxdepth ([4-9]|[1-9][0-9]+) ' "install.sh searches deep enough for src/tokenless/Cargo.toml"
hasnt "$INSTALL_SH" "archive/refs/heads/main" "install.sh never downloads the main branch archive"
hasnt "$INSTALL_SH" "local tmpdir" "install.sh keeps the temp dir in a variable the EXIT trap can read"
has "$INSTALL_SH" "trap cleanup_src_tmpdir EXIT" "install.sh registers its cleanup trap at top level"
has "$INSTALL_SH" "write_receipt" "install.sh records what it created"
has "$INSTALL_SH" "Windows is not supported" "install.sh rejects Windows as documented"
hasnt "$INSTALL_SH" "for bin in tokenless rtk toon" "install.sh does not link the retired toon binary"
has "$UNINSTALL_SH" "No install receipt found" "uninstall.sh refuses to guess without a receipt"

echo "install-docs test passed"
