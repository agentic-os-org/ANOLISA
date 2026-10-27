#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/../static/install.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/anolisa-installer-test.XXXXXX")"
TEST_ROOT="$(cd "$TEST_ROOT" && pwd -P)"
FAKE_BIN="${TEST_ROOT}/bin"
TEST_CLI_VERSION="0.0.0-test"
OLD_CLI_VERSION="0.0.0-old"
REAL_TAR="$(command -v tar)"
TOOL_DIR="${TEST_ROOT}/tools"
REQUIRED_TOOLS="
  awk bash cat chmod cp dirname env grep gzip install ln mkdir mv readlink rm sed
  sh stat tar tr
"
SYSTEM_PATH="$TOOL_DIR"
BASE_PATH="${FAKE_BIN}:${SYSTEM_PATH}"
LAST_STATUS=0

cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

fail() {
  echo "installer test failed: $*" >&2
  exit 1
}

mkdir -p "$FAKE_BIN" "$TOOL_DIR"
for tool in $REQUIRED_TOOLS; do
  found="$(command -v "$tool" 2>/dev/null)" ||
    fail "test tool '$tool' is unavailable on this host"
  ln -s "$found" "${TOOL_DIR}/${tool}"
done
if command -v sha256sum >/dev/null 2>&1; then
  ln -s "$(command -v sha256sum)" "${TOOL_DIR}/sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  ln -s "$(command -v shasum)" "${TOOL_DIR}/shasum"
else
  fail "neither sha256sum nor shasum is available on this host"
fi
ln -s "$(command -v mktemp)" "${TOOL_DIR}/mktemp"

cat >"${FAKE_BIN}/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      output="$2"
      shift 2
      ;;
    https://*)
      url="$1"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
printf '%s\n' "$url" >>"${ANOLISA_TEST_SHA_FILE}.urls"

if [ -z "$output" ]; then
  test -s "$ANOLISA_TEST_SHA_FILE"
  printf '%s  artifact\n' "$(cat "$ANOLISA_TEST_SHA_FILE")"
  exit 0
fi

if [ "$url" = "https://fixture.invalid/intel-cli.tar.gz" ]; then
  [ "${ANOLISA_TEST_DOWNLOAD_FAIL:-0}" != 1 ] || exit 22
  cp "${ANOLISA_TEST_SHA_FILE}.tar.gz" "$output"
  exit 0
fi

payload_dir="$(mktemp -d)"
trap 'rm -rf "$payload_dir"' EXIT
binary="${payload_dir}/anolisa"
if [ "${ANOLISA_TEST_LAYOUT:-legacy}" = packaged ]; then
  mkdir -p "$payload_dir/bin" "$payload_dir/share/anolisa/components"
  binary="$payload_dir/bin/anolisa"
  for component in cosh-ng sec-core agentsight tokenless skillfs ws-ckpt agent-memory; do
    mkdir -p "$payload_dir/share/anolisa/components/$component"
    printf '[component]\nname = "%s"\n' "$component" \
      >"$payload_dir/share/anolisa/components/$component/component.toml"
  done
  if [ "${ANOLISA_TEST_MISSING_CONTRACT:-0}" = 1 ]; then
    rm "$payload_dir/share/anolisa/components/ws-ckpt/component.toml"
  fi
  if [ "${ANOLISA_TEST_SYMLINK_SHARE:-0}" = 1 ]; then
    mv "$payload_dir/share" "$payload_dir/linked-share"
    ln -s linked-share "$payload_dir/share"
  fi
fi
cat >"$binary" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "--version" ]; then
  echo "anolisa ${ANOLISA_TEST_VERSION}"
  exit 0
fi
printf '%s\n' "$*" >>"$ANOLISA_TEST_LOG"
printf '%s\n' "${ANOLISA_DATA_DIR:-}" >"${ANOLISA_TEST_LOG}.data-dir"
printf '%s\n' "$0" >"${ANOLISA_TEST_LOG}.binary"
if [ "${ANOLISA_TEST_EXPECT_DATA:-0}" = 1 ]; then
  data_root="${ANOLISA_DATA_DIR:-$(cd "$(dirname "$0")/.." && pwd)/share/anolisa}"
  for component in cosh-ng sec-core agentsight tokenless skillfs ws-ckpt agent-memory; do
    test -f "$data_root/components/$component/component.toml"
    grep -Fq "name = \"$component\"" "$data_root/components/$component/component.toml"
  done
fi
SCRIPT
chmod +x "$binary"
"$ANOLISA_TEST_TAR" -czf "$output" -C "$payload_dir" .
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$output" | awk '{print $1}' >"$ANOLISA_TEST_SHA_FILE"
else
  shasum -a 256 "$output" | awk '{print $1}' >"$ANOLISA_TEST_SHA_FILE"
fi
if [ "${output##*/}" = release-manifest.toml ]; then
  cp "$output" "${ANOLISA_TEST_SHA_FILE}.tar.gz"
  digest="$(cat "$ANOLISA_TEST_SHA_FILE")"
  if [ "${ANOLISA_TEST_BAD_CHECKSUM:-0}" = 1 ]; then
    digest="$(printf '%064d' 0)"
  fi
  cat >"$output" <<MANIFEST
schema_version = 1
version = "$ANOLISA_TEST_VERSION"
[[artifacts]]
os = "macos"
arch = "${ANOLISA_TEST_MANIFEST_ARCH:-x86_64}"
url = "https://fixture.invalid/intel-cli.tar.gz"
sha256 = "$digest"
MANIFEST
fi
EOF

cat >"${FAKE_BIN}/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) echo "${ANOLISA_TEST_OS:-Darwin}" ;;
  -m) echo "${ANOLISA_TEST_ARCH:-arm64}" ;;
  *) exit 2 ;;
esac
EOF

cat >"${FAKE_BIN}/id" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -u) echo "${ANOLISA_TEST_EUID:-1000}" ;;
  *) exit 2 ;;
esac
EOF

cat >"${FAKE_BIN}/sudo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$ANOLISA_TEST_SUDO_LOG"
cat >"${ANOLISA_INSTALL_DIR}/anolisa" <<'SCRIPT'
#!/usr/bin/env bash
printf 'tampered CLI executed\n' >"$ANOLISA_TEST_TAMPER_LOG"
exit 99
SCRIPT
chmod +x "${ANOLISA_INSTALL_DIR}/anolisa"
if [ -f "$ANOLISA_TEST_PREFIX_DATADIR/components/ws-ckpt/component.toml" ]; then
  printf 'tampered user data\n' \
    >"$ANOLISA_TEST_PREFIX_DATADIR/components/ws-ckpt/component.toml"
fi
if [ "${ANOLISA_TEST_LATE_OLD_OWNER:-0}" = 1 ]; then
  export ANOLISA_TEST_RPM_SCOPE=system ANOLISA_TEST_RPM_OWNER=ws-ckpt
fi
exec "$@"
EOF

cat >"${FAKE_BIN}/rpm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${ANOLISA_TEST_RPM_LOG}"
path="${!#}"
case "${ANOLISA_TEST_RPM_SCOPE:-none}" in
  system) expected="$ANOLISA_TEST_PACKAGE_DATADIR" ;;
  prefix) expected="$ANOLISA_TEST_PREFIX_DATADIR" ;;
  *) exit 1 ;;
esac
if [ "$path" = "$expected/components/${ANOLISA_TEST_RPM_COMPONENT:-ws-ckpt}/component.toml" ]; then
  printf '%s\n' "${ANOLISA_TEST_RPM_OWNER:-ws-ckpt}"
else
  exit 1
fi
EOF

cat >"${FAKE_BIN}/stat" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
path="${!#}"
if [ "${ANOLISA_TEST_STAT_STYLE:-gnu}" = bsd ] && [ "$1" = -c ]; then
  exit 1
fi
if [ "${ANOLISA_TEST_STAT_STYLE:-gnu}" = unavailable ]; then
  exit 1
fi
if [ "${ANOLISA_TEST_BAD_DATA_ANCESTOR:-0}" = 1 ] &&
  [ "$path" = "${ANOLISA_TEST_SYSTEM_DATADIR%/share/anolisa}" ]; then
  printf '1000 755\n'
  exit 0
fi
if [ "$path" = "$ANOLISA_TEST_SYSTEM_DATADIR" ]; then
  printf '%s %s\n' "${ANOLISA_TEST_DATA_OWNER:-0}" "${ANOLISA_TEST_DATA_MODE:-755}"
else
  # The runner uses a temporary filesystem in this fixture, not real root data.
  printf '0 755\n'
fi
EOF

chmod +x "${FAKE_BIN}/curl" "${FAKE_BIN}/uname" "${FAKE_BIN}/id" \
  "${FAKE_BIN}/sudo" "${FAKE_BIN}/rpm" "${FAKE_BIN}/stat"

make_cli() {
  local path="$1" version="$2"
  mkdir -p "$(dirname "$path")"
  cat >"$path" <<EOF
#!/usr/bin/env bash
echo "anolisa ${version}"
EOF
  chmod +x "$path"
}

make_npm_cli() {
  local prefix="$1" version="$2"
  local real_bin="${prefix}/lib/node_modules/@anolisa/cli/bin"
  mkdir -p "$real_bin" "${prefix}/bin"
  make_cli "${real_bin}/anolisa" "$version"
  ln -s "${real_bin}/anolisa" "${prefix}/bin/anolisa"
}

show_output() {
  local case_root="${TEST_ROOT}/$1"
  sed "s|^|[$1 stdout] |" "${case_root}/stdout" >&2 ||:
  sed "s|^|[$1 stderr] |" "${case_root}/stderr" >&2 ||:
}

assert_contains() {
  local file="$1" needle="$2" name="$3"
  if ! grep -Fq -- "$needle" "$file"; then
    echo "case '$name' output does not contain '$needle'" >&2
    show_output "$name"
    exit 1
  fi
}

assert_not_contains() {
  local file="$1" needle="$2" name="$3"
  if grep -Fq -- "$needle" "$file"; then
    echo "case '$name' output unexpectedly contains '$needle'" >&2
    show_output "$name"
    exit 1
  fi
}

run_install_case() {
  local name="$1" case_path="$2"
  shift 2

  local case_root="${TEST_ROOT}/${name}"
  local install_dir="${case_root}/install"
  local case_installer="${case_root}/install.sh"
  local system_datadir="${case_root}/system/share/anolisa"
  if [ "${ANOLISA_TEST_OS:-Darwin}" = Darwin ]; then
    system_datadir="${case_root}/system-local/share/anolisa"
  fi
  mkdir -p "$case_root"
  # Exercise the privileged runner, but redirect every system write and every
  # trusted-path tool lookup in this test-only copy. Never write /usr/share.
  sed -e "s|/usr/share/anolisa|${case_root}/system/share/anolisa|g" \
    -e "s|/usr/local/share/anolisa|${case_root}/system-local/share/anolisa|g" \
    -e "s|PATH=/usr/sbin:/usr/bin:/sbin:/bin|PATH=${BASE_PATH}|g" \
    "$INSTALLER" >"$case_installer"
  LAST_STATUS=0
  PATH="$case_path" \
    ANOLISA_INSTALL_DIR="$install_dir" \
    ANOLISA_TEST_LOG="${case_root}/commands.log" \
    ANOLISA_TEST_SUDO_LOG="${case_root}/sudo.log" \
    ANOLISA_TEST_TAMPER_LOG="${case_root}/tampered.log" \
    ANOLISA_TEST_SHA_FILE="${case_root}/artifact.sha256" \
    ANOLISA_TEST_RPM_LOG="${case_root}/rpm.log" \
    ANOLISA_TEST_SYSTEM_DATADIR="$system_datadir" \
    ANOLISA_TEST_PACKAGE_DATADIR="${case_root}/system/share/anolisa" \
    ANOLISA_TEST_PREFIX_DATADIR="${case_root}/share/anolisa" \
    ANOLISA_TEST_TAR="$REAL_TAR" \
    ANOLISA_TEST_VERSION="$TEST_CLI_VERSION" \
    ANOLISA_VERSION="${ANOLISA_TEST_REQUEST_VERSION:-$TEST_CLI_VERSION}" \
    bash -s -- "$@" <"$case_installer" >"${case_root}/stdout" \
      2>"${case_root}/stderr" || LAST_STATUS=$?
}

run_case() {
  local name="$1"
  local expected="$2"
  shift 2

  local case_root="${TEST_ROOT}/${name}"
  local command_log="${case_root}/commands.log"
  run_install_case "$name" "$BASE_PATH" "$@"

  if [ "$LAST_STATUS" -ne 0 ]; then
    echo "case '$name' exited with status $LAST_STATUS" >&2
    show_output "$name"
    exit 1
  fi

  local actual=""
  if [ -f "$command_log" ]; then
    actual="$(cat "$command_log")"
  fi
  if [ "$actual" != "$expected" ]; then
    echo "case '$name' invoked '$actual', expected '$expected'" >&2
    exit 1
  fi
  if [ -e "${case_root}/tampered.log" ]; then
    echo "case '$name' executed a replaced user-local CLI" >&2
    exit 1
  fi
}

run_case cli-only ""
assert_contains "${TEST_ROOT}/cli-only/artifact.sha256.urls" \
  "/artifacts/macos/aarch64/anolisa-cli-${TEST_CLI_VERSION}-aarch64-apple-darwin.tar.gz" cli-only
run_case help "" --help
grep -Fq -- "--component NAME" "${TEST_ROOT}/help/stdout"
grep -Fq -- "--backend BACKEND" "${TEST_ROOT}/help/stdout"
grep -Fq -- "system uses sudo when needed" "${TEST_ROOT}/help/stdout"
run_case install-component-user \
  "--install-mode user install tokenless --backend raw" \
  --component tokenless --install-mode user
run_case install-component-equals "install agent-memory --backend raw" --component=agent-memory
run_case install-cosh-ng-alias "install cosh-ng --backend raw" --cosh-ng
run_case install-cosh-ng-system \
  "--install-mode system install cosh-ng --backend raw" \
  --cosh-ng --install-mode=system
test -s "${TEST_ROOT}/install-cosh-ng-system/sudo.log"
run_case install-cosh-ng-rpm \
  "--install-mode system install cosh-ng --backend rpm" \
  --cosh-ng --backend=rpm --install-mode=system
run_case upgrade-component \
  "--install-mode system update cosh-ng" \
  --component cosh-ng --install-mode system --upgrade
run_case uninstall-component \
  "--install-mode system uninstall cosh-ng" \
  --uninstall --component cosh-ng --install-mode system
test ! -e "${TEST_ROOT}/install-cosh-ng-system/system/share/anolisa"
test -z "$(cat "${TEST_ROOT}/install-cosh-ng-system/commands.log.data-dir")"

export ANOLISA_TEST_OS=Linux
ANOLISA_TEST_LAYOUT=packaged run_case packaged-cli ""
for component in cosh-ng sec-core agentsight tokenless skillfs ws-ckpt agent-memory; do
  test -f "${TEST_ROOT}/packaged-cli/share/anolisa/components/$component/component.toml"
done
test ! -e "${TEST_ROOT}/packaged-cli/system/share/anolisa"
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_EXPECT_DATA=1 \
  run_case packaged-user "--install-mode user install ws-ckpt --backend raw" \
    --component ws-ckpt --install-mode user

for euid in 1000 0; do
  name="packaged-system-$euid"
  ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_EXPECT_DATA=1 ANOLISA_TEST_EUID="$euid" \
    ANOLISA_DATA_DIR="${TEST_ROOT}/untrusted-user-data" \
    run_case "$name" "--install-mode system install ws-ckpt --backend raw" \
      --component ws-ckpt --install-mode system
  test "$(cat "${TEST_ROOT}/$name/commands.log.data-dir")" = \
    "${TEST_ROOT}/$name/system/share/anolisa"
  temporary_binary="$(cat "${TEST_ROOT}/$name/commands.log.binary")"
  test ! -e "$temporary_binary" || fail "temporary privileged CLI was retained"
  test -f "${TEST_ROOT}/$name/system/share/anolisa/components/ws-ckpt/component.toml"
done

name=packaged-incomplete
make_cli "${TEST_ROOT}/$name/install/anolisa" "$OLD_CLI_VERSION"
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_MISSING_CONTRACT=1 \
  run_install_case "$name" "$BASE_PATH"
test "$LAST_STATUS" -ne 0 || fail "incomplete integration data was accepted"
assert_contains "${TEST_ROOT}/$name/stderr" "integration contract missing or unsafe" "$name"
test "$("${TEST_ROOT}/$name/install/anolisa" --version)" = "anolisa $OLD_CLI_VERSION"
test ! -e "${TEST_ROOT}/$name/share/anolisa"

name=packaged-symlink-share
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_SYMLINK_SHARE=1 \
  run_install_case "$name" "$BASE_PATH"
test "$LAST_STATUS" -ne 0 || fail "symlinked archive data was accepted"
assert_contains "${TEST_ROOT}/$name/stderr" "invalid CLI integration data directory" "$name"
test ! -e "${TEST_ROOT}/$name/share/anolisa"

# Both the write destination and the system registry (which takes runtime
# precedence) must be free of every former component RPM owner before writing.
for scope in prefix system; do
  for owner in cosh-ng agent-sec-cli agentsight tokenless skillfs ws-ckpt agent-memory; do
    component="$owner"
    [ "$owner" != agent-sec-cli ] || component=sec-core
    name="packaged-old-owner-$scope-$owner"
    case_root="${TEST_ROOT}/$name"
    if [ "$scope" = prefix ]; then
      datadir="$case_root/share/anolisa"
    else
      datadir="$case_root/system/share/anolisa"
    fi
    mkdir -p "$datadir/components/$component"
    printf 'old package contract\n' >"$datadir/components/$component/component.toml"
    make_cli "$case_root/install/anolisa" "$OLD_CLI_VERSION"
    ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_RPM_SCOPE="$scope" \
      ANOLISA_TEST_RPM_OWNER="$owner" ANOLISA_TEST_RPM_COMPONENT="$component" \
      run_install_case "$name" "$BASE_PATH" --component ws-ckpt --install-mode system
    test "$LAST_STATUS" -ne 0 || fail "old owner $owner at $scope was accepted"
    assert_contains "$case_root/stderr" "still owned by component RPM $owner" "$name"
    assert_contains "$case_root/stderr" "native package manager" "$name"
    test "$(cat "$datadir/components/$component/component.toml")" = 'old package contract'
    test "$("$case_root/install/anolisa" --version)" = "anolisa $OLD_CLI_VERSION"
    test ! -e "$case_root/sudo.log"
    test ! -e "$case_root/commands.log"
  done
done

name=packaged-late-old-owner
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_LATE_OLD_OWNER=1 \
  run_install_case "$name" "$BASE_PATH" --component ws-ckpt --install-mode system
test "$LAST_STATUS" -ne 0 || fail "privileged old-owner preflight was bypassed"
assert_contains "${TEST_ROOT}/$name/stderr" \
  "still owned by component RPM ws-ckpt" "$name"
test -s "${TEST_ROOT}/$name/sudo.log"
test ! -e "${TEST_ROOT}/$name/system/share/anolisa"
test ! -e "${TEST_ROOT}/$name/commands.log"

name=packaged-anolisa-owner
mkdir -p "${TEST_ROOT}/$name/system/share/anolisa/components/ws-ckpt"
printf 'old ANOLISA contract\n' \
  >"${TEST_ROOT}/$name/system/share/anolisa/components/ws-ckpt/component.toml"
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_EXPECT_DATA=1 \
  ANOLISA_TEST_RPM_SCOPE=system ANOLISA_TEST_RPM_OWNER=anolisa \
  run_case "$name" "--install-mode system update ws-ckpt" \
    --component ws-ckpt --install-mode system --upgrade

ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_EXPECT_DATA=1 ANOLISA_TEST_STAT_STYLE=bsd \
  run_case packaged-bsd-stat "--install-mode system install ws-ckpt --backend raw" \
    --component ws-ckpt --install-mode system
name=packaged-stat-unavailable
ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_STAT_STYLE=unavailable \
  run_install_case "$name" "$BASE_PATH" --component ws-ckpt --install-mode system
test "$LAST_STATUS" -ne 0 || fail "unverifiable root data ownership was accepted"
assert_contains "${TEST_ROOT}/$name/stderr" "cannot verify integration data ownership" "$name"
test ! -e "${TEST_ROOT}/$name/system/share/anolisa"
test ! -e "${TEST_ROOT}/$name/commands.log"

for failure in owner mode symlink ancestor-symlink; do
  name="packaged-unsafe-system-$failure"
  case_root="${TEST_ROOT}/$name"
  datadir="$case_root/system/share/anolisa"
  mkdir -p "$datadir/components/ws-ckpt"
  printf 'existing contract\n' >"$datadir/components/ws-ckpt/component.toml"
  case "$failure" in
    owner) export ANOLISA_TEST_DATA_OWNER=1000 ;;
    mode) export ANOLISA_TEST_DATA_MODE=777 ;;
    symlink)
      mv "$datadir" "$case_root/outside"
      ln -s "$case_root/outside" "$datadir"
      ;;
    ancestor-symlink)
      mv "$case_root/system/share" "$case_root/outside"
      ln -s "$case_root/outside" "$case_root/system/share"
      ;;
  esac
  ANOLISA_TEST_LAYOUT=packaged \
    run_install_case "$name" "$BASE_PATH" --component ws-ckpt --install-mode system
  test "$LAST_STATUS" -ne 0 || fail "unsafe system data $failure was accepted"
  if [ "$failure" = owner ] || [ "$failure" = mode ]; then
    assert_contains "$case_root/stderr" "must be root-owned" "$name"
  else
    assert_contains "$case_root/stderr" "must not be a symlink" "$name"
  fi
  test "$(cat "$datadir/components/ws-ckpt/component.toml")" = 'existing contract'
  test ! -e "$case_root/commands.log"
  unset ANOLISA_TEST_DATA_OWNER ANOLISA_TEST_DATA_MODE
done

name=packaged-darwin-system
ANOLISA_TEST_OS=Darwin ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_EXPECT_DATA=1 \
  run_case "$name" "--install-mode system install tokenless --backend raw" \
    --component tokenless --install-mode system
test "$(cat "${TEST_ROOT}/$name/commands.log.data-dir")" = \
  "${TEST_ROOT}/$name/system-local/share/anolisa"
test -f "${TEST_ROOT}/$name/system-local/share/anolisa/components/tokenless/component.toml"
test ! -e "${TEST_ROOT}/$name/system/share/anolisa"
name=packaged-darwin-user-owned-prefix
mkdir -p "${TEST_ROOT}/$name/system-local"
ANOLISA_TEST_OS=Darwin ANOLISA_TEST_LAYOUT=packaged ANOLISA_TEST_BAD_DATA_ANCESTOR=1 \
  run_install_case "$name" "$BASE_PATH" --component tokenless --install-mode system
test "$LAST_STATUS" -ne 0 || fail "user-owned Darwin system data ancestor was accepted"
assert_contains "${TEST_ROOT}/$name/stderr" "must be root-owned" "$name"
test ! -e "${TEST_ROOT}/$name/system-local/share/anolisa"
test ! -e "${TEST_ROOT}/$name/commands.log"
unset ANOLISA_TEST_OS

ANOLISA_TEST_ARCH=x86_64 run_case intel-pinned \
  "--install-mode user install cosh-ng --backend raw" \
  --cosh-ng --backend raw --install-mode user
assert_contains "${TEST_ROOT}/intel-pinned/artifact.sha256.urls" \
  "/artifacts/macos/x86_64/anolisa-cli-${TEST_CLI_VERSION}-x86_64-apple-darwin.tar.gz" intel-pinned
ANOLISA_TEST_ARCH=x86_64 ANOLISA_TEST_REQUEST_VERSION=stable \
  run_case intel-stable "--install-mode user install cosh-ng --backend raw" \
  --cosh-ng --backend raw --install-mode user
assert_contains "${TEST_ROOT}/intel-stable/artifact.sha256.urls" \
  'https://fixture.invalid/intel-cli.tar.gz' intel-stable
test ! -e "${TEST_ROOT}/intel-stable/sudo.log"
ANOLISA_TEST_OS=Linux ANOLISA_TEST_ARCH=x86_64 \
  run_case linux-x64 "install cosh-ng --backend raw" --cosh-ng
assert_contains "${TEST_ROOT}/linux-x64/artifact.sha256.urls" \
  "/artifacts/linux/x86_64/anolisa-cli-${TEST_CLI_VERSION}-x86_64-unknown-linux-gnu.tar.gz" linux-x64

for failure in missing-platform checksum download; do
  name="intel-$failure"
  existing="${TEST_ROOT}/${name}/install/anolisa"
  make_cli "$existing" "$OLD_CLI_VERSION"
  case "$failure" in
    missing-platform)
      export ANOLISA_TEST_MANIFEST_ARCH=aarch64
      expected='release manifest has no artifact for macos/x86_64'
      ;;
    checksum)
      export ANOLISA_TEST_BAD_CHECKSUM=1
      expected='sha256 mismatch'
      ;;
    download)
      export ANOLISA_TEST_DOWNLOAD_FAIL=1
      expected='download failed'
      ;;
  esac
  ANOLISA_TEST_ARCH=x86_64 ANOLISA_TEST_REQUEST_VERSION=stable \
    run_install_case "$name" "$BASE_PATH" \
      --cosh-ng --backend raw --install-mode user
  test "$LAST_STATUS" -ne 0 || fail "$name unexpectedly succeeded"
  assert_contains "${TEST_ROOT}/${name}/stderr" "$expected" "$name"
  test "$("$existing" --version)" = "anolisa $OLD_CLI_VERSION"
  test ! -e "${TEST_ROOT}/${name}/commands.log"
  if [ "$failure" = missing-platform ]; then
    assert_not_contains "${TEST_ROOT}/${name}/artifact.sha256.urls" \
      'https://fixture.invalid/intel-cli.tar.gz' "$name"
  fi
  unset ANOLISA_TEST_MANIFEST_ARCH ANOLISA_TEST_BAD_CHECKSUM ANOLISA_TEST_DOWNLOAD_FAIL
done

assert_shadow_reported() {
  local name="$1" active_path="$2" active_version="$3"
  local case_root="${TEST_ROOT}/${name}"
  local installed="${case_root}/install/anolisa"

  if [ "$LAST_STATUS" -eq 0 ]; then
    echo "case '$name' reported success while the CLI was shadowed" >&2
    show_output "$name"
    exit 1
  fi
  assert_contains "${case_root}/stderr" \
    "installed: ${installed} (anolisa ${TEST_CLI_VERSION})" "$name"
  assert_contains "${case_root}/stderr" \
    "active:    ${active_path} (anolisa ${active_version})" "$name"
  assert_contains "${case_root}/stderr" \
    "export PATH=\"${installed%/*}:\$PATH\"" "$name"
  assert_not_contains "${case_root}/stdout" "done" "$name"
}

fresh_shell_path() {
  env -i HOME="$TEST_ROOT" PATH="$1" bash -c \
    'hash -r 2>/dev/null ||:; command -v anolisa'
}

fresh_shell_version() {
  env -i HOME="$TEST_ROOT" PATH="$1" \
    ANOLISA_TEST_VERSION="$TEST_CLI_VERSION" bash -c 'anolisa --version'
}

NPM_OLD="${TEST_ROOT}/npm-old"
HOMEBREW_OLD="${TEST_ROOT}/homebrew-old"
make_npm_cli "$NPM_OLD" "$OLD_CLI_VERSION"
make_cli "${HOMEBREW_OLD}/Cellar/anolisa/${OLD_CLI_VERSION}/bin/anolisa" \
  "$OLD_CLI_VERSION"
mkdir -p "${HOMEBREW_OLD}/bin"
ln -s "${HOMEBREW_OLD}/Cellar/anolisa/${OLD_CLI_VERSION}/bin/anolisa" \
  "${HOMEBREW_OLD}/bin/anolisa"

name=shadowed-by-npm
run_install_case "$name" \
  "${FAKE_BIN}:${NPM_OLD}/bin:${TEST_ROOT}/${name}/install:${SYSTEM_PATH}"
assert_shadow_reported "$name" "${NPM_OLD}/bin/anolisa" "$OLD_CLI_VERSION"
assert_contains "${TEST_ROOT}/${name}/stderr" \
  "npm uninstall -g @anolisa/cli" "$name"
if [ "$(PATH="${NPM_OLD}/bin:${SYSTEM_PATH}" anolisa --version)" != \
     "anolisa ${OLD_CLI_VERSION}" ]; then
  fail "npm-owned CLI was modified"
fi

name=shadowed-by-homebrew
run_install_case "$name" \
  "${FAKE_BIN}:${HOMEBREW_OLD}/bin:${TEST_ROOT}/${name}/install:${SYSTEM_PATH}"
assert_shadow_reported "$name" "${HOMEBREW_OLD}/bin/anolisa" \
  "$OLD_CLI_VERSION"
assert_contains "${TEST_ROOT}/${name}/stderr" "brew uninstall anolisa" "$name"

name=shadowed-with-noisy-version
noisy_cli="${TEST_ROOT}/${name}/old/anolisa"
make_cli "$noisy_cli" \
  "$OLD_CLI_VERSION"$'\033[31m\033[0m\r\t\b\a\177\nUNEXPECTED_VERSION_TAIL'
run_install_case "$name" \
  "${FAKE_BIN}:${noisy_cli%/*}:${TEST_ROOT}/${name}/install:${SYSTEM_PATH}"
assert_shadow_reported "$name" "$noisy_cli" "$OLD_CLI_VERSION"
grep -Fxq "    active:    ${noisy_cli} (anolisa ${OLD_CLI_VERSION})" \
  "${TEST_ROOT}/${name}/stderr" || fail "version report contains control bytes"
assert_not_contains "${TEST_ROOT}/${name}/stderr" "UNEXPECTED_VERSION_TAIL" "$name"

name=install-dir-first
install_dir="${TEST_ROOT}/${name}/install"
run_install_case "$name" \
  "${FAKE_BIN}:${install_dir}:${NPM_OLD}/bin:${SYSTEM_PATH}"
if [ "$LAST_STATUS" -ne 0 ]; then
  echo "case '$name' rejected the active standalone CLI" >&2
  show_output "$name"
  exit 1
fi
assert_contains "${TEST_ROOT}/${name}/stdout" "done" "$name"
if [ "$(fresh_shell_path "${FAKE_BIN}:${install_dir}:${NPM_OLD}/bin:${SYSTEM_PATH}")" != \
     "${install_dir}/anolisa" ]; then
  fail "fresh shell did not resolve the standalone CLI"
fi
if [ "$(fresh_shell_version "${FAKE_BIN}:${install_dir}:${NPM_OLD}/bin:${SYSTEM_PATH}")" != \
     "anolisa ${TEST_CLI_VERSION}" ]; then
  fail "fresh shell did not run the installed version"
fi
if [ "$(fresh_shell_path "${FAKE_BIN}:${NPM_OLD}/bin:${install_dir}:${SYSTEM_PATH}")" != \
     "${NPM_OLD}/bin/anolisa" ]; then
  fail "fresh shell did not resolve the shadowing CLI"
fi

name=reinstall-active
active_install="${TEST_ROOT}/${name}/install"
run_install_case "$name" "${FAKE_BIN}:${active_install}:${SYSTEM_PATH}"
[ "$LAST_STATUS" -eq 0 ] || fail "initial standalone install failed"
run_install_case "$name" "${FAKE_BIN}:${active_install}:${SYSTEM_PATH}"
[ "$LAST_STATUS" -eq 0 ] || fail "reinstall reported a false conflict"

name=install-dir-missing-from-path
run_install_case "$name" "${FAKE_BIN}:${SYSTEM_PATH}"
if [ "$LAST_STATUS" -ne 0 ]; then
  echo "case '$name' failed without a competing CLI" >&2
  show_output "$name"
  exit 1
fi
assert_contains "${TEST_ROOT}/${name}/stderr" "is not in your PATH" "$name"
assert_contains "${TEST_ROOT}/${name}/stdout" "done" "$name"

name=same-directory-two-names
real_install="${TEST_ROOT}/${name}/real-install"
mkdir -p "$real_install"
ln -s "$real_install" "${TEST_ROOT}/${name}/install"
run_install_case "$name" "${FAKE_BIN}:${real_install}:${SYSTEM_PATH}"
if [ "$LAST_STATUS" -ne 0 ]; then
  echo "case '$name' reported a false conflict for a symlinked directory" >&2
  show_output "$name"
  exit 1
fi

name=same-directory-different-case
install_dir="${TEST_ROOT}/${name}/install"
mkdir -p "$install_dir"
if [ -d "${TEST_ROOT}/${name}/INSTALL" ]; then
  run_install_case "$name" \
    "${FAKE_BIN}:${TEST_ROOT}/${name}/INSTALL:${SYSTEM_PATH}"
  [ "$LAST_STATUS" -eq 0 ] || fail "case-only path difference reported a conflict"
  assert_contains "${TEST_ROOT}/${name}/stdout" "done" "$name"
  assert_not_contains "${TEST_ROOT}/${name}/stderr" "shadowed" "$name"
else
  echo "skipping case-only path test on a case-sensitive filesystem"
fi

expect_rejected() {
  local name="$1"
  local expected_error="$2"
  shift 2

  local case_root="${TEST_ROOT}/${name}"
  run_install_case "$name" "$BASE_PATH" "$@"
  if [ "$LAST_STATUS" -eq 0 ]; then
    echo "case '$name' unexpectedly succeeded" >&2
    exit 1
  fi

  if ! grep -Fq -- "$expected_error" "${case_root}/stderr"; then
    echo "case '$name' did not report '$expected_error'" >&2
    cat "${case_root}/stderr" >&2
    exit 1
  fi
}

expect_rejected conflicting-actions \
  "--upgrade and --uninstall cannot be used together" \
  --component cosh-ng --upgrade --uninstall
expect_rejected action-without-component \
  "--upgrade and --uninstall require --component NAME" \
  --upgrade
expect_rejected missing-component-name \
  "--component requires a component name" \
  --component
expect_rejected invalid-component-name \
  "invalid component name: invalid/name" \
  --component invalid/name
expect_rejected duplicate-component \
  "only one component can be selected" \
  --component cosh-ng --component tokenless
expect_rejected missing-install-mode \
  "--install-mode requires user or system" \
  --component cosh-ng --install-mode
expect_rejected invalid-install-mode \
  "invalid install mode: invalid (expected user or system)" \
  --component cosh-ng --install-mode invalid
expect_rejected install-mode-without-component \
  "--install-mode requires --component NAME" \
  --install-mode user
expect_rejected missing-backend-name \
  "--backend requires a backend name" \
  --component cosh-ng --backend
expect_rejected invalid-backend-name \
  "invalid backend: invalid/name" \
  --component cosh-ng --backend invalid/name
expect_rejected duplicate-backend \
  "only one component backend can be selected" \
  --component cosh-ng --backend raw --backend rpm
expect_rejected backend-without-component \
  "--backend requires --component NAME" \
  --backend rpm
expect_rejected backend-with-action \
  "--backend is only valid when installing a component" \
  --component cosh-ng --backend rpm --upgrade
expect_rejected unknown-argument \
  "unknown argument: --unknown" \
  --unknown

echo "installer tests passed"
