#!/usr/bin/env bash
# Run the candidate package and backend against a disposable Btrfs filesystem.
set -euo pipefail

if [[ "${1:-}" != --inside-test-namespace ]]; then
    if [[ $# != 3 || $EUID != 0 ]]; then
        echo "Usage: sudo bash $0 AW_BINARY PROVIDER_BUNDLE WS_CKPT_BINARY" >&2
        exit 2
    fi
    exec timeout --kill-after=15s 180s unshare --mount --propagation private \
        --pid --fork --mount-proc --kill-child=TERM bash \
        "$(realpath "$0")" --inside-test-namespace "$(readlink /proc/self/ns/mnt)" "$@"
fi
test "$EUID" = 0
test "$(readlink /proc/self/ns/mnt)" != "$2"
shift 2
aw=$(realpath "$1")
bundle=$(realpath "$2")
ws_ckpt=$(realpath "$3")
repo=$(cd "$(dirname "$0")/../../.." && pwd)
test -x "$aw"
test -f "$bundle/package.json"
test -x "$ws_ckpt"

root=$(mktemp -d /tmp/aw-checkpoint.XXXXXX)
loop=
daemon=
private_etc=false
private_state=false
cleanup() {
    local result=$?
    trap - EXIT
    if [[ "$result" != 0 && -f "$root/ws-ckpt.log" ]]; then cat "$root/ws-ckpt.log" >&2; fi
    if [[ -n "$daemon" ]]; then
        kill "$daemon" 2>/dev/null || true
        wait "$daemon" 2>/dev/null || true
    fi
    if [[ -n "$loop" ]]; then
        umount -R "$root/btrfs" 2>/dev/null || true
        losetup -d "$loop" || result=1
    fi
    if $private_state; then umount /var/lib || result=1; fi
    if $private_etc; then umount /etc || result=1; fi
    # Managed workspaces can be bind mounted. Check for mounts before deleting the fixture.
    if findmnt --raw --noheadings --output TARGET | grep -F "$root/" >/dev/null; then
        echo "Fixture still has mounts under $root; leaving its directory intact" >&2
        result=1
    else
        rm -rf -- "$root" || result=1
    fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The daemon uses fixed config and state paths. Keep both private to the fixture.
mkdir "$root/etc-upper" "$root/etc-work"
mount -t overlay overlay -o "lowerdir=/etc,upperdir=$root/etc-upper,workdir=$root/etc-work" /etc
private_etc=true
mount -t tmpfs -o mode=0755 tmpfs /var/lib
private_state=true
mkdir -p /etc/ws-ckpt "$root/btrfs" "$root/bin"

truncate -s 2G "$root/btrfs.img"
loop=$(losetup --find --show "$root/btrfs.img")
mkfs.btrfs -f "$loop" >/dev/null
mount -t btrfs "$loop" "$root/btrfs"
printf '[backend]\ntype = "btrfs-base"\n[backend.btrfs-base]\nbtrfs_mount = "%s"\n' \
    "$root/btrfs" > /etc/ws-ckpt/config.toml

uid=$(id -u nobody)
gid=$(id -g nobody)
chmod 0755 "$root"
install -d -m 0700 -o "$uid" -g "$gid" "$root/home" "$root/launch" "$root/workspace"
# The Provider resolves its policy path through the account database.
awk -F: -v OFS=: -v uid="$uid" -v home="$root/home" '$3 == uid { $6 = home; found = 1 } 1; END { if (!found) exit 1 }' \
    /etc/passwd > "$root/passwd"
install -m 0644 "$root/passwd" /etc/passwd
test "$(getent passwd "$uid" | cut -d: -f6)" = "$root/home"

install -m 0755 "$aw" "$root/bin/aw"
install -m 0755 "$ws_ckpt" "$root/bin/ws-ckpt"
install -m 0755 "$(dirname "$0")/qoder-fixture.py" "$root/bin/qoder-fixture"
cat > "$root/bin/traced-ws-ckpt" <<'SH'
#!/bin/sh
set -eu
if [ "$1" = checkpoint ]; then
    printf '%s\n' checkpoint >> "${0%/*}/../launch/checkpoint-calls"
fi
exec "${0%/*}/ws-ckpt" "$@"
SH
chmod 0755 "$root/bin/traced-ws-ckpt"
cp -R "$bundle" "$root/provider"
cp -R "$repo/src/ws-ckpt/src/skills/ws-ckpt/scripts" "$root/skill"

socket="$root/ws-ckpt.sock"
"$root/bin/ws-ckpt" daemon --socket "$socket" > "$root/ws-ckpt.log" 2>&1 &
daemon=$!
for _ in $(seq 1 60); do
    [[ ! -S "$socket" ]] || break
    kill -0 "$daemon"
    sleep .25
done
test -S "$socket"

cd "$root/launch"
env -i PATH="$root/bin:/usr/sbin:/usr/bin:/sbin:/bin" HOME="$root/home" \
    XDG_STATE_HOME="$root/home/state" WS_CKPT_SOCKET="$socket" \
    WS_CKPT_AW_TARGET=qoder \
    WS_CKPT_AW_PROVIDER="$root/home/prefix/libexec/aw/providers/ws-ckpt/aw-provider-ws-ckpt" \
    setpriv --reuid "$uid" --regid "$gid" --clear-groups --no-new-privs \
    python3 -B - "$root" < "$(dirname "$0")/btrfs-integration.py"
