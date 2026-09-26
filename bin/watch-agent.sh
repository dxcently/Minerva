#!/usr/bin/env bash
# Git Bash twin of watch-agent.ps1, which needs PowerShell 7 and this box has 5.1.
# Same contract: pinned model, no fallback, separate stdout/stderr, a status file
# every ten seconds, the launched process tree killed at the deadline (exit 124),
# and a unique run name so evidence is never overwritten.
#
#   bin/watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]
set -u

task_file=${1:?usage: watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]}
run_name=${2:?usage: watch-agent.sh TASK_FILE RUN_NAME [TIMEOUT_SECONDS]}
timeout_s=${3:-480}
model=ollama:deepseek-v4.1-flash

[[ $run_name =~ ^[A-Za-z0-9_-]+$ ]] || { echo "bad run name: $run_name" >&2; exit 2; }
[[ -f $task_file ]] || { echo "no task file: $task_file" >&2; exit 2; }

repo=$(cd "$(dirname "$0")/.." && pwd)
out_dir=$repo/logs/agents
mkdir -p "$out_dir"
stdout=$out_dir/$run_name.stdout.log
stderr=$out_dir/$run_name.stderr.log
status=$out_dir/$run_name.status.json
[[ -e $stdout || -e $stderr ]] && { echo "run name exists; choose a unique one" >&2; exit 2; }
cp "$task_file" "$out_dir/$run_name.task.txt"

exe=
for flavor in release debug; do
  [[ -x $repo/eidolon/target/$flavor/eidolon.exe ]] && { exe=$repo/eidolon/target/$flavor/eidolon.exe; break; }
done
[[ -n $exe ]] || { echo "no built eidolon executable" >&2; exit 2; }

[[ -x "/c/Program Files/Git/bin/bash.exe" ]] && export PATH="/c/Program Files/Git/bin:$PATH"

write_status() { # state [exit_code]
  printf '{"pid":%s,"winpid":%s,"model":"%s","state":"%s","exitCode":%s,"elapsedSeconds":%s,"timeoutSeconds":%s,"stdoutBytes":%s,"stderrBytes":%s,"checkedUtc":"%s"}\n' \
    "$pid" "${winpid:-null}" "$model" "$1" "${2:-null}" "$elapsed" "$timeout_s" \
    "$(stat -c %s "$stdout" 2>/dev/null || echo 0)" "$(stat -c %s "$stderr" 2>/dev/null || echo 0)" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$status"
}

# AGENT_CWD points the session elsewhere, e.g. a \\wsl.localhost\ tree.
"$exe" run --cwd "${AGENT_CWD:-$(cd "$repo" && pwd -W)}" --model "$model" --no-fallback "$(cat "$task_file")" \
  > "$stdout" 2> "$stderr" &
pid=$!
winpid=$(cat "/proc/$pid/winpid" 2>/dev/null || echo null)
started=$SECONDS
elapsed=0
timed_out=0

while kill -0 "$pid" 2>/dev/null; do
  elapsed=$((SECONDS - started))
  write_status running
  echo "$run_name pid=$pid winpid=$winpid elapsed=${elapsed}s stdout=$(stat -c %s "$stdout") stderr=$(stat -c %s "$stderr")"
  if (( elapsed >= timeout_s )); then
    timed_out=1
    # The whole native tree, not just the MSYS wrapper.
    [[ $winpid != null ]] && taskkill //T //F //PID "$winpid" > /dev/null 2>&1
    kill "$pid" 2>/dev/null
    break
  fi
  for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
done

wait "$pid" 2>/dev/null
code=$?
elapsed=$((SECONDS - started))
if (( timed_out )); then state=timed_out; code=124
elif (( code == 0 )); then state=exited
else state=failed; fi
write_status "$state" "$code"

cat "$status"
echo "--- stderr (tail)"; tail -n 30 "$stderr"
echo "--- stdout (tail)"; tail -n 80 "$stdout"
exit "$code"
