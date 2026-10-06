#!/bin/sh
# chromium: starts the bundled headless Chromium with its own libraries and fonts, so it needs nothing from the system
# but the C library. It is what eidolon-browser finds on PATH (the browser plugin looks for `chromium`).
self=$(readlink -f "$0")
d=$(cd "$(dirname "$self")/../lib/chromium" && pwd)
export LD_LIBRARY_PATH="$d/syslib:$d${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# A machine with no fonts renders no text (read/innerText comes back empty): point fontconfig at the bundled DejaVu.
conf=$d/fonts.conf
if [ ! -f "$conf" ]; then
  conf=${XDG_CACHE_HOME:-$HOME/.cache}/minerva/fonts.conf
  mkdir -p "$(dirname "$conf")"
fi
if [ ! -f "$conf" ]; then
  tmp=$conf.$$
  printf '<?xml version="1.0"?>\n<!DOCTYPE fontconfig SYSTEM "fonts.dtd">\n<fontconfig>\n<dir>%s/fonts</dir>\n<dir>/usr/share/fonts</dir>\n<cachedir>%s/minerva/fontconfig</cachedir>\n</fontconfig>\n' \
    "$d" "${XDG_CACHE_HOME:-$HOME/.cache}" >"$tmp" && mv "$tmp" "$conf"
fi
export FONTCONFIG_FILE=$conf
exec "$d/chrome-headless-shell" "$@"
