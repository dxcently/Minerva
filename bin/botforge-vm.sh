#!/bin/sh
# botforge-vm.sh -- the BotForge practice VM under QEMU/KVM in WSL
#
#   botforge-vm.sh setup   one boot with outbound network and a window, to install sshd
#   botforge-vm.sh freeze  make the base image read-only
#   botforge-vm.sh reset   throw away the run overlay and start a fresh one
#   botforge-vm.sh up      boot the overlay headless: no outbound, ssh on 127.0.0.1:2222 only
#   botforge-vm.sh down    power the guest off
set -eu

DIR="${BOTFORGE_DIR:-$HOME/botforge}"
BASE="$DIR/botforge-base.qcow2"
OVERLAY="$DIR/overlay.qcow2"
PIDFILE="$DIR/qemu.pid"
MONITOR="$DIR/monitor.sock"
KEY="$HOME/.ssh/minerva_agent"

# The OVF's own hardware: 2 vCPUs, 4 GiB, an LSI Logic disk, an E1000 NIC.
qemu() {
    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4096 \
        -device lsi53c895a,id=scsi0 -device scsi-hd,drive=disk0,bus=scsi0.0 \
        -device e1000,netdev=net0 "$@"
}

case "${1:-}" in
setup)
    [ -f "$KEY" ] || ssh-keygen -t ed25519 -N '' -C minerva-agent -f "$KEY"
    echo "public key to add inside the VM (~/.ssh/authorized_keys):"
    cat "$KEY.pub"
    qemu -drive file="$BASE",if=none,id=disk0,format=qcow2 \
        -netdev user,id=net0,hostfwd=tcp:127.0.0.1:2222-:22
    ;;
freeze)
    chmod a-w "$BASE"
    ls -l "$BASE"
    ;;
reset)
    rm -f "$OVERLAY"
    qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY" >/dev/null
    echo "fresh overlay on $(basename "$BASE")"
    ;;
up)
    [ -f "$OVERLAY" ] || "$0" reset
    qemu -drive file="$OVERLAY",if=none,id=disk0,format=qcow2 \
        -netdev user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:2222-:22 \
        -display none -daemonize -pidfile "$PIDFILE" \
        -monitor unix:"$MONITOR",server,nowait
    echo "booting; ssh -p 2222 -i $KEY <user>@127.0.0.1 once sshd answers"
    ;;
down)
    [ -S "$MONITOR" ] && printf 'system_powerdown\n' | nc -U -q1 "$MONITOR" >/dev/null 2>&1 || true
    ;;
*)
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
