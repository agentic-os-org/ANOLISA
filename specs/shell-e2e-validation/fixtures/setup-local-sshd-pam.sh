#!/usr/bin/env bash
# Adds a UsePAM sshd on port 22223, profile fixtures, and password accounts on
# top of setup-local-sshd.sh. Disposable test containers only.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bash "$here/setup-local-sshd.sh" "$1"

root=/tmp/cosh-li-sshd
port=22223
password='解释一下 ls -la "当前目录 (preview)"'

for account in coshli coshlicosh; do
    home="/home/$account"
    install -d -m 755 -o "$account" -g "$account" "$home/profile-bin"
    install -m 755 -o "$account" -g "$account" /dev/null "$home/profile-bin/cosh-li-profile-tool"
    printf '#!/bin/sh\n' >"$home/profile-bin/cosh-li-profile-tool"
    cat >"$home/.bash_profile" <<'EOF'
printf 'profile-hit\n' >> "$HOME/profile-hits"
export PATH="$HOME/profile-bin:$PATH"
PS1='cosh-li-login$ '
EOF
    chown "$account:$account" "$home/.bash_profile"
    rm -f "$home/profile-hits" "$home/core-path" "$home/core-child"
done
install -m 755 -o coshlicosh -g coshlicosh "$here/startup-path-core.sh" \
    /home/coshlicosh/profile-bin/cosh-core

if ! id -u coshlipw >/dev/null 2>&1; then
    useradd --create-home --shell /bin/bash coshlipw
fi
printf '%s:%s\n' coshlipw "$password" coshli "$password" | chpasswd
printf '%s\n' 'coshli ALL=(ALL) ALL' 'Defaults:coshli passwd_tries=1, !lecture' \
    >/etc/sudoers.d/cosh-li
chmod 440 /etc/sudoers.d/cosh-li

cat >"$root/sshd_pam_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $root/ssh_host_ed25519_key
PidFile $root/sshd-pam.pid
AuthorizedKeysFile .ssh/authorized_keys
UsePAM yes
PubkeyAuthentication yes
PasswordAuthentication yes
KbdInteractiveAuthentication no
PermitRootLogin no
PermitEmptyPasswords no
AllowUsers coshli coshlicosh coshlipw
AllowTcpForwarding no
X11Forwarding no
PrintMotd no
PrintLastLog no
UseDNS no
LogLevel ERROR
Subsystem sftp /usr/libexec/openssh/sftp-server
EOF
if [[ -s "$root/sshd-pam.pid" ]]; then
    kill "$(<"$root/sshd-pam.pid")" 2>/dev/null || true
    sleep 0.2
fi
rm -f "$root/sshd-pam.pid"
/usr/sbin/sshd -f "$root/sshd_pam_config" -E "$root/sshd-pam.log"

for _ in {1..50}; do
    if ssh-keyscan -T 1 -p "$port" 127.0.0.1 >"$root/known_hosts.pam" 2>/dev/null &&
        [[ -s "$root/known_hosts.pam" ]]; then
        break
    fi
    sleep 0.1
done
test -s "$root/known_hosts.pam"
cat "$root/known_hosts.pam" >>"$root/known_hosts"

for alias_account in "cosh-li-pam:coshli" "cosh-li-cosh-pam:coshlicosh"; do
    cat >>"$root/ssh_config" <<EOF
Host ${alias_account%%:*}
    HostName 127.0.0.1
    Port $port
    User ${alias_account#*:}
    IdentityFile $root/id_ed25519
    IdentitiesOnly yes
    IdentityAgent none
    UserKnownHostsFile $root/known_hosts
    StrictHostKeyChecking yes
    BatchMode yes
    PreferredAuthentications publickey
    ConnectTimeout 5
    LogLevel ERROR
EOF
done

# The password client runs as coshli, so it needs its own readable config.
install -d -m 700 -o coshli -g coshli /home/coshli/.ssh
install -m 600 -o coshli -g coshli "$root/known_hosts" /home/coshli/.ssh/li-known_hosts
cat >/home/coshli/.ssh/li-pw-config <<EOF
Host cosh-li-pw
    HostName 127.0.0.1
    Port $port
    User coshlipw
    PubkeyAuthentication no
    PreferredAuthentications password
    NumberOfPasswordPrompts 1
    UserKnownHostsFile /home/coshli/.ssh/li-known_hosts
    StrictHostKeyChecking yes
    ConnectTimeout 5
    LogLevel ERROR
EOF
chown coshli:coshli /home/coshli/.ssh/li-pw-config
chmod 600 /home/coshli/.ssh/li-pw-config
