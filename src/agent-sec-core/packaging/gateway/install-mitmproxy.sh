#!/bin/sh
# Installs the pinned mitmproxy standalone build for the secret gateway and
# creates the agent-sec-gw system account.
#
# Usage: install-mitmproxy.sh [--archive PATH]
#   --archive PATH  use an already downloaded release archive instead of
#                   downloading it from downloads.mitmproxy.org
# The archive is verified against the SHA-256 pinned in mitmproxy.provenance.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
. "$here/mitmproxy.provenance"
root=/opt/agent-sec-gateway
user=agent-sec-gw

archive=
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) archive=${2:?--archive needs a path}; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
case "$(uname -m)" in
    x86_64) url=$MITMPROXY_URL_X86_64; sha256=$MITMPROXY_SHA256_X86_64 ;;
    aarch64) url=$MITMPROXY_URL_AARCH64; sha256=$MITMPROXY_SHA256_AARCH64 ;;
    *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [ -z "$archive" ]; then
    archive=$work/mitmproxy.tar.gz
    curl --fail --silent --show-error --location --proto '=https' -o "$archive" "$url"
fi
echo "$sha256  $archive" | sha256sum --check --quiet - || {
    echo "mitmproxy archive does not match the pinned SHA-256" >&2
    exit 1
}

target=$root/$MITMPROXY_VERSION
install -d -m 0755 -o root -g root "$root"
rm -rf "$target.tmp"
install -d -m 0755 -o root -g root "$target.tmp"
tar -xzf "$archive" -C "$target.tmp" --no-same-owner mitmdump
chown -R root:root "$target.tmp"
chmod 0755 "$target.tmp/mitmdump"
rm -rf "$target"
mv "$target.tmp" "$target"
ln -sfn "$MITMPROXY_VERSION" "$root/current"

if ! id "$user" >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /var/lib/agent-sec-gateway \
        --shell /sbin/nologin --user-group "$user"
fi
# The gateway's group can read the real keys, so it must belong to the account alone.
members=$(getent group "$user" | cut -d: -f4)
if [ "$(id -gn "$user")" != "$user" ] || [ -n "$members" ]; then
    echo "$user must have a dedicated primary group named $user without members" >&2
    exit 1
fi
echo "mitmproxy $MITMPROXY_VERSION installed in $target"
