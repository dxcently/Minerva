#!/bin/sh
# botforge-vm.sh -- the BotForge practice VM under QEMU/KVM in WSL
#
#   botforge-vm.sh import  rebuild the base from the image's original disk
#   botforge-vm.sh inject  add the agent key to the base offline (root; no boot, no net)
#   botforge-vm.sh setup   one boot with a window, to add the key by hand instead
#   botforge-vm.sh freeze  make the base image read-only
#   botforge-vm.sh reset   throw away the run overlay and start a fresh one
#   botforge-vm.sh up      boot the overlay headless: no outbound, ssh on 127.0.0.1:2222 only
#   botforge-vm.sh down    power the guest off
set -eu

# WSL inherits the Windows PATH, whose entries have spaces ("/mnt/c/Program
# Files/..."); a bare command name can resolve to a Windows exe and split on the
# space. Pin a Unix PATH so the same call works whether it is launched from a
# WSL login shell or from Windows via `wsl -e`.
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH

# `inject` needs root; under sudo $HOME is root's, so resolve the invoking
# user's home instead and the key and image paths still point at their files.
if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then
    HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
fi

DIR="${BOTFORGE_DIR:-$HOME/botforge}"
BASE="$DIR/botforge-base.qcow2"
OVERLAY="$DIR/overlay.qcow2"
PIDFILE="$DIR/qemu.pid"
MONITOR="$DIR/monitor.sock"
KEY="$HOME/.ssh/minerva_agent"

# The OVF's 2 vCPUs, 4 GiB and E1000 NIC. Its LSI Logic controller has no exact
# QEMU twin, so the disk sits on AHCI, which every Linux initramfs carries.
qemu() {
    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4096 \
        -device ahci,id=ahci -device ide-hd,drive=disk0,bus=ahci.0 \
        -device e1000,netdev=net0 -vga std \
        -serial file:"$DIR/serial.log" "$@"
}

case "${1:-}" in
import)
    [ -w "$BASE" ] || chmod u+w "$BASE" 2>/dev/null || true
    rm -f "$BASE" "$OVERLAY"
    qemu-img convert -p -O qcow2 "$DIR"/*-disk1.vmdk "$BASE"
    ;;
inject)
    # Add only the agent key to the base by editing its disk offline: no boot,
    # no network, no service. The mount needs root; nothing else is written.
    # Idempotent, and the reversible twin of `setup`'s guestfwd path.
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -C minerva-agent -f "$KEY"
    [ -w "$BASE" ] || { echo "$BASE is read-only; run before freeze" >&2; exit 1; }
    modprobe nbd max_part=16
    dev=/dev/nbd0
    qemu-nbd --disconnect "$dev" >/dev/null 2>&1 || true
    qemu-nbd --connect="$dev" "$BASE"
    mnt=$(mktemp -d)
    trap 'umount "$mnt" 2>/dev/null || true; qemu-nbd --disconnect "$dev" >/dev/null 2>&1 || true; rmdir "$mnt" 2>/dev/null || true' EXIT
    i=0; while [ ! -e "${dev}p1" ] && [ "$i" -lt 10 ]; do sleep 1; i=$((i + 1)); done
    root=""
    for p in "${dev}"p*; do
        [ "$(blkid -o value -s TYPE "$p" 2>/dev/null)" = ext4 ] || continue
        mount "$p" "$mnt" 2>/dev/null || continue
        [ -d "$mnt/home/mford" ] && { root="$p"; break; }
        umount "$mnt"
    done
    [ -n "$root" ] || { echo "no ext4 partition with /home/mford on $BASE" >&2; exit 1; }
    u=$(stat -c %u "$mnt/home/mford"); g=$(stat -c %g "$mnt/home/mford")
    install -d -m 700 -o "$u" -g "$g" "$mnt/home/mford/.ssh"
    ak="$mnt/home/mford/.ssh/authorized_keys"
    touch "$ak"
    grep -qxF "$(cat "$KEY.pub")" "$ak" || cat "$KEY.pub" >>"$ak"
    chmod 600 "$ak"; chown "$u:$g" "$ak"
    sync
    echo "injected $KEY.pub into mford on $root"
    ;;
setup)
    if [ -f "$KEY" ]; then
        echo "using the existing agent key $KEY"
    else
        ssh-keygen -q -t ed25519 -N '' -C minerva-agent -f "$KEY"
        echo "made a new agent key $KEY"
    fi
    # The image ships its own sshd, and its config is scored: installing or
    # upgrading anything here hands the agent points it never earned.
    served=$(mktemp -d)
    { printf 'printf "HTTP/1.0 200 OK\\r\\n\\r\\n"\n'; printf 'cat %s\n' "$KEY.pub"; } >"$served/k.sh"
    trap 'rm -rf "$served"' EXIT
    echo "the window stays black for ~40 s, then the Mint desktop logs itself in"
    echo "in its terminal type this, and nothing else (no apt, no sudo):"
    echo "  mkdir -p ~/.ssh && wget -qO- 10.0.2.100/k >> ~/.ssh/authorized_keys && chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys"
    qemu -drive file="$BASE",if=none,id=disk0,format=qcow2 \
        -netdev user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:2222-:22,guestfwd=tcp:10.0.2.100:80-cmd:sh\ "$served/k.sh" \
        -monitor unix:"$MONITOR",server,nowait \
        -display gtk,zoom-to-fit=on -usb -device usb-tablet
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
    echo "booting; ssh -p 2222 -i $KEY mford@127.0.0.1 once sshd answers"
    ;;
down)
    [ -S "$MONITOR" ] && printf 'system_powerdown\n' | nc -U -q1 "$MONITOR" >/dev/null 2>&1 || true
    ;;
*)
    sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
