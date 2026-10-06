#!/usr/bin/env bash
# minerva: the desktop command of a Nix install. Verbs: web, status, stop, init, plugins, services, update, version.
# Store paths are substituted at build time; only coreutils-class tools come from PATH.
# Behaviour follows bin/perch-open.sh: the perch token never leaves its 0600 file (curl reads it through
# `-H @file`), and a browser gets a 30 s single-use login code (`POST /hub/code`), never the token.
set -euo pipefail
umask 077

VERSION=@version@
PERCH_BIN=${MINERVA_PERCH_BIN:-@perch@/bin/perch}
SETUP_BIN=${MINERVA_SETUP_BIN:-@setup@/bin/minerva-setup}
EIDOLON_BIN=${MINERVA_EIDOLON_BIN:-@eidolon@/bin/eidolon}
UI=${MINERVA_UI_DIR:-@webui@/share/minerva/webui-term}
# The bundled plugins (browser, jev, subagent): installed into the member's eidolon, trusted and refreshed by minerva-core,
# the same code the Linux installer runs. Its record of what is ours lives under the user's state dir, never in the store.
CORE=@core@/bin/minerva-core
PLUGINS=${MINERVA_PLUGINS_DIR:-@plugins@/share/minerva/plugins}
PLUGIN_BIN=@browser@/bin
PLUGIN_STATE=${XDG_STATE_HOME:-$HOME/.local/state}/minerva-nix
core() {
  "$CORE" "$@" --eidolon "$EIDOLON_BIN" --src "$PLUGINS" --bin-dir "$PLUGIN_BIN" --prefix "$PLUGIN_STATE" --version "$VERSION"
}
PORT=${MINERVA_PERCH_PORT:-47820}
MHOME=${MINERVA_HOME:-$HOME/Minerva}
PROFILE_NAME=minerva

usage() {
  cat <<USAGE
minerva $VERSION (installed with Nix)

usage: minerva <verb> [options]
  web [--code|--no-browser]  start or reuse the Minerva server and open it in a browser
                             --code        print one login URL, open nothing
                             --no-browser  print the address only
  status                     is it up, its url and pid, session counts by state
  stop                       stop the server
  init                       the first-run wizard (home folder, default model, project roots)
  plugins sync|status        install/refresh the bundled plugins (browser, subagent, jev) and trust them
  services start|stop|status the browser service the browser plugin talks to
  update                     print how to update (a Nix install does not update itself)
  version                    print the version

environment: MINERVA_HOME (default ~/Minerva), MINERVA_PERCH_PORT (default 47820)
USAGE
}

verb=${1:-web}
if [ $# -gt 0 ]; then shift; fi

case "$verb" in
  -h|--help|help) usage; exit 0 ;;
  version|--version|-V) echo "minerva $VERSION"; exit 0 ;;
  update)
    echo "installed with Nix; update with: nix profile upgrade $PROFILE_NAME"
    echo "(from a flake input: nix flake update minerva, then rebuild)"
    exit 0 ;;
  init)
    MINERVA_HOME=$MHOME exec "$SETUP_BIN" "$@" ;;
  plugins) sub=${1:-status}; if [ $# -gt 0 ]; then shift; fi; exec "$CORE" plugins "$sub" "$@" --eidolon "$EIDOLON_BIN" --src "$PLUGINS" --bin-dir "$PLUGIN_BIN" --prefix "$PLUGIN_STATE" --version "$VERSION" ;;
  services) sub=${1:-status}; if [ $# -gt 0 ]; then shift; fi; exec "$CORE" services "$sub" "$@" --eidolon "$EIDOLON_BIN" --src "$PLUGINS" --bin-dir "$PLUGIN_BIN" --prefix "$PLUGIN_STATE" --version "$VERSION" ;;
  web|status|stop) ;;
  *) echo "minerva: unknown verb: $verb (try minerva help)" >&2; exit 2 ;;
esac

browser=1
want_code=1
code_only=0
for a in "$@"; do
  case "$a" in
    --code) code_only=1; browser=0 ;;
    --no-browser) browser=0; want_code=0 ;;
    *) echo "minerva: unknown argument: $a (try minerva help)" >&2; exit 2 ;;
  esac
done
if [ "$code_only" = 1 ]; then want_code=1; fi

case "$PORT" in ''|*[!0-9]*) echo "minerva: MINERVA_PERCH_PORT must be a number" >&2; exit 1 ;; esac
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
  echo "minerva: XDG_RUNTIME_DIR is not set; the perch refuses to keep its token in a shared temp dir" >&2
  exit 1
fi
STATE=$XDG_RUNTIME_DIR/minerva
mkdir -p "$STATE"
chmod 700 "$STATE"
PIDF=$STATE/perch-$PORT.pid
OUTF=$STATE/perch-$PORT.out
LOGF=$STATE/perch-$PORT.log
TOKF=$STATE/perch-$PORT.token
URL=http://127.0.0.1:$PORT/

alive() { [ -f "$PIDF" ] && kill -0 "$(<"$PIDF")" 2>/dev/null; }
listening() { [ -n "$(ss -ltn "sport = :$PORT" 2>/dev/null | tail -n +2)" ]; }

HDR=
mkhdr() {
  HDR=$(mktemp "$STATE/hdr.XXXXXX")
  printf 'Authorization: Bearer %s\n' "$(<"$TOKF")" >"$HDR"
}
rmhdr() { if [ -n "$HDR" ]; then rm -f "$HDR"; fi; HDR=; }
trap rmhdr EXIT

hub_get() {
  mkhdr
  curl -sS --max-time 5 -H "@$HDR" "${URL}${1#/}" || { rmhdr; return 1; }
  rmhdr
}
hub_code() {
  mkhdr
  curl -s -o /dev/null -w '%{http_code}' --max-time 5 -H "@$HDR" "${URL}${1#/}" || true
  rmhdr
}
reusable() { alive && [ -f "$TOKF" ] && [ "$(hub_code /hub/sessions)" = 200 ]; }

if [ "$verb" = stop ]; then
  core services stop --quiet || true
  if ! alive; then
    echo "not running on port $PORT"
    rm -f "$PIDF"
    exit 0
  fi
  pid=$(<"$PIDF")
  kill -TERM "$pid" 2>/dev/null || true
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  if kill -0 "$pid" 2>/dev/null; then echo "still running (pid $pid)"; exit 1; fi
  rm -f "$PIDF"
  if [ -e "$TOKF" ]; then echo "stopped (pid $pid), but the token file is still there: $TOKF"; exit 1; fi
  echo "stopped (pid $pid); token file gone"
  exit 0
fi

if [ "$verb" = status ]; then
  if reusable; then
    body=$(hub_get /hub/sessions)
    counts=$(printf '%s' "$body" | { grep -o '"state":"[a-z-]*"' || true; } | sed 's/"state":"//;s/"//' | sort | uniq -c \
      | awk '{printf "%s%s %s", (n++?" · ":""), $2, $1}')
    echo "up: $URL (pid $(<"$PIDF"))"
    echo "sessions: ${counts:-none}"
    exit 0
  fi
  if alive; then echo "pid $(<"$PIDF") is alive but did not answer /hub/sessions on $URL"; else echo "down: nothing of ours on port $PORT"; fi
  exit 1
fi

# --- web ---
# Best effort and quiet: a failure here never stops the perch from starting (minerva plugins sync says what went wrong).
if [ -z "${MINERVA_NO_PLUGIN_SYNC:-}" ]; then
  core plugins sync --if-needed --quiet || true
  core services start --quiet || true
fi
reused=
if reusable; then
  reused=" (reused)"
else
  if listening; then
    echo "minerva: port $PORT is already in use by something that is not our perch; set MINERVA_PERCH_PORT" >&2
    exit 1
  fi
  rm -f "$PIDF" "$OUTF" "$LOGF" "$TOKF"
  case "$MHOME" in /*) ;; *) echo "minerva: MINERVA_HOME must be an absolute path: $MHOME" >&2; exit 1 ;; esac
  mkdir -p "$MHOME/sessions" "$MHOME/scratch"
  if [ ! -e "$MHOME/minerva.toml" ]; then printf 'default_model = "chatgpt:gpt-5.6-luna"\n' >"$MHOME/minerva.toml"; fi
  if [ ! -e "$MHOME/projects.json" ]; then printf '[]\n' >"$MHOME/projects.json"; fi
  # Roots: what `minerva init` saved; else $HOME, plus the Windows profile under WSL.
  roots=()
  while IFS= read -r line; do
    case "$line" in /*) roots+=(--root "$line") ;; esac
  done < <(MINERVA_HOME="$MHOME" "$SETUP_BIN" --print-roots 2>/dev/null || true)
  if [ "${#roots[@]}" = 0 ]; then
    roots=(--root "$HOME")
    if grep -qi microsoft /proc/version 2>/dev/null; then
      winprofile=$(cd /mnt/c 2>/dev/null && cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null | tr -d '\r\n' || true)
      winroot=
      case "$winprofile" in ''|'%USERPROFILE%') ;; *) winroot=$(wslpath -u "$winprofile" 2>/dev/null || true) ;; esac
      if [ -n "$winroot" ] && [ -d "$winroot" ]; then roots+=(--root "$winroot"); fi
    fi
  fi
  setsid nohup "$PERCH_BIN" --bind "127.0.0.1:$PORT" --ui-dir "$UI" --eidolon "$EIDOLON_BIN" \
    "${roots[@]}" --home "$MHOME" >"$OUTF" 2>"$LOGF" </dev/null &
  spid=$!
  tokline=
  for _ in $(seq 1 40); do
    tokline=$(sed -n '2p' "$OUTF" 2>/dev/null || true)
    case "$tokline" in "token file: "*) [ -s "$TOKF" ] && break ;; esac
    tokline=
    kill -0 "$spid" 2>/dev/null || break
    sleep 0.5
  done
  if [ -z "$tokline" ]; then
    echo "minerva: the perch did not come up in 20 s; last lines of its log:" >&2
    tail -n 20 "$LOGF" >&2 || true
    exit 1
  fi
  pid=$(ss -ltnp "sport = :$PORT" 2>/dev/null | grep -o 'pid=[0-9]*' | head -n1 | cut -d= -f2 || true)
  printf '%s\n' "${pid:-$spid}" >"$PIDF"
fi

if [ "$want_code" = 0 ]; then
  echo "minerva perch on $URL (pid $(<"$PIDF"), log $LOGF)$reused"
  exit 0
fi

mkhdr
reply=$(curl -sS --max-time 5 -X POST -H "@$HDR" -w '\n%{http_code}' "${URL}hub/code") || { rmhdr; echo "minerva: the perch did not answer POST /hub/code" >&2; exit 1; }
rmhdr
status=${reply##*$'\n'}
body=${reply%$'\n'*}
if [ "$status" != 200 ]; then echo "minerva: POST /hub/code answered $status" >&2; exit 1; fi
code=$(printf '%s' "$body" | sed -n 's/.*"code":"\([A-Za-z0-9_-]*\)".*/\1/p')
[ -n "$code" ] || { echo "minerva: the perch gave no code" >&2; exit 1; }
LINK="${URL}#code=$code"

if [ "$code_only" = 1 ]; then printf '%s\n' "$LINK"; exit 0; fi
if grep -qi microsoft /proc/version 2>/dev/null; then
  powershell.exe -NoProfile -NonInteractive -Command "Start-Process '$LINK'" >/dev/null
elif command -v xdg-open >/dev/null 2>&1; then
  xdg-open "$LINK" >/dev/null 2>&1 || echo "open this in a browser: $LINK"
else
  echo "open this in a browser: $LINK"
fi
echo "minerva perch on $URL (pid $(<"$PIDF"), log $LOGF)$reused"
