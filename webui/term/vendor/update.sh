#!/bin/sh
# Re-vendor the page's runtime packages from registry.npmjs.org.
#
#   sh vendor/update.sh                      re-fetch exactly what VERSIONS pins
#   sh vendor/update.sh --check              same, but write nothing: fail if the
#                                            files on disk differ from the result
#   sh vendor/update.sh --bump <pkg> <ver>   move one pin, then re-vendor
#   sh vendor/update.sh --add <pkg> <ver>    pin a package this script already
#                                            has a block for, then re-vendor
#
# VERSIONS is the pin file: one line per package, "name version sha512-...".
# The default mode never changes a pin. For each package it reads the
# version's metadata (for the tarball URL only), refuses any tarball URL that
# is not under https://registry.npmjs.org/, downloads the tarball, and stops
# unless its sha512 equals the hash PINNED in VERSIONS. The registry's word
# is not trusted here: a registry (or metadata) that changed under a pinned
# version fails the run.
#
# --bump and --add are the only ways to change a pin. Each fetches <pkg>@<ver>,
# checks the tarball against the registry's dist.integrity (the one time the
# registry is trusted, and only for that version), writes that hash as the pin
# (--bump replaces the line, --add appends one; --add refuses a package that
# is already pinned), and then re-vendors everything against the pins as
# above. Nothing is written until every check has passed. Read the diff.
# --add skips, with a note, a package the script knows but VERSIONS does not
# pin yet, so several packages can be added one run at a time; every other
# mode refuses a missing pin. A pin for a package the script has no block for
# is refused too, so VERSIONS and the script cannot drift apart.
#
# Then, for every package: unpack the one ESM file the page needs, rewrite
# its bare import specifiers to relative paths, and fail if any bare
# specifier is left, static (from "x", import "x") or dynamic (import("x")),
# or if a JS file calls eval( or Function( / new Function (the door's CSP has
# no unsafe-eval, so they could never run, and nothing vendored may rely on
# them).
#
# Why rewrite: the door's CSP forbids inline scripts, so an inline importmap is
# impossible; the browser can only resolve "./x.js", never "preact".
# Needs: sh, curl, tar, sed, openssl, base64, cmp. No node, no npm.
set -eu

REG=https://registry.npmjs.org
here=$(cd "$(dirname "$0")" && pwd)
pins="$here/VERSIONS"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Every package this script has a block for (the loop at the bottom).
pkgs="preact @preact/signals-core @preact/signals htm @dagrejs/graphlib @dagrejs/dagre @xterm/xterm @xterm/addon-fit"
check= adding=

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

# ---------------------------------------------------------------- --bump, --add
usage="usage: sh vendor/update.sh [--check | --bump <pkg> <version> | --add <pkg> <version>]"
case "${1:-}" in
  --bump|--add)
    mode=${1#--}
    [ $# -eq 3 ] || die "$usage"
    bpkg=$2 bver=$3
    if [ "$mode" = bump ]; then
      [ -n "$(pin_of "$bpkg")" ] || die "$bpkg is not a vendored package (see VERSIONS; --add pins a new one)"
    else
      [ -z "$(pin_of "$bpkg")" ] || die "$bpkg is already pinned (--bump moves a pin)"
      case " $pkgs " in
        *" $bpkg "*) ;;
        *) die "$bpkg has no block in this script: write its take lines first (the packages: $pkgs)" ;;
      esac
      adding=1
    fi
    printf '%s' "$bver" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || die "bad version: $bver"
    meta "$bpkg" "$bver"
    curl -fsS -o "$work/bump.tgz" "$url"
    got=$(sha "$work/bump.tgz")
    [ "$got" = "$reg_integrity" ] || die "INTEGRITY MISMATCH for $bpkg@$bver: registry $reg_integrity, tarball $got"
    # (tr: a checkout that turned VERSIONS into CRLF must not leave it half CRLF)
    if [ "$mode" = bump ]; then
      awk -v n="$bpkg" -v v="$bver" -v h="$got" '$1 == n { print n, v, h; next } { print }' "$pins" | tr -d '\r' > "$work/pins.new"
    else
      { tr -d '\r' < "$pins"; echo "$bpkg $bver $got"; } > "$work/pins.new"
    fi
    echo "$mode $bpkg -> $bver  $got (matches the registry)"
    pins_use="$work/pins.new"
    ;;
  --check)
    [ $# -eq 1 ] || die "$usage"
    check=1
    pins_use="$pins"
    ;;
  "")
    [ $# -eq 0 ] || die "$usage"
    pins_use="$pins"
    ;;
  *) die "$usage" ;;
esac

# A pin for a package this script cannot vendor would never be verified.
for n in $(awk '!/^#/ && NF { print $1 }' "$pins_use"); do
  case " $pkgs " in
    *" $n "*) ;;
    *) die "$n is pinned in VERSIONS but this script has no block for it" ;;
  esac
done

# is_pinned <pkg>: 0 if pins_use pins it. Unpinned: only --add may go on without
# it (it vendors what is pinned so far); every other mode refuses.
is_pinned() {
  if [ -n "$(awk -v n="$1" '$1 == n { print; exit }' "$pins_use")" ]; then return 0; fi
  if [ -n "$adding" ]; then echo "skip $1 (not pinned yet)"; return 1; fi
  die "$1 is not pinned in VERSIONS"
}

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
# Rewrites the bare specifiers the packages use between themselves (dagre and
# addon-fit ship as self-contained bundles today and match none of the last
# two rules; they stay in case a later version imports its sibling), and
# drops the sourceMappingURL line (the .map files are not vendored).
take() {
  sed -e 's#\(from *\)"preact/hooks"#\1"./hooks.js"#g' \
      -e "s#\\(from *\\)'preact/hooks'#\\1'./hooks.js'#g" \
      -e 's#\(from *\)"preact"#\1"./preact.js"#g' \
      -e "s#\\(from *\\)'preact'#\\1'./preact.js'#g" \
      -e 's#\(from *\)"@preact/signals-core"#\1"./signals-core.js"#g' \
      -e "s#\\(from *\\)'@preact/signals-core'#\\1'./signals-core.js'#g" \
      -e 's#\(from *\)"@dagrejs/graphlib"#\1"./graphlib.js"#g' \
      -e "s#\\(from *\\)'@dagrejs/graphlib'#\\1'./graphlib.js'#g" \
      -e 's#\(from *\)"@xterm/xterm"#\1"./xterm.js"#g' \
      -e "s#\\(from *\\)'@xterm/xterm'#\\1'./xterm.js'#g" \
      -e '/^\/\/# sourceMappingURL=/d' \
      -e 's#//\# sourceMappingURL=.*$##' \
      "$pkg/$1" > "$work/out-$2"
}

# One block per package: which file(s) of the tarball the page takes. Add a
# package by writing its block here and adding its name to $pkgs; then
# `--add <pkg> <ver>` pins it.
# Never vendor the xterm webgl and canvas addons (the canvas one is 5.x only)
# or the clipboard (OSC 52) and web-links addons: the page needs none of them.
for p in $pkgs; do
  is_pinned "$p" || continue
  fetch "$p"
  case "$p" in
    preact)
      take dist/preact.module.js preact.js
      take hooks/dist/hooks.module.js hooks.js
      cp "$pkg/LICENSE" "$work/LICENSE-preact" ;;
    @preact/signals-core)
      take dist/signals-core.module.js signals-core.js
      cp "$pkg/LICENSE" "$work/LICENSE-signals-core" ;;
    @preact/signals)
      take dist/signals.module.js signals.js
      cp "$pkg/LICENSE" "$work/LICENSE-signals" ;;
    htm)
      take dist/htm.module.js htm.js
      cp "$pkg/LICENSE" "$work/LICENSE-htm" ;;
    @dagrejs/graphlib)
      take dist/graphlib.esm.js graphlib.js
      cp "$pkg/LICENSE" "$work/LICENSE-graphlib" ;;
    @dagrejs/dagre)
      take dist/dagre.esm.js dagre.js
      cp "$pkg/LICENSE" "$work/LICENSE-dagre" ;;
    @xterm/xterm)
      take lib/xterm.mjs xterm.js
      cp "$pkg/css/xterm.css" "$work/out-xterm.css"
      cp "$pkg/LICENSE" "$work/LICENSE-xterm" ;;
    @xterm/addon-fit)
      take lib/addon-fit.mjs addon-fit.js
      cp "$pkg/LICENSE" "$work/LICENSE-xterm-addon-fit" ;;
  esac
done

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

# The CSP has no unsafe-eval: eval( and Function( / new Function would be
# refused at run time, so nothing vendored may reach for them. A hit in
# minified code may be a string, not a call: read it before overruling.
for f in "$work"/out-*.js; do
  if grep -Eo '(^|[^A-Za-z0-9_$])(eval|Function) *\(|new +Function([^A-Za-z0-9_$]|$)' "$f"; then
    die "eval( or Function( or new Function in ${f##*/out-}"
  fi
done

# --check: write nothing; the files on disk must be exactly what the pinned
# tarballs give, and the directory must hold nothing else.
if [ -n "$check" ]; then
  bad=0 count=0 expect=" VERSIONS update.sh README.md "
  for f in "$work"/out-* "$work"/LICENSE-*; do
    n=${f##*/}; n=${n#out-}
    expect="$expect$n "
    count=$((count + 1))
    if [ ! -f "$here/$n" ]; then echo "MISSING $n" >&2; bad=1
    elif ! cmp -s "$f" "$here/$n"; then echo "DIFFERS $n" >&2; bad=1
    fi
  done
  for f in "$here"/*; do
    n=${f##*/}
    case "$expect" in
      *" $n "*) ;;
      *) echo "UNEXPECTED $n (this script does not produce it)" >&2; bad=1 ;;
    esac
  done
  [ "$bad" -eq 0 ] || die "check FAILED: $here is not what the pins give (sh vendor/update.sh rewrites it; read the diff)"
  echo "check ok: $count files byte-identical to the pinned tarballs"
  exit 0
fi

for f in "$work"/out-*; do cp "$f" "$here/${f##*/out-}"; done
cp "$work"/LICENSE-* "$here/"
if [ "$pins_use" != "$pins" ]; then cp "$pins_use" "$pins"; fi
echo "vendored into $here"
