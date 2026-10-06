#!/usr/bin/env bash
# perch-open.sh: start (or reuse) the Minerva server (the perch) in WSL and sign a browser in.
# The perch token never leaves its 0600 file: this script reads it for curl through `-H @file`
# (never argv, env, stdout or a log), and browsers get a 30 s single-use login code instead
# (`POST /hub/code`), which the page trades for an HttpOnly cookie (docs/design/hub.md 10.5).
# `hoot` (bin/hoot.ps1) runs this with --code and opens the one URL line it prints.
#
# usage: bin/perch-open.sh [--code] [--no-browser] [--status] [--stop]
#   --code         start or reuse the perch, mint a login code, print exactly one line on stdout:
#                  http://127.0.0.1:<port>/#code=<code>   (everything else goes to stderr)
#   (none)         start or reuse the perch, mint a code and open the URL in the Windows browser
#   --no-browser   start or reuse the perch and print its address; no code, no browser
#   --status       is it up, its url and pid, session rows by state (counts only)
#   --stop         SIGTERM the perch, wait up to 15 s, report stopped / still running
# environment (all optional):
#   MINERVA_PERCH_BIN   default $HOME/perch-target/release/perch. Build it in WSL with:
#                         cd perch && CARGO_TARGET_DIR=$HOME/perch-target cargo build --release
#   MINERVA_PERCH_PORT  default 47820
#   MINERVA_UI_DIR      default <repo>/webui/term, found from this script's path
#   MINERVA_HOME        the Minerva home, default $HOME/Minerva: sessions/, scratch/, projects.json and
#                       minerva.toml created when missing; the perch regenerates config.toml at every start
#   MINERVA_SETUP_BIN   default $HOME/setup-target/release/minerva-setup (the wizard; see setup/README.md)
#   Roots a chat may work in: the `roots` that `minerva setup` saved in <home>/minerva.toml; without them $HOME,
#   the Minerva home and the Windows profile folder (USERPROFILE).

set -euo pipefail
umask 077

mode=open
browser=1
want_code=1
code_only=0
for a in "$@"; do
  case "$a" in
    --code) code_only=1; browser=0 ;;
    --no-browser) browser=0; want_code=0 ;;
    --status) mode=status ;;
    --stop) mode=stop ;;
    -h|--help) sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "perch-open: unknown argument: $a (try --help)" >&2; exit 2 ;;
  esac
done
if [ "$code_only" = 1 ]; then want_code=1; fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Installed mode: this script sits in <install>/bin next to the prebuilt perch (a release bundle, installed by
# install.sh or minerva-install.exe). Then the binaries and the UI come from the install, and its bin/ goes first on
# PATH so the doors the perch starts are the bundled eidolon. In a repo checkout bin/ has no perch binary, so
# nothing changes there. The MINERVA_* variables below still win in both modes.
if [ -x "$here/perch" ]; then
  BIN=${MINERVA_PERCH_BIN:-$here/perch}
  SETUP_BIN=${MINERVA_SETUP_BIN:-$here/minerva-setup}
  export PATH="$here:$PATH"
else
  BIN=${MINERVA_PERCH_BIN:-$HOME/perch-target/release/perch}
  SETUP_BIN=${MINERVA_SETUP_BIN:-$HOME/setup-target/release/minerva-setup}
fi
PORT=${MINERVA_PERCH_PORT:-47820}
UI=${MINERVA_UI_DIR:-$here/../webui/term}
MHOME=${MINERVA_HOME:-$HOME/Minerva}

case "$PORT" in ''|*[!0-9]*) echo "perch-open: MINERVA_PERCH_PORT must be a number" >&2; exit 1 ;; esac

# Installed mode only: keep the bundled plugins current and trusted, and run their services (the browser, and jev once
# set up). Quiet and best effort: a failure here never stops the perch from starting.
if [ -x "$here/perch" ] && [ -x "$here/minerva" ] && [ "${MINERVA_NO_PLUGIN_SYNC:-}" = "" ]; then
  case "$mode" in
    stop) "$here/minerva" services stop --quiet || true ;;
    status) ;;
    *) "$here/minerva" plugins sync --if-needed --quiet || true; "$here/minerva" services start --quiet || true ;;
  esac
fi

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
    counts=$(printf '%s' "$body" | { grep -o '"state":"[a-z-]*"' || true; } | sed 's/"state":"//;s/"//' | sort | uniq -c \
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
  # An old build lacks --home or --eidolon-config and would refuse the start with a usage error: say
  # what to do instead. The help is captured first: a pipe into `grep -q` can die of SIGPIPE under pipefail.
  help=$("$BIN" --help 2>&1 || true)
  for flag in --home --eidolon-config; do
    case $help in
      *"$flag"*) ;;
      *)
        echo "perch-open: $BIN is an old build (it has no $flag); rebuild it in WSL:" >&2
        echo "  cd $(cd "$here/.." && pwd)/perch && CARGO_TARGET_DIR=\$HOME/perch-target cargo build --release" >&2
        exit 1
        ;;
    esac
  done
  if listening; then
    echo "perch-open: port $PORT is already in use by something that is not our perch; set MINERVA_PERCH_PORT" >&2
    exit 1
  fi
  [ -d "$UI" ] || { echo "perch-open: UI dir not found: $UI" >&2; exit 1; }
  UI=$(cd "$UI" && pwd)
  rm -f "$PIDF" "$OUTF" "$LOGF" "$TOKF"
  # The Minerva home: UI sessions live there, not in eidolon's shared sessions dir. The dirs,
  # projects.json and minerva.toml are created only when missing. config.toml is NOT written here: the
  # perch generates it at every start from ~/.config/eidolon/config.toml (every key kept), with
  # sessions_dir forced to <home>/sessions and default_model filled from minerva.toml when the member's
  # config names none. Edit ~/.config/eidolon/config.toml or minerva.toml, never <home>/config.toml.
  case "$MHOME" in /*) ;; *) echo "perch-open: MINERVA_HOME must be an absolute path: $MHOME" >&2; exit 1 ;; esac
  mkdir -p "$MHOME/sessions" "$MHOME/scratch"
  if [ ! -e "$MHOME/minerva.toml" ]; then printf 'default_model = "chatgpt:gpt-5.6-luna"\n' >"$MHOME/minerva.toml"; fi
  if [ ! -e "$MHOME/projects.json" ]; then printf '[]\n' >"$MHOME/projects.json"; fi
  # Roots: $HOME, plus the Windows profile when it can be found (so a pasted C:\Users\you\...
  # folder is allowed); --home adds the Minerva home as one more root. The profile folder is asked of
  # cmd.exe (run from /mnt/c, so it does not complain about a UNC working directory) and mapped by
  # wslpath, so a renamed account or a custom automount root still resolves; without one, no extra root.
  #
  # Once `minerva setup` has run, its choice wins: `roots` in <home>/minerva.toml, read by
  # `minerva-setup --print-roots` (one existing folder per line; no TOML parsing in bash). Without the file,
  # without the built wizard or with an empty list, the defaults above apply.
  roots=()
  if [ -f "$MHOME/minerva.toml" ]; then
    if [ -x "$SETUP_BIN" ]; then
      while IFS= read -r line; do
        case "$line" in /*) roots+=(--root "$line") ;; esac
      done < <(MINERVA_HOME="$MHOME" "$SETUP_BIN" --print-roots || echo "perch-open: could not read the roots in $MHOME/minerva.toml; using the defaults" >&2)
    else
      echo "perch-open: $SETUP_BIN is not built, so the roots in minerva.toml are ignored (cd setup && CARGO_TARGET_DIR=\$HOME/setup-target cargo build --release)" >&2
    fi
  fi
  if [ "${#roots[@]}" = 0 ]; then
    roots=(--root "$HOME")
    winprofile=$(cd /mnt/c 2>/dev/null && cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null | tr -d '\r\n' || true)
    winroot=
    case "$winprofile" in ''|'%USERPROFILE%') ;; *) winroot=$(wslpath -u "$winprofile" 2>/dev/null || true) ;; esac
    if [ -n "$winroot" ] && [ -d "$winroot" ]; then
      roots+=(--root "$winroot")
    else
      echo "perch-open: no Windows profile folder found; pasted C:\... folders will be refused" >&2
    fi
  fi
  setsid nohup "$BIN" --bind "127.0.0.1:$PORT" --ui-dir "$UI" "${roots[@]}" --home "$MHOME" \
    >"$OUTF" 2>"$LOGF" </dev/null &
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

if [ "$want_code" = 0 ]; then
  echo "minerva perch on $URL (pid $(<"$PIDF"), log $LOGF)$reused"
  exit 0
fi

# A login code: 30 s, single use, minted behind the token. The token goes to curl through the
# 0600 header file only. An old perch (started before codes existed) answers 404 here.
mkhdr
reply=$(curl -sS --max-time 5 -X POST -H "@$HDR" -w '\n%{http_code}' "${URL}hub/code") || { rmhdr; echo "perch-open: the perch did not answer POST /hub/code" >&2; exit 1; }
rmhdr
status=${reply##*$'\n'}
body=${reply%$'\n'*}
if [ "$status" = 404 ]; then
  echo "perch-open: the running perch is too old for login codes; stop it (perch-open.sh --stop) and run this again" >&2
  exit 1
fi
if [ "$status" != 200 ]; then echo "perch-open: POST /hub/code answered $status" >&2; exit 1; fi
code=$(printf '%s' "$body" | sed -n 's/.*"code":"\([A-Za-z0-9_-]*\)".*/\1/p')
[ -n "$code" ] || { echo "perch-open: the perch gave no code" >&2; exit 1; }
LINK="${URL}#code=$code"

if [ "$code_only" = 1 ]; then
  printf '%s\n' "$LINK"
  exit 0
fi
# Opened by the Windows shell: the URL is the only thing in argv, and it is a code that is dead in
# 30 s or at first use.
powershell.exe -NoProfile -NonInteractive -Command "Start-Process '$LINK'" >/dev/null
echo "minerva perch on $URL (pid $(<"$PIDF"), log $LOGF)$reused"
