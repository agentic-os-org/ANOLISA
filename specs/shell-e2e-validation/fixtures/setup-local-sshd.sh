#!/usr/bin/env bash
set -euo pipefail

root=/tmp/cosh-li-sshd
port=22222
cosh_binary=${1:-}
bash_account=coshli
cosh_account=coshlicosh
noisy_account=coshlinoisy

for tool in sshd rsync ssh scp sftp ssh-keyscan; do
    if ! command -v "$tool" >/dev/null; then
        dnf -y -q install openssh-server openssh-clients rsync >/dev/null
        break
    fi
done
sftp_server=/usr/libexec/openssh/sftp-server
test -x "$sftp_server"

accounts=("$bash_account")
if [[ -n "$cosh_binary" ]]; then
    install -d /usr/local/libexec/cosh-li
    install -m 755 "$cosh_binary" /usr/local/libexec/cosh-li/cosh-shell
    ln -sfn /usr/local/libexec/cosh-li/cosh-shell /usr/local/bin/cosh
    cat >/usr/local/bin/cosh-li-noisy-shell <<'EOF'
#!/bin/bash
printf '__L01_NOISY_SHELL_BANNER__\n'
exec /bin/bash "$@"
EOF
    chmod 755 /usr/local/bin/cosh-li-noisy-shell
    accounts+=("$cosh_account" "$noisy_account")
fi

login_shell_for() {
    case $1 in
        "$cosh_account") printf '/usr/local/bin/cosh\n' ;;
        "$noisy_account") printf '/usr/local/bin/cosh-li-noisy-shell\n' ;;
        *) printf '/bin/bash\n' ;;
    esac
}

install -d -m 700 "$root" "$root/client-home"
if [[ ! -f "$root/id_ed25519" ]]; then
    ssh-keygen -q -t ed25519 -N '' -f "$root/id_ed25519"
fi
if [[ ! -f "$root/ssh_host_ed25519_key" ]]; then
    ssh-keygen -q -t ed25519 -N '' -f "$root/ssh_host_ed25519_key"
fi

python3 - "$root/payload.bin" <<'PY'
import pathlib
import sys
pathlib.Path(sys.argv[1]).write_bytes(bytes(range(256)) * 4096)
PY

rm -rf "$root/git-src"
git init -q -b main "$root/git-src"
cp "$root/payload.bin" "$root/git-src/payload.bin"
git -C "$root/git-src" config user.name 'Cosh LI Fixture'
git -C "$root/git-src" config user.email 'cosh-li@example.invalid'
git -C "$root/git-src" add payload.bin
git -C "$root/git-src" commit -q -m fixture

for account in "${accounts[@]}"; do
    shell=$(login_shell_for "$account")
    if ! id -u "$account" >/dev/null 2>&1; then
        useradd --create-home --shell "$shell" "$account"
    fi
    usermod --shell "$shell" --password '*' "$account"
    home="/home/$account"
    install -d -m 700 -o "$account" -g "$account" "$home/.ssh"
    install -m 600 -o "$account" -g "$account" "$root/id_ed25519.pub" "$home/.ssh/authorized_keys"
    printf 'printf "hit\\n" >>"$HOME/bashrc-hits"\n' >"$home/.bashrc"
    rm -rf "$home/transport.git"
    rm -f "$home/scp.bin" "$home/scp-legacy.bin" "$home/sftp.bin" "$home/rsync.bin" \
        "$home/stdin.bin" "$home/bashrc-hits"
    git init -q --bare -b master "$home/transport.git"
    chown -R "$account:$account" "$home"
    cat >"$root/sftp-put-$account.batch" <<EOF
put $root/payload.bin $home/sftp.bin
EOF
done
cat >"$root/sftp-put.batch" <<EOF
put $root/payload.bin /home/$bash_account/sftp.bin
EOF
cat >"$root/sftp-missing.batch" <<EOF
put $root/missing.bin /home/$bash_account/sftp-missing.bin
EOF

cat >"$root/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $root/ssh_host_ed25519_key
PidFile $root/sshd.pid
AuthorizedKeysFile .ssh/authorized_keys
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
UsePAM no
PermitRootLogin no
PermitEmptyPasswords no
AllowUsers ${accounts[*]}
AllowTcpForwarding no
X11Forwarding no
PrintMotd no
PrintLastLog no
UseDNS no
LogLevel ERROR
Subsystem sftp $sftp_server
EOF

if [[ -s "$root/sshd.pid" ]]; then
    sshd_pid=$(<"$root/sshd.pid")
    if kill "$sshd_pid" 2>/dev/null; then
        for _ in {1..50}; do
            kill -0 "$sshd_pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
fi
rm -f "$root/sshd.pid"
/usr/sbin/sshd -f "$root/sshd_config" -E "$root/sshd.log"

known_hosts="$root/known_hosts"
rm -f "$known_hosts"
for _ in {1..50}; do
    if ssh-keyscan -T 1 -p "$port" 127.0.0.1 >"$known_hosts.tmp" 2>/dev/null && [[ -s "$known_hosts.tmp" ]]; then
        mv "$known_hosts.tmp" "$known_hosts"
        break
    fi
    sleep 0.1
done
test -s "$known_hosts"

{
    for alias_account in "cosh-li:$bash_account" "cosh-li-cosh:$cosh_account" "cosh-li-noisy:$noisy_account"; do
        cat <<EOF
Host ${alias_account%%:*}
    HostName 127.0.0.1
    Port $port
    User ${alias_account#*:}
    IdentityFile $root/id_ed25519
    IdentitiesOnly yes
    IdentityAgent none
    UserKnownHostsFile $known_hosts
    StrictHostKeyChecking yes
    BatchMode yes
    PasswordAuthentication no
    PreferredAuthentications publickey
    ConnectTimeout 5
    LogLevel ERROR
EOF
    done
} >"$root/ssh_config"
chmod 600 "$root/ssh_config" "$root/id_ed25519" "$known_hosts"
