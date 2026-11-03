#!/usr/bin/env bash
# ws-ckpt raw pre_uninstall hook.
#
# anolisa currently executes pre_uninstall before stopping/removing service
# files. Keep this best-effort: recover workspaces while ws-ckpt still exists,
# then confirm the daemon stopped before detaching btrfs runtime resources.
# Keep images and state: another request can arrive between recovery and stop.
set -u

warn() {
    echo "WARNING: $*" >&2
}

ws_ckpt=""
recover_failed=0
for c in "$(command -v ws-ckpt 2>/dev/null || true)" \
         /usr/local/bin/ws-ckpt /usr/bin/ws-ckpt; do
    if [ -n "$c" ] && [ -x "$c" ]; then
        ws_ckpt="$c"
        break
    fi
done

if [ -n "$ws_ckpt" ]; then
    echo "ws-ckpt pre-uninstall: recovering all workspaces with '$ws_ckpt'..."
    if ! "$ws_ckpt" recover --all --force 2>&1; then
        recover_failed=1
        warn "recover workspaces failed; preserving ws-ckpt data for manual recovery"
    fi
else
    recover_failed=1
    warn "ws-ckpt binary not found; preserving ws-ckpt data for manual recovery"
fi

if ! command -v systemctl >/dev/null 2>&1; then
    warn "cannot confirm daemon stopped; preserving ws-ckpt mounts and data"
    exit 0
fi
echo "ws-ckpt pre-uninstall: stopping ws-ckpt.service..."
if ! systemctl stop ws-ckpt.service >/dev/null 2>&1; then
    warn "stop ws-ckpt.service failed; preserving ws-ckpt mounts and data"
    exit 0
fi
if [ "$(systemctl show ws-ckpt.service --property=ActiveState --value 2>/dev/null)" != inactive ]; then
    warn "cannot confirm daemon stopped; preserving ws-ckpt mounts and data"
    exit 0
fi
if [ "$recover_failed" -ne 0 ]; then
    exit 0
fi

unmount_target() {
    target="$1"
    [ -n "$target" ] || return 0
    if command -v mountpoint >/dev/null 2>&1 && ! mountpoint -q "$target"; then
        return 0
    fi
    echo "ws-ckpt pre-uninstall: unmounting $target..."
    umount "$target" >/dev/null 2>&1 || umount -l "$target" >/dev/null 2>&1 || warn "unmount $target failed"
}

if command -v findmnt >/dev/null 2>&1; then
    findmnt -rn -t btrfs -o TARGET 2>/dev/null \
        | awk '$1 ~ "^/mnt/btrfs-workspace($|/)" { print $1 }' \
        | sort -r \
        | while IFS= read -r target; do
            unmount_target "$target"
          done
else
    unmount_target /mnt/btrfs-workspace
fi

if command -v losetup >/dev/null 2>&1; then
    losetup -a 2>/dev/null \
        | while IFS= read -r line; do
            case "$line" in
                *"(/var/lib/ws-ckpt/"*.img")"*|*"(/data/ws-ckpt/"*.img")"*)
                    dev="${line%%:*}"
                    echo "ws-ckpt pre-uninstall: detaching $dev..."
                    losetup -d "$dev" >/dev/null 2>&1 || warn "detach $dev failed"
                    ;;
            esac
          done
fi

warn "preserving ws-ckpt images and state for manual recovery or cleanup"
rmdir /mnt/btrfs-workspace 2>/dev/null || true

exit 0
