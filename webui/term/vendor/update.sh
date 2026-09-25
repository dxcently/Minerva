#!/bin/sh
# Re-vendor the page's four runtime packages from registry.npmjs.org.
#
#   sh vendor/update.sh                      re-fetch exactly what VERSIONS pins
#   sh vendor/update.sh --bump <pkg> <ver>   move one pin, then re-vendor
#
# VERSIONS is the pin file: one line per package, "name version sha512-...".
# The default mode never changes a pin. For each package it reads the
# version's metadata (for the tarball URL only), refuses any tarball URL that
# is not under https://registry.npmjs.org/, downloads the tarball, and stops
# unless its sha512 equals the hash PINNED in VERSIONS. The registry's word
# is not trusted here: a registry (or metadata) that changed under a pinned
# version fails the run.
#
# --bump is the only way to change a pin. It fetches <pkg>@<ver>, checks the
# tarball against the registry's dist.integrity (the one time the registry is
# trusted, and only for the new version), writes that hash as the new pin,
# and then re-vendors everything against the pins as above. Read the diff.
#
# Then, for every package: unpack the one ESM file the page needs, rewrite
# its bare import specifiers to relative paths, and fail if any bare
# specifier is left, static (from "x", import "x") or dynamic (import("x")).
#
# Why rewrite: the door's CSP forbids inline scripts, so an inline importmap is
# impossible; the browser can only resolve "./x.js", never "preact".
# Needs: sh, curl, tar, sed, openssl, base64. No node, no npm.
set -eu

REG=https://registry.npmjs.org
here=$(cd "$(dirname "$0")" && pwd)
pins="$here/VERSIONS"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

die() { echo "$*" >&2; exit 1; }
[ -f "$pins" ] || die "no $pins: the pins are the source of truth"

# name -> sha512 of the tarball at $2
sha() { echo "sha512-$(openssl dgst -sha512 -binary "$1" | base64 | tr -d '\n')"; }

# meta <name> <version>: sets $url (checked) and $reg_integrity
meta() {
  m=$(curl -fsS "$REG/$1/$2") || die "no metadata for $1@$2"
  # the two fields we need, without jq: "tarball":"...", "integrity":"sha512-..."
  url=$(printf '%s' "$m" | sed -n 's/.*"tarball":"\([^"]*\)".*/\1/p')
  reg_integrity=$(printf '%s' "$m" | sed -n 's/.*"integrity":"\(sha512-[^"]*\)".*/\1/p')
  [ -n "$url" ] && [ -n "$reg_integrity" ] || die "no tarball/integrity for $1@$2"
  case "$url" in
    "$REG"/*) ;;
    *) die "REFUSED tarball URL for $1@$2 (not under $REG/): $url" ;;
  esac
  case "$url" in *..*|*' '*) die "REFUSED tarball URL for $1@$2: $url" ;; esac
}

# pinned line for a package: "name version hash"
pin_of() { awk -v n="$1" '$1 == n { print; exit }' "$pins"; }

# ---------------------------------------------------------------- --bump
if [ "${1:-}" = "--bump" ]; then
  [ $# -eq 3 ] || die "usage: sh vendor/update.sh --bump <pkg> <version>"
  bpkg=$2 bver=$3
  [ -n "$(pin_of "$bpkg")" ] || die "$bpkg is not a vendored package (see VERSIONS)"
  printf '%s' "$bver" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || die "bad version: $bver"
  meta "$bpkg" "$bver"
  curl -fsS -o "$work/bump.tgz" "$url"
  got=$(sha "$work/bump.tgz")
  [ "$got" = "$reg_integrity" ] || die "INTEGRITY MISMATCH for $bpkg@$bver: registry $reg_integrity, tarball $got"
  awk -v n="$bpkg" -v v="$bver" -v h="$got" '$1 == n { print n, v, h; next } { print }' "$pins" > "$work/pins.new"
  echo "bump $bpkg -> $bver  $got (matches the registry)"
  pins_use="$work/pins.new"
elif [ $# -ne 0 ]; then
  die "usage: sh vendor/update.sh [--bump <pkg> <version>]"
else
  pins_use="$pins"
fi

# ---------------------------------------------------------------- fetch against the pins
# fetch <package> -> unpacks into $work/<safe name>/package, sets $pkg
fetch() {
  line=$(awk -v n="$1" '$1 == n { print; exit }' "$pins_use")
  [ -n "$line" ] || die "$1 is not pinned in VERSIONS"
  set -- $line
  name=$1 ver=$2 want=$3
  safe=$(echo "$name" | tr '/@' '__')
  meta "$name" "$ver"
  curl -fsS -o "$work/$safe.tgz" "$url"
  got=$(sha "$work/$safe.tgz")
  if [ "$got" != "$want" ]; then
    echo "PIN MISMATCH for $name@$ver" >&2
    echo "  pinned:  $want" >&2
    echo "  tarball: $got" >&2
    exit 1
  fi
  echo "ok  $name@$ver  sha512 matches the pin"
  mkdir -p "$work/$safe"
  tar -xzf "$work/$safe.tgz" -C "$work/$safe"
  pkg="$work/$safe/package"
}

# take <file in package> <out name>
# Rewrites the bare specifiers the four packages use between themselves, and
# drops the sourceMappingURL line (the .map files are not vendored).
take() {
  sed -e 's#\(from *\)"preact/hooks"#\1"./hooks.js"#g' \
      -e "s#\\(from *\\)'preact/hooks'#\\1'./hooks.js'#g" \
      -e 's#\(from *\)"preact"#\1"./preact.js"#g' \
      -e "s#\\(from *\\)'preact'#\\1'./preact.js'#g" \
      -e 's#\(from *\)"@preact/signals-core"#\1"./signals-core.js"#g' \
      -e "s#\\(from *\\)'@preact/signals-core'#\\1'./signals-core.js'#g" \
      -e '/^\/\/# sourceMappingURL=/d' \
      -e 's#//\# sourceMappingURL=.*$##' \
      "$pkg/$1" > "$work/out-$2"
}

fetch preact
take dist/preact.module.js preact.js
take hooks/dist/hooks.module.js hooks.js
cp "$pkg/LICENSE" "$work/LICENSE-preact"

fetch @preact/signals-core
take dist/signals-core.module.js signals-core.js
cp "$pkg/LICENSE" "$work/LICENSE-signals-core"

fetch @preact/signals
take dist/signals.module.js signals.js
cp "$pkg/LICENSE" "$work/LICENSE-signals"

fetch htm
take dist/htm.module.js htm.js
cp "$pkg/LICENSE" "$work/LICENSE-htm"

# Every import left must be relative: a bare one would fail in the browser.
q="[\"'\`]"
for f in "$work"/out-*; do
  if grep -Eo "(from|import) *$q[^./\"'\`][^\"'\`]*$q" "$f"; then
    die "bare import specifier left in ${f##*/out-}"
  fi
  if grep -Eo "import *\\( *$q[^./\"'\`][^\"'\`]*$q" "$f"; then
    die "bare dynamic import() left in ${f##*/out-}"
  fi
done

for f in "$work"/out-*; do cp "$f" "$here/${f##*/out-}"; done
cp "$work"/LICENSE-* "$here/"
if [ "$pins_use" != "$pins" ]; then cp "$pins_use" "$pins"; fi
echo "vendored into $here"
