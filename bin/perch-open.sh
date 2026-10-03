#!/usr/bin/env bash
# perch-open.sh: start (or reuse) the Minerva perch in WSL and open the web UI in the
# Windows browser. The perch token goes from its 0600 file into a 0600 redirect file and
# nowhere else: never argv, stdout, a log, a clipboard or a URL you type (web-ui.md 5.2).
#
# usage: bin/perch-open.sh [--no-browser] [--status] [--stop]
#   (none)         start or reuse the perch, write the redirect file, open it, print the url
#   --no-browser   same, but print `open file: <windows path>` instead of opening it
#   --status       is it up, its url and pid, session rows by state (counts only)
#   --stop         SIGTERM the perch, wait up to 15 s, report stopped / still running
# environment (all optional):
#   MINERVA_PERCH_BIN   default $HOME/perch-target/release/perch
#   MINERVA_PERCH_PORT  default 47820
#   MINERVA_UI_DIR      default <repo>/webui/term, found from this script's path
#   MINERVA_OPEN_TTL    seconds the redirect file lives, default 60

set -euo pipefail
umask 077

mode=open
browser=1
for a in "$@"; do
  case "$a" in
    --no-browser) browser=0 ;;
    --status) mode=status ;;
    --stop) mode=stop ;;
    -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "perch-open: unknown argument: $a (try --help)" >&2; exit 2 ;;
  esac
done

BIN=${MINERVA_PERCH_BIN:-$HOME/perch-target/release/perch}
PORT=${MINERVA_PERCH_PORT:-47820}
TTL=${MINERVA_OPEN_TTL:-60}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
UI=${MINERVA_UI_DIR:-$here/../webui/term}

case "$PORT" in ''|*[!0-9]*) echo "perch-open: MINERVA_PERCH_PORT must be a number" >&2; exit 1 ;; esac
case "$TTL" in ''|*[!0-9]*) echo "perch-open: MINERVA_OPEN_TTL must be a number" >&2; exit 1 ;; esac

if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
  echo "perch-open: XDG_RUNTIME_DIR is not set; the perch refuses to keep its token in a shared temp dir" >&2
  exit 1
fi
STATE=$XDG_RUNTIME_DIR/minerva
mkdir -p "$STATE"
chmod 700 "$STATE"
PIDF=$STATE/perch-$PORT.pid
OUTF=$STATE/perch-$PORT.out
LOGF=$STATE/perch-$PORT.log
TOKF=$STATE/perch-$PORT.token
OPENF=$STATE/open-$PORT.html
URL=http://127.0.0.1:$PORT/

alive() { [ -f "$PIDF" ] && kill -0 "$(<"$PIDF")" 2>/dev/null; }
listening() { [ -n "$(ss -ltn "sport = :$PORT" 2>/dev/null | tail -n +2)" ]; }

# GET <path> with the token in a 0600 header file (curl -H @file): the token is never in
# argv. Prints the body to stdout; returns curl's status. Callers get the http code via
# `hub_code`.
HDR=
mkhdr() {
  HDR=$(mktemp "$STATE/hdr.XXXXXX")
  printf 'Authorization: Bearer %s\n' "$(<"$TOKF")" >"$HDR"
}
rmhdr() { [ -n "$HDR" ] && rm -f "$HDR"; HDR=; }
trap rmhdr EXIT

hub_get() { # $1 = path; body on stdout
  mkhdr
  curl -sS --max-time 5 -H "@$HDR" "${URL}${1#/}" || { rmhdr; return 1; }
  rmhdr
}
hub_code() { # $1 = path; prints the http status
  mkhdr
  curl -s -o /dev/null -w '%{http_code}' --max-time 5 -H "@$HDR" "${URL}${1#/}" || true
  rmhdr
}

reusable() {
  alive && [ -f "$TOKF" ] && [ "$(hub_code /hub/sessions)" = 200 ]
}

if [ "$mode" = stop ]; then
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
  if kill -0 "$pid" 2>/dev/null; then
    echo "still running (pid $pid)"
    exit 1
  fi
  rm -f "$PIDF"
  if [ -e "$TOKF" ]; then echo "stopped (pid $pid), but the token file is still there: $TOKF"; exit 1; fi
  echo "stopped (pid $pid); token file gone"
  exit 0
fi

if [ "$mode" = status ]; then
  if reusable; then
    body=$(hub_get /hub/sessions)
    counts=$(printf '%s' "$body" | grep -o '"state":"[a-z-]*"' | sed 's/"state":"//;s/"//' | sort | uniq -c \
      | awk '{printf "%s%s %s", (n++?" · ":""), $2, $1}')
    echo "up: $URL (pid $(<"$PIDF"))"
    echo "sessions: ${counts:-none}"
  else
    if alive; then echo "pid $(<"$PIDF") is alive but did not answer /hub/sessions on $URL"; else echo "down: nothing of ours on port $PORT"; fi
    exit 1
  fi
  exit 0
fi

# --- open ---
reused=
if reusable; then
  reused=" (reused)"
else
  if [ ! -x "$BIN" ]; then
    echo "perch-open: the perch binary is missing: $BIN" >&2
    echo "build it:  cd perch && CARGO_TARGET_DIR=\$HOME/perch-target cargo build --release" >&2
    exit 1
  fi
  if listening; then
    echo "perch-open: port $PORT is already in use by something that is not our perch; set MINERVA_PERCH_PORT" >&2
    exit 1
  fi
  [ -d "$UI" ] || { echo "perch-open: UI dir not found: $UI" >&2; exit 1; }
  UI=$(cd "$UI" && pwd)
  rm -f "$PIDF" "$OUTF" "$LOGF" "$TOKF"
  # No --sessions-dir (the perch's default must equal the door's own) and no --root.
  setsid nohup "$BIN" --bind "127.0.0.1:$PORT" --ui-dir "$UI" >"$OUTF" 2>"$LOGF" </dev/null &
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
    echo "perch-open: the perch did not come up in 20 s; last lines of its log:" >&2
    tail -n 20 "$LOGF" >&2 || true
    exit 1
  fi
  # setsid/nohup exec in place when the shell is not a group leader, so $! is the perch;
  # confirm by the listening socket and prefer that pid.
  pid=$(ss -ltnp "sport = :$PORT" 2>/dev/null | grep -o 'pid=[0-9]*' | head -n1 | cut -d= -f2 || true)
  printf '%s\n' "${pid:-$spid}" >"$PIDF"
fi

# Redirect file: the only place the token is written besides its own file.
rm -f "$OPENF"
printf '<!doctype html><meta charset="utf-8"><meta name="referrer" content="no-referrer">\n<title>Minerva</title><script>location.replace("http://127.0.0.1:%s/#token=%s")</script>\n' \
  "$PORT" "$(<"$TOKF")" >"$OPENF"
chmod 600 "$OPENF"
WINP=$(wslpath -w "$OPENF")

if [ "$browser" = 1 ]; then
  powershell.exe -NoProfile -NonInteractive -Command "Start-Process -FilePath '$WINP'" >/dev/null
else
  echo "open file: $WINP (deleted in ${TTL}s)"
fi
setsid nohup bash -c 'sleep "$1"; rm -f "$2"' _ "$TTL" "$OPENF" >/dev/null 2>&1 </dev/null &
disown 2>/dev/null || true

echo "minerva perch on $URL (pid $(<"$PIDF"), log $LOGF)$reused"
