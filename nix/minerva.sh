#!/usr/bin/env bash
# minerva: the desktop command of a Nix install. Verbs: web, status, stop, init, plugins, services, keys, update, version.
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
DEFAULT_PORT=${MINERVA_PERCH_PORT:-47820}
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
                             web, status and stop take --port P (1 to 65535) in place of MINERVA_PERCH_PORT; a server started
                             on a port other than that default is a second one and gets no agent token files
  init                       the first-run wizard (home folder, default model, project roots)
  plugins sync|status        install/refresh the bundled plugins (browser, subagent, jev) and trust them
  services start|stop|status the browser service the browser plugin talks to
  keys allow-web FP          let the web page add or remove the update key with fingerprint FP (16 hex digits, spaces
                             allowed), once, within 2 minutes; --port P --token-file F is for tests (F needs P)
  update                     print how to update (a Nix install does not update itself)
  version                    print the version

environment: MINERVA_HOME (default ~/Minerva), MINERVA_PERCH_PORT (default 47820)
USAGE
}

verb=${1:-web}
if [ $# -gt 0 ]; then shift; fi

# keys allow-web FP: the arm of the web page's update-key step (the same as bin/perch-open.sh --keys-arm; keep the two in step).
arm=0
arm_fp=
opt_port=
arm_tokfile=
if [ "$verb" = keys ]; then
  if [ "${1:-}" != allow-web ]; then echo "minerva: only 'keys allow-web FP' is available here (try minerva help)" >&2; exit 2; fi
  shift
  arm=1
  while [ $# -gt 0 ]; do
    case "$1" in
      --port|--token-file)
        if [ $# -lt 2 ] || [ -z "$2" ]; then echo "minerva: $1 needs a value" >&2; exit 2; fi
        if [ "$1" = --port ]; then opt_port=$2; else arm_tokfile=$2; fi
        shift 2 ;;
      -*) echo "minerva: unknown argument: $1 (try minerva help)" >&2; exit 2 ;;
      *) arm_fp="$arm_fp$1"; shift ;;
    esac
  done
  # A token file is one perch's token: it is never read without the port that perch answers on.
  if [ -n "$arm_tokfile" ] && [ -z "$opt_port" ]; then echo "minerva: --token-file needs --port too" >&2; exit 2; fi
fi

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
  web|status|stop|keys) ;;
  *) echo "minerva: unknown verb: $verb (try minerva help)" >&2; exit 2 ;;
esac

browser=1
want_code=1
code_only=0
while [ $# -gt 0 ]; do
  case "$1" in
    --code) code_only=1; browser=0 ;;
    --no-browser) browser=0; want_code=0 ;;
    --port)
      if [ $# -lt 2 ] || [ -z "$2" ]; then echo "minerva: --port needs a value" >&2; exit 2; fi
      opt_port=$2
      shift ;;
    *) echo "minerva: unknown argument: $1 (try minerva help)" >&2; exit 2 ;;
  esac
  shift
done
if [ "$code_only" = 1 ]; then want_code=1; fi

# One spelling of a port: 1 to 65535, digits only, no leading zero (bash would read 047820 as octal, and a long run of digits
# wraps). Neither value is echoed back.
PORT=${opt_port:-$DEFAULT_PORT}
if ! { [[ $PORT =~ ^[1-9][0-9]{0,4}$ ]] && (( PORT <= 65535 )); }; then
  if [ -n "$opt_port" ]; then echo "minerva: --port must be a port number, 1 to 65535, without leading zeros" >&2; else echo "minerva: MINERVA_PERCH_PORT must be a port number, 1 to 65535, without leading zeros" >&2; fi
  exit 1
fi
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

# The token reaches curl through ONE 0600 header file (curl -H @file) and nowhere else: not argv, not the environment, not output,
# not xtrace. It is made here, in this shell, before anything is probed, and the EXIT trap removes it on every way out (a signal
# included); a command substitution only ever uses it. mkhdr fills it from a token file after checking the file is one the perch
# wrote, so EVERY header built from a token file, on every path, has passed the same checks first. (Keep in step with
# bin/perch-open.sh.)
HDR=
rmhdr() { if [ -n "$HDR" ]; then rm -f -- "$HDR"; fi; }
trap rmhdr EXIT
HDR=$(mktemp "$STATE/hdr.XXXXXX")

# mkhdr <token file>: 0 when the header file now holds that token, 1 (with the reason on stderr, never the token) when not.
mkhdr() {
  local f=$1 tok
  if [ ! -f "$f" ] || [ -L "$f" ]; then echo "minerva: the token file is not a plain file" >&2; return 1; fi
  case "$(stat -c '%u %a' -- "$f")" in
    "$(id -u) 600"|"$(id -u) 400") ;;
    *) echo "minerva: the token file is not yours alone (owner and mode 0600 are required)" >&2; return 1 ;;
  esac
  { set +x; } 2>/dev/null
  tok=$(<"$f")
  if ! [[ $tok =~ ^[A-Za-z0-9_-]{16,128}$ ]]; then echo "minerva: the token file does not hold a token" >&2; return 1; fi
  printf 'Authorization: Bearer %s\n' "$tok" >"$HDR"
}

# curl for anything that carries the token: -q first (no ~/.curlrc, whose `verbose` would print the Authorization header) and
# --noproxy '*' (a proxy variable, lowercase ones included, would send the token through the proxy; no_proxy=localhost does not
# cover 127.0.0.1). Every call below begins this way.
hub_get() { # body on stdout (mkhdr has filled the header file)
  curl -q --noproxy '*' -sS --max-time 5 -H "@$HDR" "${URL}${1#/}"
}
hub_code() { # prints the http status
  curl -q --noproxy '*' -s -o /dev/null -w '%{http_code}' --max-time 5 -H "@$HDR" "${URL}${1#/}" || true
}
reusable() { alive && [ -f "$TOKF" ] && mkhdr "$TOKF" && [ "$(hub_code /hub/sessions)" = 200 ]; }

if [ "$arm" = 1 ]; then
  fp=$(printf '%s' "$arm_fp" | tr -d '[:space:]' | tr 'a-f' 'A-F')
  if ! [[ $fp =~ ^[0123456789ABCDEF]{16}$ ]]; then
    echo "minerva: keys allow-web needs the key's fingerprint: 16 hex digits (spaces allowed)" >&2
    exit 2
  fi
  if [ -n "$arm_tokfile" ]; then
    mkhdr "$arm_tokfile" || exit 1 # a test's own token and port: no perch of ours is needed (a stub answers)
  else
    # The perch's own token goes only to a perch that is ours and answering on its port, and only from a token file as the perch
    # writes it (mkhdr checks that before the first call, the liveness probe included).
    if ! alive || [ ! -f "$TOKF" ]; then
      echo "minerva: the Minerva server is not running on port $PORT; start it first (minerva web), then allow the key" >&2
      exit 1
    fi
    mkhdr "$TOKF" || exit 1
    if [ "$(hub_code /hub/sessions)" != 200 ]; then
      echo "minerva: the Minerva server is not running on port $PORT; start it first (minerva web), then allow the key" >&2
      exit 1
    fi
  fi
  body=$(printf '{"fingerprint":"%s"}' "$fp")
  reply=$(curl -q --noproxy '*' -sS --max-time 5 -X POST -H "@$HDR" -H 'Content-Type: application/json' --data "$body" -w '\n%{http_code}' "${URL}hub/install/keys/arm") || {
    echo "minerva: the perch did not answer on port $PORT" >&2
    exit 1
  }
  status=${reply##*$'\n'}
  groups=$(printf '%s' "$fp" | sed 's/.\{4\}/& /g; s/ $//')
  case "$status" in
    200)
      echo "allowed: the web page may add or remove the key $groups once in the next 2 minutes"
      exit 0 ;;
    404) echo "minerva: this perch has no update-key routes (404): stop it and start it again from the new build" >&2 ;;
    *) echo "minerva: POST /hub/install/keys/arm answered $status" >&2 ;;
  esac
  exit 1
fi

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
  # The flags newer than a perch an operator may still run (MINERVA_PERCH_BIN): each goes in only when the binary's own --help
  # lists it as an option (the same probe as bin/perch-open.sh; --aoide is not --aoide-home), once per start.
  # Plain text only: with CLICOLOR_FORCE or FORCE_COLOR set clap would wrap each flag in colour codes, the line-anchored probe would
  # find none, and a newer perch would start without the flags it can use. A perch that hangs on --help is given 10 s.
  help=$(env -u CLICOLOR_FORCE -u FORCE_COLOR NO_COLOR=1 timeout 10 "$PERCH_BIN" --help </dev/null 2>&1 || true)
  perch_has() { grep -Eq -- "^[[:space:]]+(-[[:alnum:]], )?$1([[:space:]<=]|\$)" <<<"$help"; }
  extra=()
  agent_dir=$HOME/.config/eidolon
  # The agent token and url files are the member's, one set per person, not one per perch: only the perch on the default port is
  # given them, so a second perch on another port never overwrites the first one's files.
  if [ "$PORT" = "$DEFAULT_PORT" ]; then
    if perch_has --term-token-file; then extra+=(--term-token-file "$agent_dir/minerva-term.token"); fi
    if perch_has --mail-token-file; then extra+=(--mail-token-file "$agent_dir/minerva-mail.token"); fi
    if perch_has --agent-url-file; then extra+=(--agent-url-file "$agent_dir/minerva-perch.url"); fi
  fi
  if [ "${#extra[@]}" -gt 0 ]; then mkdir -p "$agent_dir"; fi
  case "$HOME" in
    /*) if perch_has --aoide && [ -f "$HOME/.local/bin/aoide" ] && [ -x "$HOME/.local/bin/aoide" ]; then extra+=(--aoide "$HOME/.local/bin/aoide"); fi ;;
  esac
  if perch_has --plugins-dir && [ -d "$PLUGINS" ]; then extra+=(--plugins-dir "$(cd "$PLUGINS" && pwd)"); fi
  setsid nohup "$PERCH_BIN" --bind "127.0.0.1:$PORT" --ui-dir "$UI" --eidolon "$EIDOLON_BIN" \
    "${roots[@]}" --home "$MHOME" ${extra[@]+"${extra[@]}"} >"$OUTF" 2>"$LOGF" </dev/null &
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

mkhdr "$TOKF" || exit 1
reply=$(curl -q --noproxy '*' -sS --max-time 5 -X POST -H "@$HDR" -w '\n%{http_code}' "${URL}hub/code") || { echo "minerva: the perch did not answer POST /hub/code" >&2; exit 1; }
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
