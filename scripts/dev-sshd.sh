#!/usr/bin/env bash
# Starts an unprivileged OpenSSH server on 127.0.0.1 for manual testing (key auth only, as the
# current user). Usage: scripts/dev-sshd.sh [dir] [port]   — stop it with: kill $(cat <dir>/sshd.pid)
set -euo pipefail
DIR="${1:-${TMPDIR:-/tmp}/peler-dev-sshd}"
PORT="${2:-2299}"
mkdir -p "$DIR"
[ -f "$DIR/host_ed25519" ] || ssh-keygen -q -t ed25519 -N "" -f "$DIR/host_ed25519"
[ -f "$DIR/client_ed25519" ] || ssh-keygen -q -t ed25519 -N "" -C peler-dev -f "$DIR/client_ed25519"
cp "$DIR/client_ed25519.pub" "$DIR/authorized_keys"
cat > "$DIR/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $DIR/host_ed25519
AuthorizedKeysFile $DIR/authorized_keys
PidFile $DIR/sshd.pid
UsePAM no
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
Subsystem sftp /usr/libexec/sftp-server
LogLevel ERROR
AcceptEnv LANG LC_*
EOF
/usr/sbin/sshd -f "$DIR/sshd_config"
echo "sshd on 127.0.0.1:$PORT"
echo "user: $USER"
echo "key:  $DIR/client_ed25519"
echo "host key: $(cut -d' ' -f1,2 "$DIR/host_ed25519.pub")"
