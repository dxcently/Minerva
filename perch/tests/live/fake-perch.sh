#!/usr/bin/env bash
# A throwaway test perch, for a slice's live check: the real perch binary in front of the stand-in door (src/bin/fake_eidolon.rs), the real
# page from webui/term, and nothing of yours. Prints exactly one line on stdout, the login URL; everything else goes to stderr.
#
#   PORT=48102 perch/tests/live/fake-perch.sh            start (or say why it cannot) and print http://127.0.0.1:<PORT>/#code=<code>
#   PORT=48102 perch/tests/live/fake-perch.sh --stop     stop the one on that port and delete its temp tree
#   PORT=48102 perch/tests/live/fake-perch.sh --url      mint a fresh login URL for the running one (a code lives 30 s, single use)
#
# PORT is required and has no default; 47820, the operator's perch, is refused outright. Every directory the perch touches is a fresh temp tree
# (HOME, MINERVA_HOME, XDG_CONFIG/DATA/STATE/CACHE/RUNTIME), so it reads and writes nothing of the operator's: not ~/Minerva, not
# ~/.config/eidolon, not ~/.aoide. The perch starts under `env -i` with the few variables it needs, and its doors are the fake, which makes no
# model call: the model is `mock` and no provider credential exists anywhere in the tree. The bearer token never leaves its 0600 file: it
# goes to curl through `-H @file` (a 0600 header file), never argv, never an environment variable, never a log line, never stdout. The URL
# carries a login code instead, which the page trades for a cookie.
#
# Needs a perch and a fake door built with `cargo build --features test-bins` (set PERCH_BIN and FAKE_EIDOLON_BIN, or CARGO_TARGET_DIR and the
# script builds them; CARGO_NET_OFFLINE=true keeps cargo off the network). Unix only; run it in WSL on this machine.
set -euo pipefail

die() { echo "fake-perch: $*" >&2; exit 1; }

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../../.." && pwd)

mode=start
case "${1:-}" in
  '') ;;
  --stop) mode=stop ;;
  --url) mode=url ;;
  -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) die "unknown argument $1 (--stop, --url, --help)" ;;
esac

PORT=${PORT:-}
[ -n "$PORT" ] || die "PORT is required, with no default: PORT=48102 $0"
case "$PORT" in *[!0-9]*|0*) die "PORT must be a plain number, got '$PORT'" ;; esac
[ "$PORT" -ge 1024 ] && [ "$PORT" -le 65535 ] || die "PORT must be 1024-65535, got $PORT"
[ "$PORT" != 47820 ] || die "47820 is the operator's perch; this script never touches it"

# Where the temp tree lives: one fixed name per port, so --stop and --url find it, and a leftover from a crashed run is found and refused
# rather than silently reused. FAKE_PERCH_BASE moves it (it must be a directory you own).
base=${FAKE_PERCH_BASE:-${TMPDIR:-/tmp}}
T=$base/minerva-fake-perch-$PORT
TOKF=$T/xdg/run/minerva/perch-$PORT.token
URL=http://127.0.0.1:$PORT/

alive() { [ -f "$T/perch.pid" ] && kill -0 "$(cat "$T/perch.pid")" 2>/dev/null; }

# The bearer into a 0600 header file for one curl; removed by the trap. The token is read by the shell builtin `$(<file)` and written by the
# builtin printf, so it is in no process's argv or environment.
HDR=
mkhdr() {
  HDR=$(mktemp "$T/hdr.XXXXXX")
  chmod 600 "$HDR"
  printf 'Authorization: Bearer %s\n' "$(<"$TOKF")" >"$HDR"
}
rmhdr() { if [ -n "$HDR" ]; then rm -f "$HDR"; fi; HDR=; }
trap rmhdr EXIT

# POST /hub/code: a 30 s single-use login code, printed as the URL and nothing else.
login_url() {
  [ -s "$TOKF" ] || die "no token file for the perch on $PORT"
  mkhdr
  local reply status body code
  reply=$(curl -sS --max-time 5 -X POST -H "@$HDR" -w '\n%{http_code}' "${URL}hub/code") || { rmhdr; die "the perch did not answer POST /hub/code"; }
  rmhdr
  status=${reply##*$'\n'}
  body=${reply%$'\n'*}
  [ "$status" = 200 ] || die "POST /hub/code answered $status"
  code=$(printf '%s' "$body" | sed -n 's/.*"code":"\([A-Za-z0-9_-]*\)".*/\1/p')
  [ -n "$code" ] || die "the perch gave no code"
  printf '%s#code=%s\n' "$URL" "$code"
}

listening() { [ -n "$(ss -ltn "sport = :$PORT" 2>/dev/null | tail -n +2)" ]; }

case "$mode" in
  url)
    alive || die "no fake perch is running on $PORT (start one: PORT=$PORT $0)"
    login_url
    exit 0 ;;
  stop)
    if [ -f "$T/perch.pid" ]; then
      pid=$(cat "$T/perch.pid")
      if kill -0 "$pid" 2>/dev/null; then
        kill -TERM "$pid"
        for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
        if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; echo "fake-perch: the perch ($pid) ignored SIGTERM for 10 s and was killed" >&2; fi
      fi
    fi
    # Only a tree this script made (it carries the marker) is ever removed.
    if [ -f "$T/.fake-perch" ]; then rm -rf "$T"; echo "fake-perch: stopped; $T removed" >&2; else echo "fake-perch: nothing of mine at $T" >&2; fi
    if listening; then die "something still listens on $PORT"; fi
    exit 0 ;;
esac

# --- start ---
[ ! -e "$T" ] || die "$T exists (a run that was not stopped?): PORT=$PORT $0 --stop"
if listening; then die "something already listens on $PORT"; fi

# The two binaries, built with the test-bins feature: given, or built into CARGO_TARGET_DIR (default: the slices' disk target dir).
tgt=${CARGO_TARGET_DIR:-$HOME/.cache/minerva-slices-target}
PERCH_BIN=${PERCH_BIN:-$tgt/debug/perch}
FAKE_EIDOLON_BIN=${FAKE_EIDOLON_BIN:-$tgt/debug/fake_eidolon}
if [ ! -x "$PERCH_BIN" ] || [ ! -x "$FAKE_EIDOLON_BIN" ]; then
  echo "fake-perch: building the perch and the stand-in door into $tgt" >&2
  (cd "$repo/perch" && CARGO_TARGET_DIR="$tgt" cargo build --features test-bins >&2) || die "the build failed"
fi
[ -x "$PERCH_BIN" ] && [ -x "$FAKE_EIDOLON_BIN" ] || die "no perch at $PERCH_BIN or no fake door at $FAKE_EIDOLON_BIN"
ui=$repo/webui/term
[ -f "$ui/index.html" ] || die "no page at $ui"

# What a slice's own live check adds: more perch flags (FAKE_PERCH_ARGS="--jev-health http://127.0.0.1:48140/health") and more environment
# (FAKE_PERCH_ENV="FAKE_WORKFLOW_STEP_MS=500 FAKE_ASKS=/x/asks"), as whitespace-separated words, so no word holds a space. The environment
# cannot reach the isolation: HOME, MINERVA_HOME, PATH and every XDG_ name are the script's alone. Checked before anything is made.
read -r -a extra_args <<<"${FAKE_PERCH_ARGS:-}"
read -r -a extra_env <<<"${FAKE_PERCH_ENV:-}"
for word in ${extra_env[@]+"${extra_env[@]}"}; do
  case "$word" in
    HOME=*|MINERVA_HOME=*|PATH=*|XDG_*=*) die "FAKE_PERCH_ENV may not set $word (the isolation is not yours to move)" ;;
    [A-Za-z_]*=*) ;;
    *) die "FAKE_PERCH_ENV takes NAME=value words, got '$word'" ;;
  esac
done

# The temp tree. Nothing outside it is written; nothing of the operator's is read (the member's config is the empty one below).
umask 077
mkdir -p "$T"
touch "$T/.fake-perch"
mkdir -p "$T/home" "$T/minerva/sessions" "$T/minerva/scratch" "$T/work" \
  "$T/xdg/config/eidolon" "$T/xdg/data" "$T/xdg/state" "$T/xdg/cache" "$T/xdg/run"
chmod 700 "$T/xdg/run"
printf '[]\n' >"$T/minerva/projects.json"
printf 'default_model = "mock"\n' >"$T/minerva/minerva.toml"
: >"$T/xdg/config/eidolon/config.toml"
# The four entry points of the stock table, which manual mode wraps (the fake door has no tables to unpack): what the test harness writes.
cat >"$T/xdg/config/eidolon/policy.rn" <<'EOF'
pub fn shell_arg(tool) { "" }
pub fn classify_tool(tool, approval, input) { #{decision: "allow", reason: "stub", read_only: true} }
pub fn classify_program(argv, ctx) { #{decision: "allow", reason: "stub", read_only: true} }
pub fn fs_verb_tier(prog) { "allow" }
pub fn bash_default(posture) { posture }
EOF

# The perch, under a clean environment: no PATH entry from Windows, no variable of the operator's, no credential. Its doors inherit it.
setsid nohup env -i ${extra_env[@]+"${extra_env[@]}"} \
  PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 RUST_LOG=info \
  HOME="$T/home" MINERVA_HOME="$T/minerva" \
  XDG_CONFIG_HOME="$T/xdg/config" XDG_DATA_HOME="$T/xdg/data" XDG_STATE_HOME="$T/xdg/state" XDG_CACHE_HOME="$T/xdg/cache" XDG_RUNTIME_DIR="$T/xdg/run" \
  FAKE_LOG="$T/fake.log" FAKE_SESSIONS_DIR="$T/minerva/sessions" \
  "$PERCH_BIN" --bind "127.0.0.1:$PORT" --ui-dir "$ui" --root "$T/work" --home "$T/minerva" --eidolon "$FAKE_EIDOLON_BIN" \
  ${extra_args[@]+"${extra_args[@]}"} \
  >"$T/perch.out" 2>"$T/perch.err" </dev/null &
spid=$!
# setsid execs in place when the shell is not a process-group leader and forks when it is, so $! is only a fallback: the pid is the
# listener's, and readiness is the two boot lines plus the token file, not the liveness of $!.
ready=0
for _ in $(seq 1 100); do
  if [ "$(sed -n '2p' "$T/perch.out" 2>/dev/null | cut -c1-12)" = "token file: " ] && [ -s "$TOKF" ] && listening; then ready=1; break; fi
  sleep 0.2
done
pid=$(ss -ltnp "sport = :$PORT" 2>/dev/null | grep -o 'pid=[0-9]*' | head -n1 | cut -d= -f2 || true)
if [ "$ready" != 1 ]; then
  echo "fake-perch: the perch did not come up in 20 s; last lines of its log:" >&2
  tail -n 20 "$T/perch.err" >&2 || true
  kill -TERM "${pid:-$spid}" 2>/dev/null || true
  exit 1
fi
printf '%s\n' "${pid:-$spid}" >"$T/perch.pid"
echo "fake-perch: up on $URL (pid ${pid:-$spid}); state in $T; stop with: PORT=$PORT $0 --stop" >&2

# A perch that is up must also be answering behind its gate before a URL is printed.
mkhdr
status=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 -H "@$HDR" "${URL}hub/sessions" || true)
rmhdr
[ "$status" = 200 ] || { echo "fake-perch: GET /hub/sessions answered '$status', not 200" >&2; exit 1; }
login_url
