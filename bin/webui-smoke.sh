#!/usr/bin/env bash
# Does an `eidolon web` door still speak the API the browser UI (webui/term)
# depends on? No browser: this talks HTTP to the door the way core/api.js does
# — GET <base>events with a Bearer header, POST <base>say, POST <base>answer —
# and asserts on the *frames* the page draws (core/state.js), never on the
# model's prose.
#
#   bin/webui-smoke.sh
#
# Door mode (default) starts its own door on `--bind 127.0.0.1:0` in a fresh
# `mktemp -d` and drives it. Hub mode takes the door's API base and token file
# from the environment instead (docs/design/hub.md section 5), starts nothing
# and owns nothing, so step 7 reports its shutdown measurements rather than
# asserting them:
#
#   SMOKE_BASE=http://127.0.0.1:PORT/s/<session>/api/ SMOKE_TOKEN_FILE=/path/to.token bin/webui-smoke.sh
#
#   SMOKE_MODEL          model the three short turns run on
#                        (default ollama:deepseek-v4.1-flash)
#   SMOKE_TURN_TIMEOUT   seconds one model turn may take; a wait past it is a
#                        FAIL with the last frames printed (default 90)
#   SMOKE_START_TIMEOUT  seconds for the door's two boot lines / `hello` (20)
#   SMOKE_EXIT_TIMEOUT   seconds SIGTERM -> exit may take (10)
#   SMOKE_REVERSE_WAIT   how long step 7 watches a door SIGTERM'd with its
#                        stream still open (15)
#   SMOKE_HTTP_TIMEOUT   seconds for a POST (10)
#   SMOKE_AUTH_TIMEOUT   seconds for a status probe (3)
#   SMOKE_KEEP           1 leaves the temp dirs and the raw frames behind
#
# Cost: three short model turns on SMOKE_MODEL and two door starts (the second
# is step 7's reverse-order measurement and runs no turn). Run it from the
# background with a log if the tool timeout you are calling it from is unsure:
# `eidolon`'s own bash tool is 120 s by default (tools/src/shell.rs:26), and a
# whole run is minutes.
#
# Steps, one PASS/FAIL line each; exit non-zero if any FAILs. A step whose
# prerequisite FAILed (no door, no stream) is SKIPped rather than re-timing-out,
# so a broken door costs minutes, not four times minutes. Nothing outside
# `mktemp -d` is written. The token is read from the file the door printed and
# goes into a 0600 header file used as `curl -H @file` (so it never reaches
# another process's argv); it is never printed and never put in a URL. A door
# is only ever stopped by the exact PID this script started — no
# pkill/killall/pgrep -f, whose pattern can match the caller.
#
# Measured facts this script is written against (2026-09-27, live door; the
# report names where they disagree with eidolon/crates/web/src and hub.md):
#   * the API base is `<printed root>/api/` — `<root>/events` is 404.
#   * `touch <door cwd>/x` is policy Allow ("workspace file op") and raises no
#     ask, so the two gated steps mutate a path OUTSIDE the door's cwd, which
#     the table Flags ("filesystem mutation into /tmp"). `rm <door cwd>/x` is
#     Flag ("destructive op in workspace") too.
#   * SIGTERM with an SSE stream still open does not end the door: the graceful
#     shutdown waits for that connection out (serve.rs uses GracefulShutdown).
#     Step 7 asserts the order that works and measures that one.
set -uo pipefail
umask 077

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
helper=$here/webui-smoke.py

SMOKE_MODEL=${SMOKE_MODEL:-ollama:deepseek-v4.1-flash}
SMOKE_TURN_TIMEOUT=${SMOKE_TURN_TIMEOUT:-90}
SMOKE_START_TIMEOUT=${SMOKE_START_TIMEOUT:-20}
SMOKE_EXIT_TIMEOUT=${SMOKE_EXIT_TIMEOUT:-10}
SMOKE_REVERSE_WAIT=${SMOKE_REVERSE_WAIT:-15}
SMOKE_HTTP_TIMEOUT=${SMOKE_HTTP_TIMEOUT:-10}
SMOKE_AUTH_TIMEOUT=${SMOKE_AUTH_TIMEOUT:-3}
SMOKE_KEEP=${SMOKE_KEEP:-0}

CWD_DIR=        # the door's --cwd
MUT_DIR=        # the gated steps' target dir, outside CWD_DIR on purpose
OUT=            # scratch: raw frames, headers, bodies, logs
RAW=
RAW2=
HDR=
HDR2=
BODY=
DOOR_LOG=
DOOR2_LOG=
DOOR_PID=
DOOR2_PID=
SSE_PID=
SSE2_PID=
STREAM_PID=
BASE=
TOKEN_FILE=
HUB_MODE=0
MARK=0
STEP_WHY=
FAILS=0
PASSES=0
SKIPS=0

note() { printf '  · %s\n' "$*"; }
elapsed() { printf '%sms' "$(( ($(date +%s%N) - $1) / 1000000 ))"; }

# The first failure in a step is the one reported.
x_fail() { [[ -n $STEP_WHY ]] || STEP_WHY=$1; return 1; }
step_ok() { [[ -z $STEP_WHY ]]; }

# A process that exists and is not a zombie. `kill -0` alone says yes to a
# zombie, and bash may not have reaped a background child yet.
alive() {
  local pid=${1:-} st
  [[ -n $pid ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  st=$(ps -o stat= -p "$pid" 2>/dev/null) || return 1
  [[ -n $st && $st != Z* ]]
}

gone_within() { # pid seconds
  local pid=$1 secs=$2 deadline
  deadline=$(( $(date +%s%N) + secs * 1000000000 ))
  while (( $(date +%s%N) < deadline )); do
    alive "$pid" || return 0
    sleep 0.05
  done
  ! alive "$pid"
}

# --------------------------------------------------------------- the frames
frame_wait() { # timeout type [--where PATH=REGEX]...
  local timeout=$1 type=$2; shift 2
  python3 "$helper" wait --file "$RAW" --from "$MARK" --timeout "$timeout" --type "$type" --last 12 "$@"
}
frame_wait_raw() { # rawfile from timeout type
  local raw=$1 from=$2 timeout=$3 type=$4; shift 4
  python3 "$helper" wait --file "$raw" --from "$from" --timeout "$timeout" --type "$type" --last 12 "$@"
}
frame_count() { python3 "$helper" count --file "$RAW" --from "$MARK" "$@"; }
frame_types() { python3 "$helper" types --file "$RAW" --from "$MARK" 2>/dev/null; }
frame_field() { python3 "$helper" field --path "$2" <<<"$1" 2>/dev/null; }
dump_window() {
  note "window frames: $(frame_types)"
  python3 "$helper" tail --file "$RAW" --from "$MARK" --last 12 2>/dev/null | sed 's/^/    /'
}

# --------------------------------------------------------------- the process
# The two boot lines, and only those: the door also prints its session path and
# the policy table's path, which are not this script's business.
boot_lines() { # logfile pid -> "root<TAB>token file"
  local log=$1 pid=$2 i root tokf
  for ((i = 0; i < SMOKE_START_TIMEOUT * 10; i++)); do
    root=$(sed -n 's/^listening on \(http[^ ]*\)$/\1/p' "$log" | head -1)
    tokf=$(sed -n 's/^token file: \(.*\)$/\1/p' "$log" | head -1)
    if [[ -n $root && -n $tokf ]]; then
      printf '%s\t%s\n' "${root%/}" "$tokf"
      return 0
    fi
    alive "$pid" || return 1
    sleep 0.1
  done
  return 1
}

# The token is read by redirection (no `cat`, so no argv) and only ever written
# into a 0600 header file for `curl -H @file`.
write_header() { # tokenfile headerfile
  local token
  token=$(<"$1") || return 1
  printf 'Authorization: Bearer %s\n' "$token" >"$2" || return 1
  unset token
  return 0
}

# Started here, not inside a `$(...)`: a subshell's child is nobody's child,
# and `close_stream`'s `wait` could never reap it.
open_stream() { # rawfile headerfile logfile -> pid in STREAM_PID
  curl -sS -N -H @"$2" "${BASE}events" >>"$1" 2>"$3" &
  STREAM_PID=$!
}

# Never a bare `kill 0`: that signals this script's whole process group.
close_stream() {
  local pid=${1:-}
  [[ -n $pid && $pid != 0 ]] || return 0
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  return 0
}

# ------------------------------------------------------------- the requests
http_code() { # timeout curl args...
  local timeout=$1; shift
  curl -s -o /dev/null -m "$timeout" -w '%{http_code}' "$@"
}

post_json() { # route body-json -> status on stdout
  printf '%s' "$2" >"$BODY"
  http_code "$SMOKE_HTTP_TIMEOUT" -H @"$HDR" -H 'content-type: application/json' \
    -X POST "${BASE}${1}" --data-binary @"$BODY"
}

say_text() {
  printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps({"text": sys.stdin.read(), "mode": "send"}))'
}

# state.js:623 `post(this.sid, 'answer', { ask_id: a.id, ...reply })`, and
# serve.rs's AnswerBody { ask_id, answer }.
answer_body() { printf '{"ask_id":%s,"answer":"%s"}' "$1" "$2"; }

# ------------------------------------------------------------------ steps
step1() { # the door's base and token file
  local t0; t0=$(date +%s%N)
  if (( HUB_MODE )); then
    BASE=${SMOKE_BASE%/}/
    TOKEN_FILE=$SMOKE_TOKEN_FILE
    [[ -r $TOKEN_FILE ]] || x_fail "token file is not readable: $TOKEN_FILE"
    write_header "$TOKEN_FILE" "$HDR" || x_fail "token file did not read: $TOKEN_FILE"
    note "hub mode: started nothing; base $BASE, token file $TOKEN_FILE"
    note "token header file $HDR (mode $(stat -c %a "$HDR" 2>/dev/null))"
    step_ok
    return
  fi

  eidolon web --bind 127.0.0.1:0 --model "$SMOKE_MODEL" --cwd "$CWD_DIR" >"$DOOR_LOG" 2>&1 &
  DOOR_PID=$!
  local lines
  lines=$(boot_lines "$DOOR_LOG" "$DOOR_PID") || {
    note "door log, first lines:"
    sed -n '1,20p' "$DOOR_LOG" | sed 's/^/    /'
    x_fail "no <listening on ...> + <token file: ...> within ${SMOKE_START_TIMEOUT}s (pid $DOOR_PID)"
    return 1
  }
  local root=${lines%%$'\t'*} tokf=${lines#*$'\t'} mode
  BASE="${root}/api/"
  TOKEN_FILE=$tokf

  [[ $root == http://127.0.0.1:* ]] || x_fail "the door is not on loopback: $root"
  [[ -r $TOKEN_FILE ]] || x_fail "token file is not readable: $TOKEN_FILE"
  mode=$(stat -c %a "$TOKEN_FILE" 2>/dev/null)
  [[ $mode == 600 ]] || x_fail "token file mode is $mode, not 600: $TOKEN_FILE"
  write_header "$TOKEN_FILE" "$HDR" || x_fail "token file did not read: $TOKEN_FILE"

  note "door pid $DOOR_PID on $root; api base $BASE"
  note "token file $TOKEN_FILE (mode $mode); header file $HDR, never printed"
  note "model $SMOKE_MODEL; door cwd $CWD_DIR; gated target dir $MUT_DIR"
  note "booted in $(elapsed "$t0")"
  step_ok
}

step2() { # GET <base>events: hello, then caught-up
  open_stream "$RAW" "$HDR" "$OUT/sse.err"
  SSE_PID=$STREAM_PID
  note "sse curl pid $SSE_PID -> $RAW"
  MARK=0
  local hello
  hello=$(frame_wait "$SMOKE_START_TIMEOUT" hello) || {
    note "curl stderr: $(tr '\n' ' ' <"$OUT/sse.err" | head -c 200)"
    note "the door answers ${BASE}events with our header: $(http_code "$SMOKE_AUTH_TIMEOUT" -H @"$HDR" "${BASE}events") (401 = it refused the token)"
    x_fail "no hello frame within ${SMOKE_START_TIMEOUT}s on ${BASE}events"
    return 1
  }
  local protocol cwd pending session model
  protocol=$(frame_field "$hello" protocol)
  cwd=$(frame_field "$hello" cwd)
  pending=$(frame_field "$hello" pending)
  session=$(frame_field "$hello" session)
  model=$(frame_field "$hello" model)
  note "hello: protocol=$protocol pending=$pending model=$model"
  note "hello: session=$session"
  note "hello: cwd=$cwd"
  [[ $protocol == 1 ]] || x_fail "hello.protocol is '$protocol'; the page knows 1 (state.js:346)"
  [[ -n $session ]] || x_fail "hello.session is empty"
  [[ $pending == 0 ]] || x_fail "hello.pending is '$pending' on a fresh door, not 0"
  (( HUB_MODE )) || [[ $cwd == "$CWD_DIR" ]] || x_fail "hello.cwd is '$cwd', not the --cwd we passed"

  local caught
  caught=$(frame_wait_raw "$RAW" 0 "$SMOKE_START_TIMEOUT" caught-up) || {
    x_fail "no caught-up frame within ${SMOKE_START_TIMEOUT}s (the page stays 'replaying')"
    return 1
  }
  local asks; asks=$(python3 "$helper" count --file "$RAW" --from 0 --type ask)
  [[ $asks == 0 ]] || x_fail "the replay carried $asks ask frame(s); hello said pending='$pending'"
  note "frames: $(python3 "$helper" types --file "$RAW" --from 0)"
  step_ok
}

step3() { # the token gate
  local events_notoken events_wrong say_wrong answer_notoken say_query events_query_empty events_query_wrong
  events_notoken=$(http_code "$SMOKE_AUTH_TIMEOUT" "${BASE}events")
  events_wrong=$(http_code "$SMOKE_AUTH_TIMEOUT" -H 'Authorization: Bearer not-the-door-token' "${BASE}events")
  say_wrong=$(http_code "$SMOKE_AUTH_TIMEOUT" -X POST -H 'Authorization: Bearer not-the-door-token' \
    -H 'content-type: application/json' --data-binary '{"text":"x","mode":"send"}' "${BASE}say")
  answer_notoken=$(http_code "$SMOKE_AUTH_TIMEOUT" -X POST -H 'content-type: application/json' \
    --data-binary '{"ask_id":1,"answer":"yes"}' "${BASE}answer")
  # A query token is not a credential: a value that is not the door's token
  # must be refused. On GET events the door does accept *its own* token that way
  # (EventSource cannot set a header — serve.rs `presented`); that is the one
  # place the brief's stricter rule does not hold, and it is a NOTE, not a check,
  # because the script will not put the token in a URL to prove it.
  events_query_empty=$(http_code "$SMOKE_AUTH_TIMEOUT" "${BASE}events?token=")
  events_query_wrong=$(http_code "$SMOKE_AUTH_TIMEOUT" "${BASE}events?token=not-the-door-token")
  # POSTs never take the query form; in hub mode any query string is 400
  # (hub.md section 5) rather than the door's 401.
  say_query=$(http_code "$SMOKE_AUTH_TIMEOUT" -X POST -H 'content-type: application/json' \
    --data-binary '{"text":"x","mode":"send"}' "${BASE}say?token=not-the-door-token")

  note "events no token      -> $events_notoken"
  note "events wrong token   -> $events_wrong"
  note "say wrong token      -> $say_wrong"
  note "answer no token      -> $answer_notoken"
  note "events?token=        -> $events_query_empty"
  note "events?token=<wrong> -> $events_query_wrong"
  note "say?token=<wrong>    -> $say_query"

  [[ $events_notoken == 401 ]] || x_fail "GET events without a token answered $events_notoken, not 401"
  [[ $events_wrong == 401 ]] || x_fail "GET events with a wrong token answered $events_wrong, not 401"
  [[ $say_wrong == 401 ]] || x_fail "POST say with a wrong token answered $say_wrong, not 401"
  [[ $answer_notoken == 401 ]] || x_fail "POST answer without a token answered $answer_notoken, not 401"
  query_refused "$events_query_empty" || x_fail "GET events?token= answered $events_query_empty, not a refusal"
  query_refused "$events_query_wrong" || x_fail "GET events with a wrong ?token= answered $events_query_wrong, not a refusal"
  query_refused "$say_query" || x_fail "POST say?token=<wrong> answered $say_query, not a refusal"
  if (( HUB_MODE )) && [[ $say_query == 401 ]]; then
    note "that 401 says the query reached the door; hub.md section 5 has the hub refusing it with 400 first"
  fi
  step_ok
}

# The refusal a `?token=` query value must earn: the door's 401, or the hub's
# 400 (hub.md section 5 refuses every query string before it proxies) — the
# door's 401 is also a refusal, just not the hub's own.
query_refused() {
  if (( HUB_MODE )); then
    [[ $1 == 400 || $1 == 401 ]]
  else
    [[ $1 == 401 ]]
  fi
}

# The end of the turn behind a say: the last turn-settled, then the turn-state
# that follows it (driver.rs `announce(false)`): a model loop that called a tool
# settles before the turn itself ends.
turn_over() {
  frame_wait "$SMOKE_TURN_TIMEOUT" turn-settled >/dev/null || {
    dump_window
    x_fail "no turn-settled within ${SMOKE_TURN_TIMEOUT}s"
    return 1
  }
  frame_wait "$SMOKE_TURN_TIMEOUT" turn-state --where 'running=false' >/dev/null || {
    dump_window
    x_fail "no turn-state {running:false} within ${SMOKE_TURN_TIMEOUT}s"
    return 1
  }
  return 0
}

step4() { # say: a bash call the gate runs silently
  MARK=$(wc -c <"$RAW")
  local status
  status=$(post_json say "$(say_text 'Use bash to run exactly `echo smoke-roundtrip`, then reply done.')")
  note "POST say -> $status"
  [[ $status == 202 ]] || x_fail "POST say answered $status, not 202"

  local started
  started=$(frame_wait "$SMOKE_TURN_TIMEOUT" tool-call-started --where 'name=bash' \
    --where 'input.command=^\s*echo\b.*smoke-roundtrip') || {
    dump_window
    x_fail "no tool-call-started for bash running echo smoke-roundtrip"
    return 1
  }
  local id command
  id=$(frame_field "$started" id)
  command=$(frame_field "$started" input.command)
  note "tool-call-started id=$id command=$command"

  local finished is_error output
  finished=$(frame_wait "$SMOKE_TURN_TIMEOUT" tool-call-finished --where "id=$id") || {
    dump_window
    x_fail "no tool-call-finished for $id"
    return 1
  }
  is_error=$(frame_field "$finished" is_error)
  output=$(frame_field "$finished" output)
  note "tool-call-finished is_error=$is_error output=$(printf '%s' "$output" | head -c 160)"
  [[ $is_error == false ]] || x_fail "the echo call came back is_error=$is_error"
  [[ $output == *smoke-roundtrip* ]] || x_fail "the echo call's output has no smoke-roundtrip in it"

  turn_over || return 1
  local asks; asks=$(frame_count --type ask)
  [[ $asks == 0 ]] || x_fail "a silent command raised $asks ask frame(s)"
  note "frames: $(frame_types)"
  step_ok
}

step5() { # a gated call answered yes: the file appears
  MARK=$(wc -c <"$RAW")
  local target=$MUT_DIR/smoke-ask status
  status=$(post_json say "$(say_text "Use bash to run exactly \`touch $target\`, then reply done.")")
  note "POST say -> $status"
  [[ $status == 202 ]] || x_fail "POST say answered $status, not 202"

  local ask ask_id call_id command answers
  ask=$(frame_wait "$SMOKE_TURN_TIMEOUT" ask --where 'kind=approval' --where 'tool=bash' \
    --where "input.command=^\s*touch\b.*smoke-ask") || {
    dump_window
    x_fail "no ask frame for bash running touch $target"
    return 1
  }
  ask_id=$(frame_field "$ask" ask_id)
  call_id=$(frame_field "$ask" call_id)
  command=$(frame_field "$ask" input.command)
  answers=$(python3 "$helper" field --path answers --raw <<<"$ask" 2>/dev/null)
  note "ask kind=approval ask_id=$ask_id call_id=$call_id tool=bash answers=$answers"
  note "ask input.command=$command"
  [[ $answers == *yes* && $answers == *no* ]] || x_fail "the ask does not offer yes and no: '$answers'"

  local answered
  answered=$(post_json answer "$(answer_body "$ask_id" yes)")
  note "POST answer yes -> $answered"
  [[ $answered == 204 ]] || x_fail "POST answer yes answered $answered, not 204"

  local settled how answer
  settled=$(frame_wait "$SMOKE_TURN_TIMEOUT" ask-settled --where "ask_id=$ask_id") || {
    dump_window
    x_fail "no ask-settled for ask_id=$ask_id"
    return 1
  }
  how=$(frame_field "$settled" how)
  answer=$(frame_field "$settled" answer)
  note "ask-settled how=$how answer=$answer"
  [[ $how == answered ]] || x_fail "ask-settled how='$how', not answered"
  [[ $answer == yes ]] || x_fail "ask-settled answer='$answer', not yes"

  local finished is_error
  finished=$(frame_wait "$SMOKE_TURN_TIMEOUT" tool-call-finished --where "id=$call_id") || {
    dump_window
    x_fail "no tool-call-finished for $call_id after answering yes"
    return 1
  }
  is_error=$(frame_field "$finished" is_error)
  note "tool-call-finished is_error=$is_error output=$(frame_field "$finished" output | head -c 80)"
  [[ $is_error == false ]] || x_fail "the answered call came back is_error=$is_error"

  # The page draws the ruling as a row (state.js `verdict`), so the frame has
  # to be there for a call the gate flagged; its outcome value is reported as
  # measured rather than asserted, since only the door's table decides it.
  local verdict
  verdict=$(frame_wait "$SMOKE_TURN_TIMEOUT" policy-verdict --where 'tool=bash') || {
    dump_window
    x_fail "no policy-verdict frame for the flagged call"
    return 1
  }
  note "policy-verdict tool=bash outcome=$(frame_field "$verdict" outcome) reason=$(frame_field "$verdict" reason)"

  turn_over || return 1
  [[ -f $target ]] || x_fail "$target does not exist after answering yes"
  note "frames: $(frame_types)"
  note "$target: $(ls -l "$target" 2>/dev/null | tr -s ' ')"
  step_ok
}

step6() { # the same call answered no: declined, and the file stays
  MARK=$(wc -c <"$RAW")
  local target=$MUT_DIR/smoke-ask status
  status=$(post_json say "$(say_text "Use bash to run exactly \`rm $target\`. If it is refused, reply refused and do not retry.")")
  note "POST say -> $status"
  [[ $status == 202 ]] || x_fail "POST say answered $status, not 202"

  local ask ask_id call_id command
  ask=$(frame_wait "$SMOKE_TURN_TIMEOUT" ask --where 'kind=approval' --where 'tool=bash' \
    --where "input.command=^\s*rm\b.*smoke-ask") || {
    dump_window
    x_fail "no ask frame for bash running rm $target"
    return 1
  }
  ask_id=$(frame_field "$ask" ask_id)
  call_id=$(frame_field "$ask" call_id)
  command=$(frame_field "$ask" input.command)
  note "ask kind=approval ask_id=$ask_id call_id=$call_id input.command=$command"

  local answered
  answered=$(post_json answer "$(answer_body "$ask_id" no)")
  note "POST answer no -> $answered"
  [[ $answered == 204 ]] || x_fail "POST answer no answered $answered, not 204"

  local settled how answer
  settled=$(frame_wait "$SMOKE_TURN_TIMEOUT" ask-settled --where "ask_id=$ask_id") || {
    dump_window
    x_fail "no ask-settled for ask_id=$ask_id"
    return 1
  }
  how=$(frame_field "$settled" how)
  answer=$(frame_field "$settled" answer)
  note "ask-settled how=$how answer=$answer"
  [[ $how == answered ]] || x_fail "ask-settled how='$how', not answered"
  [[ $answer == no ]] || x_fail "ask-settled answer='$answer', not no"

  local finished is_error output
  finished=$(frame_wait "$SMOKE_TURN_TIMEOUT" tool-call-finished --where "id=$call_id") || {
    dump_window
    x_fail "no tool-call-finished for $call_id after answering no"
    return 1
  }
  is_error=$(frame_field "$finished" is_error)
  output=$(frame_field "$finished" output)
  note "tool-call-finished is_error=$is_error output=$(printf '%s' "$output" | head -c 160)"
  [[ $is_error == true ]] || x_fail "the declined call came back is_error=$is_error, not true"

  turn_over || return 1
  [[ -f $target ]] || x_fail "$target is not on disk after the declined rm (step 5 creates it; if step 5 FAILed this is its consequence)"

  # The page reads 409 as "that ask was answered elsewhere" and drops it
  # (state.js `send`); the door answers that for an ask id it no longer holds
  # (user.rs `Answer::Unknown`). Free to check — the ask is already answered.
  local again
  again=$(post_json answer "$(answer_body "$ask_id" no)")
  note "POST answer no again on ask_id=$ask_id -> $again"
  [[ $again == 409 ]] || x_fail "a second answer on ask_id=$ask_id answered $again, not 409"
  note "frames: $(frame_types)"
  step_ok
}

step7() { # shutdown: stream first then SIGTERM — and the reverse, measured
  local t0 stream_ms exit_ms code

  if (( HUB_MODE )); then
    t0=$(date +%s%N)
    close_stream "${SSE_PID:-}"
    SSE_PID=
    note "hub mode: our stream closed in $(elapsed "$t0")"
    note "this script started neither the door nor the hub, so it stops neither:"
    note "the forward/reverse shutdown measurements are door-mode only"
    note "step 7b (reverse order) is therefore SKIPped below"
    step_ok
    return
  fi

  t0=$(date +%s%N)
  close_stream "${SSE_PID:-}"; SSE_PID=
  stream_ms=$(( ($(date +%s%N) - t0) / 1000000 ))

  t0=$(date +%s%N)
  kill -TERM "$DOOR_PID" 2>/dev/null
  if gone_within "$DOOR_PID" "$SMOKE_EXIT_TIMEOUT"; then
    wait "$DOOR_PID" 2>/dev/null; code=$?
    DOOR_PID=
    exit_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    note "forward: stream closed in ${stream_ms}ms; SIGTERM -> exit in ${exit_ms}ms (status $code)"
  else
    exit_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    note "forward: still alive ${exit_ms}ms after SIGTERM with no stream open; SIGKILL"
    kill -9 "$DOOR_PID" 2>/dev/null; wait "$DOOR_PID" 2>/dev/null; DOOR_PID=
    x_fail "the door did not exit within ${SMOKE_EXIT_TIMEOUT}s of SIGTERM with no stream open (needed SIGKILL)"
  fi
  [[ -e $TOKEN_FILE ]] && x_fail "the token file outlived the door: $TOKEN_FILE"
  note "forward: token file removed: $([[ -e $TOKEN_FILE ]] && echo no || echo yes)"

  # Reverse, measured: SIGTERM while a stream is still open.
  eidolon web --bind 127.0.0.1:0 --model "$SMOKE_MODEL" --cwd "$CWD_DIR" >"$DOOR2_LOG" 2>&1 &
  DOOR2_PID=$!
  local lines root2 tokf2 saved_base=$BASE raw2=$RAW2
  lines=$(boot_lines "$DOOR2_LOG" "$DOOR2_PID") || {
    x_fail "the second door did not print its boot lines"
    return 1
  }
  root2=${lines%%$'\t'*}; tokf2=${lines#*$'\t'}
  write_header "$tokf2" "$HDR2" || x_fail "the second door's token file did not read"
  BASE="${root2}/api/"
  open_stream "$raw2" "$HDR2" "$OUT/sse2.err"
  SSE2_PID=$STREAM_PID
  if ! frame_wait_raw "$raw2" 0 "$SMOKE_START_TIMEOUT" hello >/dev/null; then
    BASE=$saved_base
    x_fail "the reverse-order measurement could not open a stream on the second door"
    return 1
  fi
  note "reverse: second door pid $DOOR2_PID on $root2, stream open, hello seen"

  t0=$(date +%s%N)
  kill -TERM "$DOOR2_PID" 2>/dev/null
  if gone_within "$DOOR2_PID" "$SMOKE_REVERSE_WAIT"; then
    note "reverse: SIGTERM while streaming -> exited in $(( ($(date +%s%N) - t0) / 1000000 ))ms"
    note "reverse: token file removed: $([[ -e $tokf2 ]] && echo no || echo yes)"
    wait "$DOOR2_PID" 2>/dev/null; DOOR2_PID=
  else
    local alive_ms c0 c1
    alive_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    note "reverse: SIGTERM while streaming -> STILL ALIVE after ${SMOKE_REVERSE_WAIT}s (${alive_ms}ms)"
    note "reverse: token file present while alive: $([[ -e $tokf2 ]] && echo yes || echo no)"
    c0=$(date +%s%N)
    close_stream "${SSE2_PID:-}"; SSE2_PID=
    if gone_within "$DOOR2_PID" "$SMOKE_EXIT_TIMEOUT"; then
      c1=$(date +%s%N)
      wait "$DOOR2_PID" 2>/dev/null; code=$?
      DOOR2_PID=
      note "reverse: closing the stream ended it $(( (c1 - c0) / 1000000 ))ms later (status $code)"
      note "reverse: token file removed: $([[ -e $tokf2 ]] && echo no || echo yes)"
    else
      note "reverse: still alive ${SMOKE_EXIT_TIMEOUT}s after the stream closed; SIGKILL"
      kill -9 "$DOOR2_PID" 2>/dev/null; wait "$DOOR2_PID" 2>/dev/null; DOOR2_PID=
      note "reverse: token file after SIGKILL: $([[ -e $tokf2 ]] && echo present || echo gone)"
      BASE=$saved_base
      x_fail "the door never exited, even after the stream closed (needed SIGKILL)"
      return 1
    fi
  fi
  BASE=$saved_base
  step_ok
}

step8() { # clean up
  close_stream "${SSE_PID:-}"; SSE_PID=
  close_stream "${SSE2_PID:-}"; SSE2_PID=
  if (( SMOKE_KEEP )); then
    note "SMOKE_KEEP=1: leaving $OUT, $CWD_DIR, $MUT_DIR"
    step_ok
    return
  fi
  local n=0 d
  for d in "$OUT" "$CWD_DIR" "$MUT_DIR"; do
    [[ -n $d && -d $d ]] || continue
    rm -rf -- "$d" || x_fail "could not remove $d"
    n=$((n + 1))
  done
  note "removed $n temp dir(s); nothing outside mktemp -d was written"
  for d in "$OUT" "$CWD_DIR" "$MUT_DIR"; do
    [[ -e $d ]] && x_fail "$d survived the cleanup"
  done
  step_ok
}

# ------------------------------------------------------------------- driver
# Only the PIDs this script started, and only ever by their exact value.
cleanup() {
  close_stream "${SSE_PID:-}"
  close_stream "${SSE2_PID:-}"
  alive "${DOOR_PID:-}" && kill -TERM "$DOOR_PID" 2>/dev/null
  alive "${DOOR2_PID:-}" && kill -TERM "$DOOR2_PID" 2>/dev/null
  SSE_PID=; SSE2_PID=
  if (( SMOKE_KEEP )); then return 0; fi
  local d
  for d in "${OUT:-}" "${CWD_DIR:-}" "${MUT_DIR:-}"; do
    [[ -n $d && -d $d ]] && rm -rf -- "$d"
  done
  return 0
}

run_step() { # number description function
  local n=$1 desc=$2 fn=$3 t0
  STEP_WHY=
  t0=$(date +%s%N)
  if "$fn"; then
    PASSES=$((PASSES + 1))
    printf 'PASS step %s: %s (%s)\n' "$n" "$desc" "$(elapsed "$t0")"
    return 0
  fi
  FAILS=$((FAILS + 1))
  printf 'FAIL step %s: %s — %s (%s)\n' "$n" "$desc" "${STEP_WHY:-no reason recorded}" "$(elapsed "$t0")"
  return 1
}

skip_step() { # number description why
  SKIPS=$((SKIPS + 1))
  printf 'SKIP step %s: %s — %s\n' "$1" "$2" "$3"
}

usage() { sed -n '2,54p' "$0" | sed 's/^# \{0,1\}//'; }

preflight() {
  if [[ -n ${SMOKE_BASE:-} || -n ${SMOKE_TOKEN_FILE:-} ]]; then
    if [[ -z ${SMOKE_BASE:-} || -z ${SMOKE_TOKEN_FILE:-} ]]; then
      echo "SMOKE_BASE and SMOKE_TOKEN_FILE must be set together (hub mode)" >&2
      exit 2
    fi
    HUB_MODE=1
  fi
  command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }
  command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }
  [[ -r $helper ]] || { echo "missing $helper" >&2; exit 2; }
  (( HUB_MODE )) || command -v eidolon >/dev/null || {
    echo "eidolon is required for door mode (or set SMOKE_BASE + SMOKE_TOKEN_FILE)" >&2
    exit 2
  }

  OUT=$(mktemp -d) || exit 2
  CWD_DIR=$(mktemp -d) || exit 2
  MUT_DIR=$(mktemp -d) || exit 2
  RAW=$OUT/frames.raw; : >"$RAW"
  RAW2=$OUT/frames-reverse.raw; : >"$RAW2"
  HDR=$OUT/auth.header
  HDR2=$OUT/auth2.header
  BODY=$OUT/body.json
  DOOR_LOG=$OUT/door.log
  DOOR2_LOG=$OUT/door2.log
  trap cleanup EXIT
  printf 'webui-smoke: model=%s turn timeout=%ss; scratch %s\n' "$SMOKE_MODEL" "$SMOKE_TURN_TIMEOUT" "$OUT"
}

main() {
  case ${1:-} in
    -h | --help) usage; exit 0 ;;
  esac
  preflight

  local base_ok=0 stream_ok=0
  if run_step 1 "door base and token file from the boot lines" step1; then base_ok=1; fi
  if (( base_ok )); then
    if run_step 2 "GET <base>events: hello, then caught-up" step2; then stream_ok=1; fi
  else
    skip_step 2 "GET <base>events: hello, then caught-up" "step 1 gave no base and no token"
  fi
  if (( base_ok )); then
    run_step 3 "the token gate" step3
  else
    skip_step 3 "the token gate" "no door to refuse anything"
  fi
  # The three model steps stand on the stream, not on each other: one of them
  # failing still tells the reader what the other two do.
  if (( stream_ok )); then
    run_step 4 "a silent bash call: its frames, and no ask" step4
    run_step 5 "a gated call answered yes: file on disk" step5
    run_step 6 "the same call answered no: declined, file stays" step6
  else
    skip_step 4 "a silent bash call: its frames, and no ask" "no stream to drive"
    skip_step 5 "a gated call answered yes: file on disk" "no stream to drive"
    skip_step 6 "the same call answered no: declined, file stays" "no stream to drive"
  fi
  # Shutdown needs the door and the stream, not the model steps.
  if (( stream_ok )) || { (( HUB_MODE )) && (( base_ok )); }; then
    run_step 7 "shutdown, stream first then SIGTERM, and the reverse order" step7
  else
    skip_step 7 "shutdown, stream first then SIGTERM, and the reverse order" "no stream was opened"
  fi
  run_step 8 "clean up the temp dirs" step8

  printf '\nwebui-smoke: %s PASS, %s FAIL, %s SKIP\n' "$PASSES" "$FAILS" "$SKIPS"
  (( FAILS == 0 )) || exit 1
  exit 0
}

main "$@"
