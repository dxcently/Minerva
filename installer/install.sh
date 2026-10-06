#!/bin/sh
# install.sh: install the Minerva Linux bundle on any Linux or inside WSL. The same logic minerva-install.exe uses:
# this only unpacks the bundle and runs its `minerva install` (the one place the install logic lives).
#
# usage: install.sh [--prefix DIR] [--uninstall] [BUNDLE.tar.gz]
#   BUNDLE      default: the minerva-*-linux-x86_64.tar.gz next to this script
#   --prefix    default ~/.local; installs to DIR/opt/minerva/<version> with a `current` link and DIR/bin/minerva
#   --uninstall remove the installed program; NEVER ~/Minerva (your chats and settings)
#
# Trust: the bundle MUST match SHA256SUMS, which must sit beside it with a line for the bundle, and sha256sum must exist; any
# of those missing is a hard failure (a check that can be skipped by deleting a file is no check). That catches damage and a
# swapped bundle, but SHA256SUMS comes from the same place as the bundle: to know it is the maintainer's, verify
# SHA256SUMS.minisig by hand (installer/README.md, "Checking a download by hand"), then every later update is signature-checked
# by `minerva update` against a key compiled into the program.
set -eu

prefix=${HOME:?HOME is not set}/.local
uninstall=0
bundle=
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) [ $# -ge 2 ] || { echo "install.sh: --prefix needs a directory" >&2; exit 2; }; prefix=$2; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "install.sh: unknown option $1" >&2; exit 2 ;;
    *) bundle=$1; shift ;;
  esac
done

if [ "$uninstall" = 1 ]; then
  m=$prefix/opt/minerva/current/bin/minerva
  [ -x "$m" ] || { echo "install.sh: nothing installed under $prefix/opt/minerva"; exit 0; }
  exec "$m" uninstall --prefix "$prefix"
fi

here=$(cd "$(dirname "$0")" && pwd)
if [ -z "$bundle" ]; then
  bundle=$(ls "$here"/minerva-*-linux-x86_64.tar.gz 2>/dev/null | sort | tail -n 1 || true)
  [ -n "$bundle" ] || { echo "install.sh: no minerva-*-linux-x86_64.tar.gz next to this script; pass the bundle path" >&2; exit 1; }
fi
[ -f "$bundle" ] || { echo "install.sh: no such bundle: $bundle" >&2; exit 1; }
[ "$(uname -m)" = x86_64 ] || { echo "install.sh: the bundle is x86_64 only (this is $(uname -m))" >&2; exit 1; }

bdir=$(cd "$(dirname "$bundle")" && pwd)
bname=$(basename "$bundle")
command -v sha256sum >/dev/null 2>&1 || { echo "install.sh: sha256sum is not installed, so the download cannot be checked; refusing (apt install coreutils)" >&2; exit 1; }
[ -f "$bdir/SHA256SUMS" ] || { echo "install.sh: no SHA256SUMS beside $bname, so the download cannot be checked; refusing (get it from the same release page)" >&2; exit 1; }
# the line for exactly this file name: "<64 hex>  <name>"
line=$(awk -v n="$bname" 'NF == 2 && $2 == n { print; exit }' "$bdir/SHA256SUMS")
[ -n "$line" ] || { echo "install.sh: SHA256SUMS has no line for $bname; refusing" >&2; exit 1; }
(cd "$bdir" && printf '%s\n' "$line" | sha256sum -c - >/dev/null) || { echo "install.sh: $bname does not match SHA256SUMS; refusing" >&2; exit 1; }
echo "checksum ok ($bname)"
if [ -f "$bdir/SHA256SUMS.minisig" ]; then echo "note: SHA256SUMS.minisig is here; to check SHA256SUMS itself: minisign -Vm SHA256SUMS -p <the maintainer's public key> (see the README)"; fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/minerva-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
tar -xzf "$bdir/$bname" -C "$tmp"
[ "$(ls "$tmp" | wc -l)" = 1 ] || { echo "install.sh: the bundle should hold exactly one top directory" >&2; exit 1; }
top=$(ls "$tmp")
"$tmp/$top/bin/minerva" install --from "$tmp/$top" --prefix "$prefix"
