#!/usr/bin/env bash
# bundle.sh: `installer/build.sh --dry-run` puts plugins/term in the bundle, with the licence notices, and leaves out what must
# not ship (the plugin's tests, the browser's Rust source, anything git does not track). No cargo, no network, nothing outside a
# temp dir is written. It runs git, so run it where git reads this checkout: Git Bash for a worktree on Windows, WSL or Linux for
# a plain checkout.
#
#   bash plugins/term/tests/bundle.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/term-bundle.XXXXXX")
trap 'rm -rf "$T"' EXIT

# a PATH with no cargo or rustup in it: a dry run has to need neither. git's own folder is added, as build.sh needs git
gitdir=$(dirname "$(command -v git)")
list=$(env -i PATH="$gitdir:/usr/bin:/bin" HOME="$T/home" bash "$repo/installer/build.sh" --out "$T/out" --dry-run --sha t3test --version-base 0.0.0)

fail=0
has() { if ! grep -qx -- "$1" <<<"$list"; then echo "FAIL the bundle lacks $1" >&2; fail=1; fi; }
lacks() { if grep -q -- "$1" <<<"$list"; then echo "FAIL the bundle holds $1" >&2; fail=1; fi; }

for f in plugin.rn LICENSE README.md tools/close.rn tools/list.rn tools/open.rn tools/read.rn tools/send.rn; do has "plugins/term/$f"; done
for p in browser jev subagent; do has "plugins/$p/plugin.rn"; done
has THIRD-PARTY.md
has LICENSES/Apache-2.0.txt
lacks '^plugins/term/tests/'
lacks '^plugins/probe'
lacks '^plugins/browser/service'
lacks '^plugins/browser/package.nix'
lacks '^plugins/README.md'
[ ! -e "$T/out/stage" ] || { echo "FAIL the dry run left its staging behind" >&2; fail=1; }

# every folder of plugins/ with a plugin.rn is in, not just term (mail, when it lands)
for d in "$repo"/plugins/*/; do
  if [ -f "$d/plugin.rn" ]; then has "plugins/$(basename "$d")/plugin.rn"; fi
done

# only what git tracks ships: a gitignored or untracked file in plugins/term (.env, a key, a cache) is not in the bundle
tracked=$(git -C "$repo" ls-files -- plugins/term | grep -v '^plugins/term/tests/' | LC_ALL=C sort)
staged=$(grep '^plugins/term/' <<<"$list" | LC_ALL=C sort)
if [ "$tracked" != "$staged" ]; then echo "FAIL plugins/term in the bundle is not what git tracks (diff below)" >&2; diff <(echo "$tracked") <(echo "$staged") >&2 || true; fail=1; fi

[ "$fail" = 0 ] || { echo "$list" >&2; exit 1; }
echo "ok   build.sh --dry-run lists plugins/term ($(grep -c '^plugins/term/' <<<"$list") files, the tracked ones) and the notices"
