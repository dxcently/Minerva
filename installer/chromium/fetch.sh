#!/usr/bin/env bash
# fetch.sh DEST: download, verify and assemble the pinned headless Chromium and the system libraries it needs, so it
# runs on an Ubuntu/Debian with nothing but the essential packages and no sudo.
#   DEST/chrome-headless-shell, its .pak/.so/locales files   (Chrome for Testing, pin.env, sha-256 checked)
#   DEST/syslib/*.so*                                        (Ubuntu 22.04 packages from libs.lock, sha-256 checked)
#   DEST/fonts/*.ttf                                         DejaVu (a machine with no fonts renders no text at all)
#   DEST/LICENSES/                                           the packages' copyright files
# Needs: curl, sha256sum, dpkg-deb, python3 (unzip). Network only for what is not in the cache (CACHE, default
# $HOME/.cache/minerva-build).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dest=${1:?usage: fetch.sh DEST}
cache=${CACHE:-$HOME/.cache/minerva-build}
mkdir -p "$cache" "$dest"
# shellcheck source=pin.env
. "$here/pin.env"

get() { # url file sha256
  if [ -f "$2" ] && echo "$3  $2" | sha256sum -c - >/dev/null 2>&1; then return 0; fi
  curl -sSfL --retry 3 -o "$2.part" "$1"
  mv "$2.part" "$2"
  echo "$3  $2" | sha256sum -c - >/dev/null || { echo "fetch.sh: checksum mismatch for $1" >&2; rm -f "$2"; exit 1; }
}

zip=$cache/chrome-headless-shell-$CFT_VERSION-linux64.zip
get "https://storage.googleapis.com/chrome-for-testing-public/$CFT_VERSION/linux64/chrome-headless-shell-linux64.zip" "$zip" "$CFT_SHA256"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/cft.XXXXXX")
trap 'rm -rf "${tmp:?}"' EXIT
python3 - "$zip" "$tmp" <<'PY'
import sys, zipfile, os, stat
z = zipfile.ZipFile(sys.argv[1])
for i in z.infolist():
    z.extract(i, sys.argv[2])
    mode = i.external_attr >> 16
    if mode:
        os.chmod(os.path.join(sys.argv[2], i.filename), mode & 0o777)
PY
rm -rf "${dest:?}"/*
cp -a "$tmp/chrome-headless-shell-linux64/." "$dest/"
rm -f "$dest/deb.deps" "$dest/rpm.deps"
mkdir -p "$dest/syslib" "$dest/LICENSES"

while read -r pkg ver path sha size; do
  deb=$cache/$(basename "$path")
  get "https://archive.ubuntu.com/ubuntu/$path" "$deb" "$sha"
  x=$tmp/x-$pkg
  mkdir -p "$x"
  dpkg-deb -x "$deb" "$x"
  # shared objects of the package (libnss3 keeps its dlopen'ed helpers beside it), symlinks kept
  # (flattened and dereferenced: nss/libfreebl3.so and the libfreebl3.so link beside it become one file)
  find "$x" -type f -path '*/lib/x86_64-linux-gnu/*' -name '*.so*' -print0 \
    | while IFS= read -r -d '' f; do cp --remove-destination "$f" "$dest/syslib/$(basename "$f")"; done
  find "$x" -type l -path '*/lib/x86_64-linux-gnu/*' -name '*.so*' -print0 \
    | while IFS= read -r -d '' f; do
        t=$(readlink "$f")
        case "$t" in
          */*) if [ -e "$f" ]; then cp -L --remove-destination "$f" "$dest/syslib/$(basename "$f")"; fi ;;
          *) ln -sf "$t" "$dest/syslib/$(basename "$f")" ;;
        esac
      done
  if [ -d "$x/usr/share/fonts" ]; then mkdir -p "$dest/fonts"; find "$x/usr/share/fonts" -name "*.ttf" -exec cp {} "$dest/fonts/" \; ; fi
  if [ -f "$x/usr/share/doc/$pkg/copyright" ]; then cp "$x/usr/share/doc/$pkg/copyright" "$dest/LICENSES/$pkg.copyright"; fi
done <"$here/libs.lock"

chmod +x "$dest/chrome-headless-shell"
echo "chromium $CFT_VERSION + $(ls "$dest/syslib" | wc -l) system libs -> $dest ($(du -sh "$dest" | cut -f1))"
