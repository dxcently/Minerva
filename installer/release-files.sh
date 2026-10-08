#!/usr/bin/env bash
# release-files.sh [resign] DIR
#   release-files.sh DIR           write SHA256SUMS and latest.json for the release files already in DIR (the bundle, install.sh,
#                                  minerva-install.exe when present). latest.json carries `released` (now), `expires` (now + 30
#                                  days) and `keys_serial` (the serial of DIR/keys.json, 0 when there is none).
#   release-files.sh resign DIR    re-stamp the existing latest.json with a fresh `released`/`expires` and the current keys_serial,
#                                  WITHOUT rebuilding anything. Then sign it again (see below). Do this at least every 30 days:
#                                  a manifest older than that still works, but members are told "release info is stale".
# Both modes first check DIR/keys.json, before anything is written: a keys.json that revokes an id with no revoke_fp entry for it, names
# a fingerprint that is not 16 hex digits, or has a revoke/revoke_fp list it cannot read is refused (exit 1, one sentence, nothing written).
# Neither signs nor publishes: signing is the operator's step (installer/README.md, "Releasing"):
#   minisign -S -s KEY -m latest.json SHA256SUMS minerva-*-linux-x86_64.tar.gz     (the bundle's .minisig only on a rebuild)
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

# A client that does not hold a revoked key refuses the whole keys.json unless revoke_fp gives that key's fingerprint, so the file is refused
# here, before anything is written. No JSON tool is assumed: the lists are read with sed, tr and grep. A list that cannot be read refuses
# the file too, and so does a keys.json that is not one object. Ids and fingerprints are compared without case.
refuse_keys() {
  echo "release-files.sh: $1; nothing was written" >&2
  exit 1
}
if [ -f keys.json ]; then
  flat=$(tr '\n\r\t' '   ' <keys.json) || refuse_keys "cannot read keys.json"
  grep -Eq '^[[:space:]]*[{].*[}][[:space:]]*$' <<<"$flat" || refuse_keys "keys.json is not one JSON object"
  for name in revoke revoke_fp; do
    n=$({ grep -o "\"$name\"[[:space:]]*:" <<<"$flat" || true; } | wc -l)
    [ "$n" -le 1 ] || refuse_keys "keys.json names $name more than once"
    [ "$n" -eq 0 ] || grep -Eq "\"$name\"[[:space:]]*:[[:space:]]*\[[^]]*\]" <<<"$flat" || refuse_keys "the $name list in keys.json cannot be read"
  done
  rev_in=$(sed -n 's/.*"revoke"[[:space:]]*:[[:space:]]*\[\([^]]*\)\].*/\1/p' <<<"$flat")
  fp_in=$(sed -n 's/.*"revoke_fp"[[:space:]]*:[[:space:]]*\[\([^]]*\)\].*/\1/p' <<<"$flat")
  # revoke: key ids only, comma-separated; each id is 16 hex digits
  grep -Eq '^[[:space:]]*("[0-9A-Fa-f]{16}"([[:space:]]*,[[:space:]]*"[0-9A-Fa-f]{16}")*)?[[:space:]]*$' <<<"$rev_in" \
    || refuse_keys "the revoke list in keys.json cannot be read"
  rev_ids=$({ grep -o '"[0-9A-Fa-f]\{16\}"' <<<"$rev_in" || true; } | tr -d '"' | tr '[:lower:]' '[:upper:]')
  # revoke_fp: objects only, each with an id and a fingerprint (16 hex digits, spaces allowed) and nothing else
  [ -z "$(sed 's/[{][^{}]*[}]//g; s/[[:space:],]//g' <<<"$fp_in")" ] || refuse_keys "the revoke_fp list in keys.json cannot be read"
  fp_ids=
  objs=$({ grep -o '[{][^{}]*[}]' <<<"$fp_in" || true; })
  if [ -n "$objs" ]; then
    while IFS= read -r obj; do
      id=$(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<<"$obj")
      fp=$(sed -n 's/.*"fp"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<<"$obj" | tr -d ' \t')
      rest=$(sed 's/"id"[[:space:]]*:[[:space:]]*"[^"]*"//; s/"fp"[[:space:]]*:[[:space:]]*"[^"]*"//; s/[{}[:space:],]//g' <<<"$obj")
      { [ -z "$rest" ] && grep -Eq '^[0-9A-Fa-f]{16}$' <<<"$id" && grep -Eq '^[0-9A-Fa-f]{16}$' <<<"$fp"; } \
        || refuse_keys "the revoke_fp list in keys.json cannot be read"
      uid=$(tr '[:lower:]' '[:upper:]' <<<"$id")
      fp_ids="$fp_ids$uid"$'\n'
    done <<<"$objs"
  fi
  for id in $rev_ids; do
    grep -Fxq "$id" <<<"$fp_ids" || refuse_keys "keys.json revokes $id without its fingerprint; the maintainer lists every revoked key in revoke_fp"
  done
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
