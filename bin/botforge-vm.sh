#!/bin/sh
# botforge-vm.sh -- the BotForge practice VM under QEMU/KVM in WSL
#
#   botforge-vm.sh import  rebuild the base from the image's original disk
#   botforge-vm.sh inject  add the agent key to the base offline (root; no boot, no net)
#   botforge-vm.sh setup   one boot with a window, to add the key by hand instead
#   botforge-vm.sh freeze  make the base image read-only
#   botforge-vm.sh reset   throw away the run overlay and start a fresh one
#   botforge-vm.sh up      boot the overlay headless: no outbound, ssh on 127.0.0.1:2222 only
#   botforge-vm.sh down    power the guest off, and wait until qemu has exited
#   botforge-vm.sh ready   exit 0 once the guest answers ssh, 1 until then
#   botforge-vm.sh score   print the Aeacus score as one line: score N/T penalties P generated ...
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

running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null
}

# The agent's own door into the guest. Host key checks are off because every
# reset boots the same image on a loopback-only port; there is nothing to pin.
guest() {
    ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
        -p 2222 -i "$KEY" mford@127.0.0.1 "$@"
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
    ! running || { echo "the guest is running; botforge-vm.sh down first" >&2; exit 1; }
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
    # An ACPI power-off first, then `quit` if the guest is still up after 20 s.
    # The Mint desktop ignores the power button (measured: never within 90 s),
    # and the overlay is thrown away at the next reset, so the hard stop loses
    # nothing. Returns once qemu is gone, so a `reset` right after never deletes
    # an overlay a live qemu still has open.
    running || exit 0
    printf 'system_powerdown\n' | nc -U -q1 "$MONITOR" >/dev/null 2>&1 || true
    i=0; while running && [ "$i" -lt 20 ]; do sleep 1; i=$((i + 1)); done
    if running; then
        printf 'quit\n' | nc -U -q1 "$MONITOR" >/dev/null 2>&1 || true
        sleep 2
    fi
    ! running || { echo "qemu $(cat "$PIDFILE") did not exit" >&2; exit 1; }
    ;;
ready)
    # Not just "ssh answers". phocus writes its first report seconds into boot,
    # before the guest's services settle, and that report is wrong: measured
    # 2026-09-26, a fresh overlay's first report read 8/256 with 1 penalty and
    # the next, 14 s later, 0/256. So ready also waits for systemd to finish
    # booting and for a report written after that moment; a baseline read any
    # earlier would credit the agent with points nobody earned.
    guest '
        case $(systemctl is-system-running 2>/dev/null) in running|degraded) ;; *) exit 1 ;; esac
        fin=$(systemctl show -p FinishTimestampMonotonic --value)
        up=$(cut -d. -f1 /proc/uptime)
        settled=$(( $(date +%s) - up + fin / 1000000 ))
        [ "$(stat -c %Y /opt/aeacus/assets/ScoringReport.html 2>/dev/null || echo 0)" -gt "$settled" ]
    ' 2>/dev/null || { echo "guest not ready (ssh, boot, or a post-boot score) yet" >&2; exit 1; }
    ;;
score)
    # Aeacus's phocus service rescores about once a minute and writes this
    # world-readable report, so reading it needs no root in the guest. Output is
    # ONE line with ONE N/T pair: bench's parser takes the first pair it sees,
    # and the report's own date (2026/09/26) would otherwise parse as 9 of 26.
    # A guest that cannot be reached prints nothing parseable and still exits 0,
    # so the bench loop retries instead of aborting the run. ssh is the agent's
    # own door too: an agent that locks it has ended its own run.
    report=$(guest 'cat /opt/aeacus/assets/ScoringReport.html' 2>/dev/null) \
        || { echo "score: guest unreachable over ssh" >&2; exit 0; }
    pair=$(printf '%s\n' "$report" | grep -oE '[0-9]+ out of [0-9]+ points received' | head -n1)
    [ -n "$pair" ] || { echo "score: no points line in the report" >&2; exit 0; }
    earned=${pair%% *}
    total=$(printf '%s\n' "$pair" | awk '{print $4}')
    pen=$(printf '%s\n' "$report" | grep -oE '[0-9]+ penalties assessed' | head -n1 | awk '{print $1}')
    gen=$(printf '%s\n' "$report" | grep -oE 'Generated At: [0-9/]+ [0-9:]+' | head -n1 \
        | sed -e 's/Generated At: //' -e 's#/#-#g' -e 's/ /T/')
    echo "score $earned/$total penalties ${pen:-?} generated ${gen:-?}"
    ;;
*)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
