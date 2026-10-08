#!/usr/bin/env bash
# shape.sh: the term_* tools against a stub perch, through a real eidolon (the mock provider; no model is ever called).
#
#   bash plugins/term/tests/shape.sh            (PATH needs python3, curl-free; EIDOLON=/path/to/eidolon if it is not on PATH)
#
# What it proves, each in a fresh temp HOME and XDG_* (the operator's ~/.config/eidolon is never read or written), with the
# stub on 127.0.0.1:$T3_STUB_PORT (default 48116) as the only listener a tool can reach:
#   listing    `eidolon tools` lists the five term_* with their approval classes
#   tails      the five tool files share their service block byte for byte
#   ungranted  no grant on the token file: the call is refused and the stub sees nothing
#   vouched    the four verbs the installer vouches run unasked; term_send, which it never vouches, is declined
#   yolo       every verb's exact request: method, path, bearer, content type, body (extra fields dropped)
#   errors     423 / 403 / 404 / 401 and an unreachable perch each become one plain sentence; missing arguments send nothing
#   note       a read and a list answer lead with a note line; the JSON after it is the perch's, text and titles untouched
#   urlfile    the perch's address is the url file when it says a loopback http address; anything else, or no file, is refused
#              by name and nothing is sent (there is no default port to fall back to)
# No case can reach the operator's perch: the url file only ever names the stub (or a bad value), and the tools name no port.
# The files under plugins/term are never touched; each case runs a COPY.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
plug=$(cd "$here/.." && pwd)
PORT=${T3_STUB_PORT:-48116}
case $PORT in 47820 | 8090 | 8091) echo "shape.sh: port $PORT belongs to something else" >&2; exit 2 ;; esac
EIDOLON=${EIDOLON:-$(command -v eidolon || true)}
[ -x "$EIDOLON" ] || { echo "shape.sh: no eidolon (set EIDOLON=/path/to/eidolon)" >&2; exit 2; }
command -v python3 >/dev/null || { echo "shape.sh: no python3" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/term-shape.XXXXXX")
stub_pid=
cleanup() {
  if [ -n "$stub_pid" ]; then kill "$stub_pid" 2>/dev/null || true; wait "$stub_pid" 2>/dev/null || true; fi
  rm -rf "$T"
}
trap cleanup EXIT

export_home() {
  HOME_T=$T/home
  CFG=$HOME_T/.config
  mkdir -p "$CFG/eidolon/plugins" "$HOME_T/run" "$HOME_T/.local/share" "$HOME_T/.local/state" "$HOME_T/.cache"
  chmod 700 "$HOME_T/run"
}
export_home
TOKEN=$CFG/eidolon/minerva-term.token
URLF=$CFG/eidolon/minerva-perch.url
LOG=$T/stub.log
cat >"$CFG/eidolon/config.toml" <<'EOF'
default_model = "mock"

[swarm]
enabled = false
EOF

# eidolon with nothing from the caller's environment: no provider key, no proxy, no real HOME
E() {
  env -i PATH="$(dirname "$EIDOLON"):/usr/local/bin:/usr/bin:/bin" LANG=C.UTF-8 HOME="$HOME_T" \
    XDG_CONFIG_HOME="$CFG" XDG_DATA_HOME="$HOME_T/.local/share" XDG_STATE_HOME="$HOME_T/.local/state" \
    XDG_CACHE_HOME="$HOME_T/.cache" XDG_RUNTIME_DIR="$HOME_T/run" "$EIDOLON" "$@"
}

stop_stub() {
  if [ -n "$stub_pid" ]; then kill "$stub_pid" 2>/dev/null || true; wait "$stub_pid" 2>/dev/null || true; stub_pid=; fi
}
start_stub() { # mode
  stop_stub
  : >"$LOG"
  python3 "$here/stub.py" --port "$PORT" --token-file "$TOKEN" --log "$LOG" --mode "${1:-normal}" >"$T/stub.out" 2>&1 &
  stub_pid=$!
  for _ in $(seq 1 50); do
    if grep -q '^ready' "$T/stub.out" 2>/dev/null; then return 0; fi
    kill -0 "$stub_pid" 2>/dev/null || { echo "shape.sh: the stub did not start: $(cat "$T/stub.out")" >&2; exit 1; }
    sleep 0.1
  done
  echo "shape.sh: the stub is not ready" >&2
  exit 1
}

# a fresh plugins dir: a COPY of plugins/term, the probe, a new token, no url file, no permits. The copy is as shipped: it names no
# port, so a case reaches the stub only through the url file the case writes.
fresh() {
  rm -rf "$CFG/eidolon/plugins" "$CFG/eidolon/policy.permits.rn" "$URLF" "$HOME_T/.local/share/eidolon"
  mkdir -p "$CFG/eidolon/plugins/term"
  cp -r "$plug/plugin.rn" "$plug/tools" "$CFG/eidolon/plugins/term/"
  cp -r "$here/probe" "$CFG/eidolon/plugins/probe"
  umask 077
  head -c 24 /dev/urandom | base64 | tr -d '=+/\n' >"$TOKEN"
  chmod 600 "$TOKEN"
}
url() { printf '%s\n' "$1" >"$URLF"; }

# what the installer does for term (installer/src/plugins.rs trust_one): four vouches and five grants, never a vouch of send
vouch_four() {
  local v
  for v in list read open close; do E plugins trust term "$v" >/dev/null; done
}
as_installer() {
  vouch_four
  grants_only
}
grants_only() {
  local v
  for v in list open read send close; do E plugins grant "term_$v" 'file:~/.config/eidolon/minerva-term.token' >/dev/null; done
}

# run the probe workflow; the one JSON line it prints goes to $T/run.json
run_wf() { # set [extra eidolon flags]
  local set=$1
  shift
  E workflow run "$CFG/eidolon/plugins/probe" shape --args "{\"set\":\"$set\"}" --provider mock --model mock "$@" >"$T/run.json" 2>"$T/run.err" || true
}

check() {
  # T3_SHOW=1 prints what each run reported (the tools' own answers; no token is ever in them)
  if [ -n "${T3_SHOW:-}" ] && [ -s "$T/run.json" ]; then echo "--- $1: $(cut -c1-1500 "$T/run.json")"; fi
  python3 "$here/check.py" "$@" --log "$LOG" --run "$T/run.json" --tools "$T/tools.json"
}
pass() { echo "ok   $1"; }

# ---- tails: no eidolon needed
python3 "$here/check.py" tails --dir "$plug/tools"
pass tails

# ---- listing
fresh
start_stub normal
E tools >"$T/tools.json" 2>/dev/null
check listing
pass listing

# ---- ungranted: the verbs are vouched, the token file is not granted, nobody to ask
fresh
vouch_four
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf list
check ungranted
pass ungranted

# ---- notoken: vouched and granted, but the perch never wrote its token file (it was started without --term-token-file)
fresh
as_installer
rm -f "$TOKEN"
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf list
check notoken
pass notoken

# ---- vouched: as the installer leaves it
fresh
as_installer
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf basic
check vouched
pass vouched

# ---- yolo: every request's exact shape
fresh
grants_only
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf basic --yolo
check yolo
pass yolo

# ---- errors
fresh
grants_only
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf errors --yolo
check errors
pass errors

for mode in noroutes wrongtoken; do
  fresh
  grants_only
  start_stub "$mode"
  url "http://127.0.0.1:$PORT/"
  run_wf list --yolo
  check "mode-$mode"
  pass "errors: $mode"
done

fresh
grants_only
start_stub normal
url "http://127.0.0.1:1/"
run_wf list --yolo
check unreachable
pass "errors: unreachable"

# ---- note: the read's note leads, and the perch's text is left as it was
fresh
grants_only
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf note --yolo
check note
pass note

# ---- list-note: the list's note leads, and the titles are left as the perch gave them
fresh
grants_only
start_stub normal
url "http://127.0.0.1:$PORT/"
run_wf list --yolo
check list-note
pass "note: list"

# ---- urlfile: a loopback address in the file is used (the stub is the only listener a case can reach)...
for good in "http://127.0.0.1:$PORT/" "http://127.0.0.1:$PORT" "http://127.0.0.1:$PORT/
second line is ignored"; do
  fresh
  grants_only
  start_stub normal
  url "$good"
  run_wf list --yolo
  check urlfile-used
done
pass "urlfile: a loopback address is used"

# ...and anything else is refused by name, with nothing sent: there is no default port to fall back to
for bad in "http://evil.example:$PORT/" "https://127.0.0.1:$PORT/" "http://127.0.0.1.evil.example:$PORT/" "http://user:pw@127.0.0.1:$PORT/" \
  "http://127.0.0.1:$PORT/hub" "http://127.0.0.1:99999/" "http://127.0.0.1:0/" "http://127.0.0.1:/" "http://[::2]:$PORT/" \
  "http://localhost:$PORT/" "garbage" ""; do
  fresh
  grants_only
  start_stub normal
  url "$bad"
  run_wf list --yolo
  check urlfile-refused
done
pass "urlfile: anything else is refused, nothing sent"
# no url file at all
fresh
grants_only
start_stub normal
run_wf list --yolo
check urlfile-refused
pass "urlfile: a missing file is refused, nothing sent"

echo "shape.sh: all cases hold"
