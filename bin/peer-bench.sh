#!/usr/bin/env bash
# One scored BotForge loop run for the peer comparison: a brain model alone
# (solo) or the same brain proposing menus that jev picks from (jev). Both arms
# use run 001's task and stop rule verbatim; only the jev line differs
# (bench/tasks/aeacus-loop-{solo,jev}.task.tmpl).
#
#   bin/peer-bench.sh solo|jev RUN_NAME [TIMEOUT_SECONDS]
#
#   AGENT_MODEL  brain (default ollama:glm-5.3)
#   JEV_CKPT     chooser checkpoint, repo-relative (default jev/runs/jev-base-v0.pt)
#   JEV_PYTHON   WSL python with torch+fastapi+uvicorn (default /tmp/venvtest/bin/python)
#   JEV_PORT     loopback port for the chooser (default 8090)
#
# The jev arm starts jev/server.py in WSL for this run only, with a fresh token
# and its decision log at logs/botforge/RUN_NAME.decisions.jsonl, and stops it
# after. The practice VM is shared: run these one at a time.
set -u
arm=${1:?usage: peer-bench.sh solo|jev RUN_NAME [TIMEOUT_SECONDS]}
run=${2:?usage: peer-bench.sh solo|jev RUN_NAME [TIMEOUT_SECONDS]}
timeout_s=${3:-1500}
[[ $arm = solo || $arm = jev ]] || { echo "arm must be solo or jev" >&2; exit 2; }
[[ $run =~ ^[A-Za-z0-9_-]+$ ]] || { echo "bad run name: $run" >&2; exit 2; }
repo=$(cd "$(dirname "$0")/.." && pwd)
distro=${WSL_DISTRO:-Ubuntu}
export AGENT_MODEL=${AGENT_MODEL:-ollama:glm-5.3}
ckpt=${JEV_CKPT:-jev/runs/jev-base-v0.pt}
py=${JEV_PYTHON:-/tmp/venvtest/bin/python}
port=${JEV_PORT:-8090}
task=$repo/logs/agents/$run.task.src.txt
mkdir -p "$repo/logs/agents" "$repo/logs/botforge"
[[ -e $task ]] && { echo "run name exists; choose a unique one" >&2; exit 2; }

wsl() { MSYS_NO_PATHCONV=1 wsl.exe -d "$distro" -e bash -lc "$@"; }
wsl_repo=$(printf '%s\n' "$repo" | sed -E 's#^/([A-Za-z])/#/mnt/\L\1/#')

token=
if [[ $arm = jev ]]; then
  token=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
  pidfile=/tmp/jev-$run.pid
  # setsid -f detaches before this wsl.exe call returns (a plain `&` job dies
  # with it, before the server even starts); the pidfile is how it ends.
  wsl 'cd "$0/jev" || exit 1
       export EIDOLON_SERVICE_PORT="$1" EIDOLON_SERVICE_TOKEN="$2" JEVLIKE_CKPT="$0/$3" \
         JEV_DECISIONS_LOG="$0/logs/botforge/$4.decisions.jsonl" HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1
       setsid -f sh -c '"'"'echo $$ > "$1"; exec "$2" -u server.py'"'"' _ "$6" "$5" \
         > "$0/logs/botforge/$4.jev.log" 2>&1 < /dev/null' \
    "$wsl_repo" "$port" "$token" "$ckpt" "$run" "$py" "$pidfile"
  for _ in $(seq 30); do
    wsl 'curl -sf -m 2 "http://127.0.0.1:$0/health"' "$port" >/dev/null && break
    sleep 1
  done
  wsl 'curl -sf -m 2 "http://127.0.0.1:$0/health"' "$port" \
    || { echo "jev service did not come up; see logs/botforge/$run.jev.log" >&2; exit 3; }
  stop_jev() { wsl 'kill "$(cat "$0")" 2>/dev/null; rm -f "$0"' "$pidfile"; }
  trap stop_jev EXIT
fi

sed -e "s/@RUN@/$run/g" -e "s/@JEV_TOKEN@/$token/g" -e "s/@JEV_PORT@/$port/g" \
  "$repo/bench/tasks/aeacus-loop-$arm.task.tmpl" > "$task"
AGENT_YOLO=1 "$repo/bin/watch-agent.sh" "$task" "$run" "$timeout_s"
code=$?
echo "--- $run ($arm, $AGENT_MODEL): $(grep -iE 'final.*score:? [0-9]+/256|score:? [0-9]+/256' "$repo/logs/botforge/$run.md" 2>/dev/null | tail -1)"
exit "$code"
