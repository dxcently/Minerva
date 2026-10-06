#!/usr/bin/env bash
# build.sh: build the Minerva Linux bundle (static musl binaries + the web UI + the Windows hoot files).
# Runs on Linux or in WSL. Needs: rust (cargo) with the x86_64-unknown-linux-musl target, a C compiler (gcc, or musl-gcc
# when installed), tar, gzip. It does not publish anything.
#
# usage: installer/build.sh --out DIR [--version-base X.Y.Z] [--sha SHA] [--pubkey FILE] [--allow-placeholder-key] [--dev] [--test-overrides]
#   --out DIR       where minerva-<version>-linux-x86_64.tar.gz and install.sh are written
#   --version-base  default: the repo-root VERSION file
#   --sha           default: git rev-parse --short HEAD (+ .dirty when the tracked files changed)
#   --pubkey FILE   the minisign PUBLIC key compiled into `minerva update` (default installer/minisign.pub; the build
#                   fails while that file is the placeholder)
#   --dev           allow the gcc-against-glibc-headers fallback for the C parts (ring, sqlite) when musl-gcc is missing. It
#                   switches glibc's FORTIFY_SOURCE off, so such a build is for testing only. A release build (no --dev)
#                   requires musl-gcc (apt install musl-tools) and keeps the stack protector and fortify.
#   --test-overrides  compile the test-only back doors (MINERVA_KEYS_DIR, update --self-path) into `minerva`; never for a release
# environment: CARGO_TARGET_DIR (default $HOME/installer-target; never the repo, never ~/perch-target)
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/.." && pwd)
out= base= sha= pubkey= allow=0 dev=0 testov=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out=$2; shift 2 ;;
    --version-base) base=$2; shift 2 ;;
    --sha) sha=$2; shift 2 ;;
    --pubkey) pubkey=$2; shift 2 ;;
    --allow-placeholder-key) allow=1; shift ;;
    --dev) dev=1; shift ;;
    --test-overrides) testov=1; shift ;;
    -h|--help) sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "build.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "build.sh: --out DIR is required" >&2; exit 2; }
mkdir -p "$out"; out=$(cd "$out" && pwd)

[ -n "$base" ] || base=$(tr -d ' \r\n' <"$repo/VERSION")
if [ -z "$sha" ]; then
  sha=$(git -C "$repo" rev-parse --short HEAD 2>/dev/null || echo nogit)
  # tracked changes AND untracked files in every path that ships (nix and the flake included)
  if [ -n "$(git -C "$repo" status --porcelain --untracked-files=all -- perch setup eidolon harnox webui/term bin installer jev eidolon-plugins nix flake.nix flake.lock VERSION 2>/dev/null)" ]; then
    sha=$sha.dirty
  fi
fi
full=$base-g$sha
echo "== building Minerva $full"
export MINERVA_VERSION_BASE=$base MINERVA_BUILD_SHA=$sha
if [ -n "$pubkey" ]; then export MINERVA_UPDATE_PUBKEY=$pubkey; fi
if [ "$allow" = 1 ]; then export MINERVA_ALLOW_PLACEHOLDER_KEY=1; fi

T=x86_64-unknown-linux-musl
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/installer-target}
rustup target list --installed 2>/dev/null | grep -qx "$T" || rustup target add "$T"

# C code in the dependency graph (ring, the bundled sqlite) must be compiled for musl. With musl-gcc installed
# (apt install musl-tools) that is all there is to it. Without it, gcc compiles against glibc's headers and links
# against musl: the sqlite amalgamation then needs glibc's fortify and 64-bit-file aliases switched off. Both ways
# produce a fully static binary (checked below).
if command -v musl-gcc >/dev/null 2>&1; then
  export CC_x86_64_unknown_linux_musl=musl-gcc
  echo "== C compiler: musl-gcc"
elif [ "$dev" = 1 ]; then
  export CC_x86_64_unknown_linux_musl=gcc
  # Only fortify and the 64-bit-file aliases go: glibc's _chk functions do not exist in musl. The stack protector stays.
  export CFLAGS_x86_64_unknown_linux_musl="-U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0 -fstack-protector-strong -U_FILE_OFFSET_BITS -D_FILE_OFFSET_BITS=32 -DSQLITE_DISABLE_LFS"
  echo "== C compiler: gcc with the glibc-header workaround (--dev: fortify OFF; NOT for a release; apt install musl-tools)"
else
  echo "build.sh: musl-gcc is not installed. A release build needs it (apt install musl-tools) so the C parts keep the stack" >&2
  echo "          protector and fortify. For a test build only: pass --dev (gcc against glibc headers, fortify off)." >&2
  exit 1
fi

build() { # dir, cargo args...
  local d=$1; shift
  echo "== cargo build $d"
  (cd "$repo/$d" && cargo build --release --locked --target "$T" "$@")
}
build setup
build perch --bin perch
build eidolon -p eidolon-cli --bin eidolon
if [ "$testov" = 1 ]; then
  echo "== NOTE: --test-overrides: this minerva has test-only back doors; do not release it"
  build installer --bin minerva --features test-overrides
else
  build installer --bin minerva
fi
build eidolon-plugins -p eidolon-browser --bin eidolon-browser

R=$CARGO_TARGET_DIR/$T/release
stage=$out/stage
rm -rf "$stage"
top=$stage/minerva-$full
mkdir -p "$top/bin" "$top/webui" "$top/windows"
for b in minerva perch eidolon minerva-setup eidolon-browser; do
  install -m 0755 "$R/$b" "$top/bin/$b"
  if command -v strip >/dev/null 2>&1; then strip "$top/bin/$b" || true; fi
  if ! file "$top/bin/$b" | grep -Eq 'statically linked|static-pie linked'; then echo "build.sh: $b is not static" >&2; exit 1; fi
done
install -m 0755 "$repo/bin/perch-open.sh" "$repo/bin/minerva-setup.sh" "$top/bin/"
cp -r "$repo/webui/term" "$top/webui/term"

# plugins (installed into the member's eidolon config by `minerva plugins sync`), without the browser's Rust source
mkdir -p "$top/plugins"
for p in browser jev subagent; do cp -r "$repo/eidolon-plugins/$p" "$top/plugins/$p"; done
rm -rf "$top/plugins/browser/service" "$top/plugins/browser/package.nix"
# the headless Chromium + the libraries it needs (pinned and checksummed: installer/chromium/)
echo "== chromium (pinned, checksummed)"
bash "$here/chromium/fetch.sh" "$top/lib/chromium"
install -m 0755 "$here/chromium/wrapper.sh" "$top/bin/chromium"
# the optional jev Python service (jev-setup builds the venv; nothing here runs without it)
mkdir -p "$top/jev/runs"
for f in server.py _paths.py requirements.txt requirements.torch.txt requirements.pinned.txt get-openjev.sh; do cp "$repo/jev/$f" "$top/jev/$f"; done
cp -r "$repo/jev/jevlike" "$repo/jev/openjev" "$top/jev/"
cp "$repo/jev/runs/synthetic.pt" "$repo/jev/runs/jev-base-v0.pt" "$repo/jev/runs/jev-base-v0.record.json" "$top/jev/runs/"
find "$top/jev" -name __pycache__ -type d -prune -exec rm -rf {} +
install -m 0644 "$repo/bin/hoot.ps1" "$repo/bin/hoot.cmd" "$top/windows/"
if [ -f "$repo/LICENSE" ]; then install -m 0644 "$repo/LICENSE" "$top/LICENSE"; fi
printf '%s\n' "$full" >"$top/VERSION"

name=minerva-$full-linux-x86_64.tar.gz
epoch=$(git -C "$repo" log -1 --format=%ct 2>/dev/null || echo 0)
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -C "$stage" -cf - "minerva-$full" | gzip -n -9 >"$out/$name"
rm -rf "$stage"
install -m 0755 "$here/install.sh" "$out/install.sh"
echo "== bundle: $out/$name ($(stat -c %s "$out/$name") bytes)"
