#!/usr/bin/env bash
# release-files.sh [resign] DIR
#   release-files.sh DIR           write SHA256SUMS and latest.json for the release files already in DIR (the bundle, install.sh,
#                                  minerva-install.exe when present). latest.json carries `released` (now), `expires` (now + 30
#                                  days) and `keys_serial` (the serial of DIR/keys.json, 0 when there is none).
#   release-files.sh resign DIR    re-stamp the existing latest.json with a fresh `released`/`expires` and the current keys_serial,
#                                  WITHOUT rebuilding anything. Then sign it again (see below). Do this at least every 30 days:
#                                  a manifest older than that still works, but members are told "release info is stale".
# Neither signs nor publishes: signing is the operator's step (installer/README.md, "Releasing"), one minisign -S per file:
#   minisign -S -s KEY -m latest.json ; minisign -S -s KEY -m SHA256SUMS ; minisign -S -s KEY -m minerva-*-linux-x86_64.tar.gz
#   if DIR holds keys.json, re-sign it too (keys.json.minisig) and keep its own "expires" date in the future.
set -euo pipefail
mode=build
if [ "${1:-}" = resign ]; then mode=resign; shift; fi
dir=${1:?usage: release-files.sh [resign] DIR}
cd "$dir"
now=$(date +%s)
exp=$((now + 30 * 24 * 3600))
keys_serial=0
if [ -f keys.json ]; then
  keys_serial=$(sed -n 's/.*"serial"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' keys.json | head -n 1)
  [ -n "$keys_serial" ] || { echo "release-files.sh: keys.json has no serial" >&2; exit 1; }
fi

if [ "$mode" = resign ]; then
  [ -f latest.json ] || { echo "release-files.sh: no latest.json in $dir to re-sign" >&2; exit 1; }
  grep -q '"released":[0-9]*' latest.json && grep -q '"expires":[0-9]*' latest.json && grep -q '"keys_serial":[0-9]*' latest.json \
    || { echo "release-files.sh: this latest.json has no released/expires/keys_serial (an old manifest); rebuild it with release-files.sh $dir" >&2; exit 1; }
  sed -E -i "s/\"released\":[0-9]+/\"released\":$now/; s/\"expires\":[0-9]+/\"expires\":$exp/; s/\"keys_serial\":[0-9]+/\"keys_serial\":$keys_serial/" latest.json
  rm -f latest.json.minisig
  echo "== latest.json re-stamped (released $(date -u -d @"$now" +%F), expires $(date -u -d @"$exp" +%F), keys_serial $keys_serial); now sign it:"
  echo "   minisign -S -s KEY -m latest.json"
  exit 0
fi

shopt -s nullglob
bundles=(minerva-*-linux-x86_64.tar.gz)
[ "${#bundles[@]}" = 1 ] || { echo "release-files.sh: expected exactly one bundle in $dir, found ${#bundles[@]}" >&2; exit 1; }
bundle=${bundles[0]}
full=${bundle#minerva-}; full=${full%-linux-x86_64.tar.gz}
version=${full%%-*}
entry() { printf '{"name":"%s","sha256":"%s","size":%s}' "$1" "$(sha256sum "$1" | cut -d' ' -f1)" "$(stat -c %s "$1")"; }
sums=("$bundle")
others=()
for f in install.sh minerva-install.exe; do
  if [ -f "$f" ]; then sums+=("$f"); others+=("$f"); fi
done
sha256sum "${sums[@]}" >SHA256SUMS
{
  printf '{"schema":1,"version":"%s","full":"%s","released":%s,"expires":%s,"keys_serial":%s,"bundle":%s,"files":[' "$version" "$full" "$now" "$exp" "$keys_serial" "$(entry "$bundle")"
  sep=
  for f in "${others[@]}"; do printf '%s%s' "$sep" "$(entry "$f")"; sep=,; done
  printf ']}\n'
} >latest.json
echo "== SHA256SUMS, latest.json written in $dir (keys_serial $keys_serial; re-sign at least every 30 days: release-files.sh resign $dir)"
