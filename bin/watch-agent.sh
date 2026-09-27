#!/usr/bin/env bash
# Run one eidolon task inside WSL from Git Bash. eidolon upstream is WSL-only,
# so this calls the `eidolon` on the WSL login PATH, not a native .exe.
# Contract: pinned model, separate stdout/stderr, a status file every ten
# seconds, the run killed at the deadline (exit 124), and a unique run name so
# evidence is never overwritten. No --no-fallback: upstream `run --model` already
# refuses an unknown model instead of swapping it (fallback is resume-only).
#
#   bin/watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]
#
#   AGENT_MODEL model to pin for this run (default ollama:deepseek-v4.1-flash);
#               recorded in the status file, so a comparison row names its brain
#   WSL_DISTRO  distro to run in (default Ubuntu)
#   AGENT_CWD   WSL path for the session's tools (default: this repo under /mnt)
#   AGENT_YOLO  1 arms --yolo for this run only (every Flag answered yes; a
#               Deny still refuses). Off by default -- a headless run with it
#               off just stalls on the first flagged tool call instead of
#               silently running unattended with every question pre-answered.
set -u
task_file=${1:?usage: watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]}
run_name=${2:?usage: watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]}
timeout_s=${3:-480}
model=${AGENT_MODEL:-ollama:deepseek-v4.1-flash}
distro=${WSL_DISTRO:-Ubuntu}
[[ $run_name =~ ^[A-Za-z0-9_-]+$ ]] || { echo "bad run name: $run_name" >&2; exit 2; }
[[ $timeout_s =~ ^[0-9]+$ ]] || { echo "bad timeout: $timeout_s" >&2; exit 2; }
[[ -f $task_file ]] || { echo "no task file: $task_file" >&2; exit 2; }
repo=$(cd "$(dirname "$0")/.." && pwd)
out_dir=$repo/logs/agents
mkdir -p "$out_dir"
stdout=$out_dir/$run_name.stdout.log
stderr=$out_dir/$run_name.stderr.log
status=$out_dir/$run_name.status.json
task_copy=$out_dir/$run_name.task.txt
[[ -e $stdout || -e $stderr ]] && { echo "run name exists; choose a unique one" >&2; exit 2; }
cp "$task_file" "$task_copy"

# /c/Users/... -> /mnt/c/Users/...
to_wsl() { printf '%s\n' "$1" | sed -E 's#^/([A-Za-z])/#/mnt/\L\1/#'; }
cwd=${AGENT_CWD:-$(to_wsl "$repo")}
task_wsl=$(to_wsl "$task_copy")

wsl() { MSYS_NO_PATHCONV=1 wsl.exe -d "$distro" -e bash -lc "$@"; }
wsl 'command -v eidolon >/dev/null' \
  || { echo "no eidolon on the $distro login PATH" >&2; exit 2; }

write_status() { # state [exit_code]
  printf '{"pid":%s,"distro":"%s","model":"%s","state":"%s","exitCode":%s,"elapsedSeconds":%s,"timeoutSeconds":%s,"stdoutBytes":%s,"stderrBytes":%s,"checkedUtc":"%s"}\n' \
    "$pid" "$distro" "$model" "$1" "${2:-null}" "$elapsed" "$timeout_s" \
    "$(stat -c %s "$stdout" 2>/dev/null || echo 0)" "$(stat -c %s "$stderr" 2>/dev/null || echo 0)" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$status"
}

# `timeout` runs inside WSL, so the deadline kills the Linux process, not just
# the wsl.exe bridge. TERM first (eidolon cancels the turn on it), KILL 10s later.
#
# --yolo: a headless run has no user to answer a Flag, so without it every
# flagged tool call (ssh, sh/bash) is declined by NoUser and the run stalls at
# its first real step. yolo only turns Flag into yes; a Deny still refuses.
# Opt in per invocation, not baked in as a default: AGENT_YOLO=1 to arm it.
yolo_flag=; [[ "${AGENT_YOLO:-0}" = 1 ]] && yolo_flag=--yolo
wsl 'exec timeout -k 10 "$0" eidolon run --cwd "$1" --model "$2" $3 "$(cat "$4")"' \
  "$timeout_s" "$cwd" "$model" "$yolo_flag" "$task_wsl" \
  > "$stdout" 2> "$stderr" &
pid=$!
started=$SECONDS
elapsed=0
while kill -0 "$pid" 2>/dev/null; do
  elapsed=$((SECONDS - started))
  write_status running
  echo "$run_name pid=$pid elapsed=${elapsed}s stdout=$(stat -c %s "$stdout") stderr=$(stat -c %s "$stderr")"
  # Backstop only: the in-WSL timeout should have ended it already.
  if (( elapsed >= timeout_s + 30 )); then
    kill "$pid" 2>/dev/null
    break
  fi
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
done
wait "$pid" 2>/dev/null
code=$?
elapsed=$((SECONDS - started))
# timeout exits 124 on TERM and 137 when it had to KILL.
if (( code == 124 || code == 137 || elapsed >= timeout_s + 30 )); then state=timed_out; code=124
elif (( code == 0 )); then state=exited
else state=failed; fi
write_status "$state" "$code"
cat "$status"
echo "--- stderr (tail)"; tail -n 30 "$stderr"
echo "--- stdout (tail)"; tail -n 80 "$stdout"
exit "$code"
