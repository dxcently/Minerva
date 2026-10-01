#!/usr/bin/env bash
# fb.sh <open|snapshot|click|back> [arg]: a four-page fake of en.wikipedia.org, so wiki-hop
# runs without Chromium. It answers the shapes the real tools do, from site/, and keeps
# "where am I" in $FB_STATE.
here=$(cd "$(dirname "$0")" && pwd)
st=${FB_STATE:-/tmp/jevrig/fb}
mkdir -p "$st"
cur() { cat "$st/cur" 2>/dev/null; }
case "$1" in
open)
  name=${2##*/wiki/}
  [ -f "$here/site/$name.txt" ] || { echo "no such page: $2" >&2; exit 1; }
  echo "$name" > "$st/cur"; : > "$st/hist"
  echo "opened https://en.wikipedia.org/wiki/$name" ;;
snapshot)
  name=$(cur); [ -n "$name" ] || { echo "no page open" >&2; exit 1; }
  title=${name//_/ }
  body=$(cat "$here/site/$name.txt")
  printf 'url: https://en.wikipedia.org/wiki/%s\ntitle: %s - Wikipedia\nheading: %s\nscope: main\nchars: %d\n\n%s' "$name" "$title" "$title" "${#body}" "$body" ;;
click)
  name=$(cur); [ -n "$name" ] || { echo "no page open" >&2; exit 1; }
  url=$(awk -v r="[ref=$2]" 'f && /- \/url:/ {sub(/.*- \/url: */,""); print; exit} index($0, r) {f=1}' "$here/site/$name.txt")
  [ -n "$url" ] || { echo "stale or unknown ref $2" >&2; exit 1; }
  tgt=${url##*/wiki/}
  echo "$name" >> "$st/hist"; echo "$tgt" > "$st/cur"
  echo "clicked $2 -> https://en.wikipedia.org/wiki/$tgt" ;;
back)
  prev=$(tail -n 1 "$st/hist" 2>/dev/null); [ -n "$prev" ] || { echo "no history" >&2; exit 1; }
  sed -i '$d' "$st/hist"; echo "$prev" > "$st/cur"
  echo "back to https://en.wikipedia.org/wiki/$prev" ;;
*) echo "usage: fb.sh open|snapshot|click|back [arg]" >&2; exit 2 ;;
esac
