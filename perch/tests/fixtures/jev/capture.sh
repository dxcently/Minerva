#!/usr/bin/env bash
# capture.sh -- capture real `eidolon workflow` journals of jev's wiki-hop graph as fixtures (slice F0).
#
# Nothing here is a model call or a network call. The runs are the real eidolon (a Linux binary, so run this
# in WSL) with `--provider mock`, the real jev plugin, the fake `browser` plugin (a five-page fake of
# en.wikipedia.org) and tests/jev/stub_service.py as the jev service, all of it taken out of the eidolon-plugins
# repository with `git archive` (no checkout, no switch, nothing written there) into a throwaway tree under /tmp.
#
#   capture.sh                  capture every case into perch/tests/fixtures/jev/ and webui/term/tests/fixtures/jev/
#   capture.sh b e              only those cases
#   capture.sh --check          re-run every case into the throwaway tree and compare it with the kept fixtures:
#                               record kinds and their order (a WorkflowStep by its tool), exit codes, the door's
#                               last line with the session path left out. Ids and timestamps are not compared.
#   capture.sh --verify         look only at the kept fixtures (no eidolon, no stub): journal ids dense, no ModelSpend,
#                               no secret or machine path, both folders identical
#   --keep                      leave the throwaway tree behind (it is $F0_TMP, /tmp/minerva-f0-capture)
#   --explore                   print what each case did and assert nothing (used while the expectations were found)
#
# Environment: PLUGINS_REPO (the eidolon-plugins checkout; ~/Projects/eidolon-plugins or /mnt/c/Users/*/Projects/...),
# P1_REF (jev-wiki-hop-max, the branch P1 committed), BASE_REF (origin/main: case e needs the graph from before P1),
# EIDOLON_BIN (the Linux binary, ~/.local/bin/eidolon; never the Minerva bin/ stub, which needs the real HOME),
# STUB_PORT (48091; 8090, 8091 and 47820 are refused), F0_TMP.
#
# Every start of eidolon rewrites <config>/eidolon/system, so every eidolon call goes through eid(), which first
# asserts that HOME and all XDG_* directories are inside the throwaway tree. The swarm is off in its config.toml.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../../../.." && pwd)
fix_perch=$root/perch/tests/fixtures/jev
fix_web=$root/webui/term/tests/fixtures/jev
real_home=$HOME

CASES="a b c d e f g h i j k l"
mode=write
keep=0
explore=0
only=()
while [ $# -gt 0 ]; do
  case $1 in
    --check) mode=check ;;
    --verify) mode=verify ;;
    --keep) keep=1 ;;
    --explore) explore=1 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "capture.sh: unknown option $1" >&2; exit 2 ;;
    *) only+=("$1") ;;
  esac
  shift
done
[ ${#only[@]} -gt 0 ] && CASES="${only[*]}"
[ "$explore" = 1 ] && mode=explore

die() { echo "capture.sh: $*" >&2; exit 2; }

P1_REF=${P1_REF:-jev-wiki-hop-max}
BASE_REF=${BASE_REF:-origin/main}
STUB_PORT=${STUB_PORT:-48091}
T=${F0_TMP:-/tmp/minerva-f0-capture}
case $STUB_PORT in 8090|8091|47820) die "port $STUB_PORT is reserved (the real jev and browser services, the operator's perch); the stub runs on 48091" ;; esac
case $T in /tmp/?*) ;; *) die "F0_TMP ($T) must be a directory under /tmp" ;; esac

py() { python3 "$T/cap.py" "$@"; }

# ---- the throwaway tree --------------------------------------------------------------------------------------

# every eidolon call asserts this first: HOME and each XDG_* directory are inside the tree, the tree is under /tmp,
# and none of them is the real home or its .config
assert_temp() {
  local v d
  for v in HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME XDG_RUNTIME_DIR; do
    d=${!v:-}
    [ -n "$d" ] || die "$v is not set; refusing to start eidolon against a real directory"
    case $(realpath -m "$d") in
      "$T"/*) ;;
      *) die "$v ($d) is not inside the throwaway tree $T" ;;
    esac
    case $(realpath -m "$d") in
      "$real_home"|"$real_home/.config"|"$real_home/.local"/*|"$real_home/.config"/*) die "$v ($d) is the real home" ;;
    esac
  done
}

eid() { assert_temp; "$EID" "$@"; }

write_helpers() {
  cat > "$T/cap.py" <<'PYEOF'
#!/usr/bin/env python3
# capture.sh's reader: it assembles a case, reduces journals to shapes and compares them. It is written out
# by capture.sh into the temp tree and run from there; nothing here touches eidolon, the network or a model.
import json
import os
import re
import sys

PLAIN_KINDS = {"SessionStart", "WorkflowStart", "WorkflowStep", "PolicyVerdict", "ToolResult", "ApiRequest", "AskUser", "WorkflowEnd"}


def records(path):
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def kind_of(r):
    k = next(iter(r["kind"]))
    return k, r["kind"][k]


def sig(r):
    # the shape of a record: its kind, plus the one field that tells a reader which of that kind it is
    k, b = kind_of(r)
    if k == "WorkflowStep":
        return k + ":" + b["tool"]
    if k == "ToolResult":
        return k + (":err" if b.get("is_error") else ":ok")
    if k == "WorkflowEnd":
        return k + ":" + str(b.get("status"))
    return k


def last_line(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = [l for l in f.read().split("\n") if l.strip()]
    except OSError:
        return ""
    return lines[-1] if lines else ""


def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def parse_line(s):
    try:
        v = json.loads(s)
    except ValueError:
        return None
    return v if isinstance(v, dict) else None


def norm_stdout(s):
    # the session path differs per capture; the rest of the door's line is the case
    v = parse_line(s)
    if v is None:
        return s
    v.pop("session", None)
    rep = v.get("report")
    if isinstance(rep, str):
        r = parse_line(rep)
        if r is not None:
            v["report"] = r
    return json.dumps(v, sort_keys=True)


def norm_stderr(s):
    return re.sub(r"\d{9,}", "N", s).strip()


def cmd_session(path):
    # prints "<session>\t<run>\t<status>" of the last stdout line, empty fields when it is not the door's JSON
    v = parse_line(last_line(path)) or {}
    print("%s\t%s\t%s" % (v.get("session", ""), v.get("run", ""), v.get("status", "")))


def cmd_records(path):
    print(len(records(path)))


def cmd_proof(path):
    # a mock run: the first record is a SessionStart for model mock, and no record is a ModelSpend
    rs = records(path)
    k, b = kind_of(rs[0])
    if k != "SessionStart" or b.get("model") != "mock":
        sys.exit("first record is %s, model %r" % (k, b.get("model") if isinstance(b, dict) else None))
    if any(kind_of(r)[0] == "ModelSpend" for r in rs):
        sys.exit("a ModelSpend record")


def cmd_kinds(path):
    seen = []
    for r in records(path):
        k = kind_of(r)[0]
        if k not in seen:
            seen.append(k)
    print(" ".join(seen))


def cmd_lastline(path):
    print(last_line(path))


def cmd_field(path, name):
    # a field of the door's last line, or report.<field> of the JSON string it carries
    v = parse_line(last_line(path)) or {}
    for part in name.split("."):
        if isinstance(v, str):
            v = parse_line(v) or {}
        v = v.get(part, "") if isinstance(v, dict) else ""
    print(v if not isinstance(v, (dict, list)) else json.dumps(v, sort_keys=True))


def cmd_build(work, out):
    env = os.environ
    stub_env = {}
    for kv in env.get("C_STUBENV", "").split():
        k, _, v = kv.partition("=")
        stub_env[k] = v
    doc = {
        "case": env["C_ID"],
        "title": env["C_TITLE"],
        "stub": {"mode": env["C_MODE"], "env": stub_env},
        "graph": {"variant": env["C_VARIANT"], "sha256": env["C_GRAPHSHA"]},
        "args": json.loads(env["C_ARGS"]),
    }
    invs = []
    n = 0
    while os.path.exists("%s/%02d.kind" % (work, n + 1)):
        n += 1
        p = "%s/%02d" % (work, n)
        invs.append({
            "n": n,
            "cmd": read(p + ".kind").strip(),
            "answer": read(p + ".answer") if os.path.exists(p + ".answer") else None,
            "signal": read(p + ".signal").strip() if os.path.exists(p + ".signal") else None,
            "exit": int(read(p + ".exit").strip()),
            "stdout": last_line(p + ".stdout"),
            "stderr": read(p + ".stderr").rstrip("\n"),
            "records": int(read(p + ".records").strip()),
        })
    doc["invocations"] = invs
    os.makedirs(out, exist_ok=True)
    with open(out + "/case.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, indent=2, ensure_ascii=False)
        f.write("\n")
    with open(work + "/journal.ndjson", encoding="utf-8") as src, \
            open(out + "/journal.ndjson", "w", encoding="utf-8", newline="\n") as dst:
        for line in src:
            if line.strip():
                dst.write(line.rstrip("\n") + "\n")


def cmd_meta(root):
    env = os.environ
    cases = sorted(d for d in os.listdir(root) if os.path.isfile("%s/%s/case.json" % (root, d)))
    doc = {
        "eidolon": {
            "version": env["EID_VERSION"],
            "binary_sha256": env["EID_SHA256"],
            "commit_planned": "bab3813 (VENDORED.md says the WSL binary is built from it; the binary does not carry its commit)",
        },
        "plugins": {"p1_ref": env["P1_REF"], "p1_sha": env["P1_SHA"], "base_ref": env["BASE_REF"], "base_sha": env["BASE_SHA"]},
        "graphs": {"p1": env["GRAPH_SHA_P1"], "pre": env["GRAPH_SHA_PRE"]},
        "stub": {
            "port": int(env["STUB_PORT"]),
            "source": "tests/jev/stub_service.py at p1_sha, unchanged; cases d, g, j, k and l run it through a wrapper that adds STUB_BREAK or STUB_DELAY_CHOOSE",
        },
        "cases": cases,
    }
    json.dump(doc, sys.stdout, indent=2)
    sys.stdout.write("\n")


def shape(case_dir):
    return [sig(r) for r in records(case_dir + "/journal.ndjson")]


def case_view(case_dir):
    c = json.loads(read(case_dir + "/case.json"))
    return {
        "stub": c.get("stub"),
        "graph": (c.get("graph") or {}).get("variant"),
        "args": c.get("args"),
        "invocations": [
            {"cmd": i["cmd"], "answer": i["answer"], "signal": i.get("signal"), "exit": i["exit"],
             "stdout": norm_stdout(i["stdout"]), "stderr": norm_stderr(i["stderr"]), "records": i["records"]}
            for i in c["invocations"]
        ],
    }


def cmd_compare(fresh, kept):
    bad = []
    a, b = shape(fresh), shape(kept)
    if a != b:
        i = 0
        while i < min(len(a), len(b)) and a[i] == b[i]:
            i += 1
        bad.append("record kinds differ from record %d: fresh %s, kept %s" % (
            i, a[i] if i < len(a) else "(end)", b[i] if i < len(b) else "(end)"))
        bad.append("fresh %d records, kept %d" % (len(a), len(b)))
    va, vb = case_view(fresh), case_view(kept)
    for key in ("stub", "graph", "args"):
        if va[key] != vb[key]:
            bad.append("%s: fresh %r, kept %r" % (key, va[key], vb[key]))
    if len(va["invocations"]) != len(vb["invocations"]):
        bad.append("invocations: fresh %d, kept %d" % (len(va["invocations"]), len(vb["invocations"])))
    else:
        for i, (x, y) in enumerate(zip(va["invocations"], vb["invocations"])):
            for key in x:
                if x[key] != y[key]:
                    bad.append("invocation %d %s: fresh %s, kept %s" % (i + 1, key, x[key], y[key]))
    for line in bad:
        print(line)
    sys.exit(1 if bad else 0)


def cmd_shape(case_dir):
    for i, s in enumerate(shape(case_dir)):
        print(i, s)


def cmd_tools(case_dir):
    # the tool sequence of the run, then what each invocation said
    rs = records(case_dir + "/journal.ndjson")
    seq = [kind_of(r)[1]["tool"] for r in rs if kind_of(r)[0] == "WorkflowStep"]
    print("tools: " + " ".join(seq))
    c = json.loads(read(case_dir + "/case.json"))
    for i in c["invocations"]:
        print("  #%d %s%s%s exit=%d records=%d" % (
            i["n"], i["cmd"], (" answer=" + i["answer"]) if i["answer"] is not None else "",
            (" signal=" + i["signal"]) if i.get("signal") else "", i["exit"], i["records"]))
        print("     stdout: " + i["stdout"][:600])
        if i["stderr"]:
            print("     stderr: " + i["stderr"].replace("\n", " | ")[:600])


def cmd_verify(root):
    bad = []
    cases = sorted(d for d in os.listdir(root) if os.path.isfile("%s/%s/case.json" % (root, d)))
    if not cases:
        bad.append("no case directories under " + root)
    for d in cases:
        cd = "%s/%s" % (root, d)
        try:
            rs = records(cd + "/journal.ndjson")
        except (OSError, ValueError) as e:
            bad.append("%s: journal unreadable: %s" % (d, e))
            continue
        if not rs:
            bad.append("%s: empty journal" % d)
            continue
        if [r["id"] for r in rs] != list(range(len(rs))):
            bad.append("%s: record ids are not 0..%d in order" % (d, len(rs) - 1))
        k0, b0 = kind_of(rs[0])
        if k0 != "SessionStart" or b0.get("model") != "mock":
            bad.append("%s: the first record is not a mock SessionStart" % d)
        kinds = {kind_of(r)[0] for r in rs}
        if "ModelSpend" in kinds:
            bad.append("%s: a ModelSpend record (a model was called)" % d)
        c = json.loads(read(cd + "/case.json"))
        if c["invocations"][-1]["records"] != len(rs):
            bad.append("%s: the last invocation says %d records, the journal has %d" % (d, c["invocations"][-1]["records"], len(rs)))
        prev = 0
        for i in c["invocations"]:
            if i["records"] < prev:
                bad.append("%s: invocation %d has fewer records than the one before" % (d, i["n"]))
            prev = i["records"]
        for name in ("journal.ndjson", "case.json"):
            text = read("%s/%s" % (cd, name))
            for needle in ("/mnt/c", "/home/", "Authorization", "Bearer "):
                if needle in text:
                    bad.append("%s/%s mentions %r" % (d, name, needle))
        extra = " ".join(sorted(kinds - PLAIN_KINDS)) or "-"
        print("%-3s %4d records  extra kinds: %s" % (d, len(rs), extra))
    for line in bad:
        print("BAD " + line)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    cmd = sys.argv[1]
    fn = globals().get("cmd_" + cmd)
    if fn is None:
        sys.exit("cap.py: no command " + cmd)
    fn(*sys.argv[2:])
PYEOF
  cat > "$T/stubwrap.py" <<'PYEOF'
#!/usr/bin/env python3
# The stand-in jev service of tests/jev/stub_service.py, unchanged, with the faults it has no mode for.
# STUB_BREAK=entail makes jev_entail answer an error while jev_choose still answers, STUB_BREAK=choose the other way
# round (--mode none breaks both). STUB_DELAY_CHOOSE=<seconds> holds each jev_choose call that long, so a run can be
# stopped inside one. It serves each request on its own thread, so /calls still answers during a held call.
import os
import sys
import time
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.environ["STUB_DIR"])
import stub_service as s  # noqa: E402

s.HTTPServer = ThreadingHTTPServer

broken = os.environ.get("STUB_BREAK", "")
if broken == "entail":
    def entail(args):
        raise ValueError("stub: the entail model is not loaded")
    s.entail = entail
elif broken == "choose":
    def choose(args):
        raise ValueError("stub: the chooser model is not loaded")
    s.choose = choose
elif broken:
    sys.exit("stubwrap: STUB_BREAK is entail or choose, not " + broken)

delay = float(os.environ.get("STUB_DELAY_CHOOSE", "0") or "0")
if delay > 0:
    _choose = s.choose

    def choose(args):
        time.sleep(delay)
        return _choose(args)
    s.choose = choose

s.main()
PYEOF
}

stub_pid=
stop_stub() {
  if [ -n "$stub_pid" ]; then
    kill "$stub_pid" 2>/dev/null || true
    wait "$stub_pid" 2>/dev/null || true
    stub_pid=
  fi
}
cleanup() {
  stop_stub
  if [ -n "${T_MADE:-}" ] && [ "$keep" = 0 ]; then rm -rf "$T"; fi
}

make_tree() {
  [ ! -e "$T" ] || die "$T exists (a capture in progress, or one kept with --keep); remove it first"
  mkdir -p "$T"/{src,base}
  T_MADE=1
  trap cleanup EXIT
  write_helpers
}

# HOME, the five XDG directories, the config with the swarm off, the token, the plugins
setup_env() {
  mkdir -p "$T"/{home,cfg,data,state,cache,run,cwd,work,out}
  chmod 700 "$T/run"
  export HOME=$T/home XDG_CONFIG_HOME=$T/cfg XDG_DATA_HOME=$T/data XDG_STATE_HOME=$T/state XDG_CACHE_HOME=$T/cache XDG_RUNTIME_DIR=$T/run
  assert_temp
  # P1's own guard (HOME, XDG_CONFIG_HOME and XDG_RUNTIME_DIR temporary, swarm off): it writes the config.toml
  . "$T/src/tests/jev/guard.sh"
  require_temp_config || die "guard.sh refused the throwaway tree"
  grep -q '^enabled = false' "$XDG_CONFIG_HOME/eidolon/config.toml" || die "the swarm is not off in $XDG_CONFIG_HOME/eidolon/config.toml"
  tok=$HOME/.config/eidolon/jev.token
  mkdir -p "$(dirname "$tok")"
  (umask 077; head -c 24 /dev/urandom | base64 > "$tok")
  plug=$XDG_CONFIG_HOME/eidolon/plugins
  mkdir -p "$plug"
  installed=
}

# the plugins out of the eidolon-plugins repository, read-only
fetch_plugins() {
  PLUGINS_REPO=${PLUGINS_REPO:-}
  if [ -z "$PLUGINS_REPO" ]; then
    local c
    for c in "$real_home/Projects/eidolon-plugins" /mnt/c/Users/*/Projects/eidolon-plugins; do
      if [ -e "$c/.git" ]; then PLUGINS_REPO=$c; break; fi
    done
  fi
  [ -n "$PLUGINS_REPO" ] || die "no eidolon-plugins checkout found; set PLUGINS_REPO"
  p1_sha=$(git -C "$PLUGINS_REPO" rev-parse --verify "$P1_REF^{commit}") || die "$P1_REF is not in $PLUGINS_REPO"
  base_sha=$(git -C "$PLUGINS_REPO" rev-parse --verify "$BASE_REF^{commit}") || die "$BASE_REF is not in $PLUGINS_REPO"
  git -C "$PLUGINS_REPO" archive "$p1_sha" jev tests/jev | tar -x -C "$T/src"
  git -C "$PLUGINS_REPO" archive "$base_sha" jev/graphs/wiki-hop.json jev/workflows/wiki-hop.rn | tar -x -C "$T/base"
  graph_sha_p1=$(sha256sum "$T/src/jev/graphs/wiki-hop.json" | cut -d' ' -f1)
  graph_sha_pre=$(sha256sum "$T/base/jev/graphs/wiki-hop.json" | cut -d' ' -f1)
}

# variant p1: the jev and fake-browser P1 committed. variant pre: the same, with the graph and workflow of BASE_REF
# (no `max` on the link snapshot). The ports and token_file are rewritten in the COPY only.
install_plugins() {
  [ "$installed" = "$1" ] && return 0
  rm -rf "$plug/jev" "$plug/browser"
  cp -r "$T/src/jev" "$plug/jev"
  cp -r "$T/src/tests/jev/fake-browser" "$plug/browser"
  if [ "$1" = pre ]; then
    cp "$T/base/jev/graphs/wiki-hop.json" "$plug/jev/graphs/wiki-hop.json"
    cp "$T/base/jev/workflows/wiki-hop.rn" "$plug/jev/workflows/wiki-hop.rn"
  fi
  local f
  for f in choose entail; do
    sed -i -e "s|http://127.0.0.1:8091|http://127.0.0.1:$STUB_PORT|" \
      -e "s|token_file: \"~/.config/eidolon/jev.token\"|token_file: \"$tok\"|" "$plug/jev/tools/$f.rn"
    grep -q "127.0.0.1:$STUB_PORT\"" "$plug/jev/tools/$f.rn" || die "the port rewrite did not apply to tools/$f.rn"
    grep -q "token_file: \"$tok\"" "$plug/jev/tools/$f.rn" || die "the token_file rewrite did not apply to tools/$f.rn"
  done
  # the workflow's PIN is the graph's hash (tests/jev/pin.sh --check, kept here so a copy cannot drift)
  local want got
  want=$(grep -o 'sha256:[0-9a-f]*' "$plug/jev/workflows/wiki-hop.rn" | head -n 1)
  got=sha256:$(sha256sum "$plug/jev/graphs/wiki-hop.json" | cut -d' ' -f1)
  [ "$want" = "$got" ] || die "wiki-hop.rn pins $want but the graph is $got ($1)"
  eid plugins trust jev >/dev/null
  eid plugins trust browser >/dev/null
  local v
  for v in jev_choose jev_entail; do eid plugins grant "$v" "file:$tok" >/dev/null; done
  installed=$1
}

# the stand-in service on STUB_PORT: MODE and any VAR=value faults (see stubwrap.py)
start_stub() {
  stop_stub
  if (exec 3<>/dev/tcp/127.0.0.1/"$STUB_PORT") 2>/dev/null; then die "something already listens on 127.0.0.1:$STUB_PORT"; fi
  local mode=$1; shift
  (
    export STUB_DIR=$T/src/tests/jev
    local kv
    for kv in "$@"; do export "$kv"; done
    exec python3 "$T/stubwrap.py" --port "$STUB_PORT" --token-file "$tok" --mode "$mode" >/dev/null 2>&1
  ) &
  stub_pid=$!
  local i
  for i in $(seq 50); do
    (exec 3<>/dev/tcp/127.0.0.1/"$STUB_PORT") 2>/dev/null && break
    sleep 0.1
  done
  local health
  health=$(curl -s "http://127.0.0.1:$STUB_PORT/health" || true)
  case $health in *'"decisions_log": "off"'*) ;; *) die "the stub's /health does not say decisions_log off: $health" ;; esac
}

# ---- one case ------------------------------------------------------------------------------------------------

# every kind of record the kept cases hold; one that is not here is a surprise, and the capture stops at it
KNOWN_KINDS="SessionStart WorkflowStart WorkflowStep PolicyVerdict ToolResult ApiRequest AskUser WorkflowEnd"
proved=0

note() { printf '%s\n' "$*" >&2; }

# a case says what it must have been; --explore only prints what it saw
expect() { # <what> <seen> <wanted>
  if [ "$explore" = 1 ]; then
    if [ "$2" = "$3" ]; then note "      [as expected] $1 = $2"; else note "      [differs] $1 = $2 (wanted $3)"; fi
    return 0
  fi
  [ "$2" = "$3" ] || die "case $C_ID: $1 is '$2', wanted '$3'"
}

begin_case() { # id title variant mode start goal [VAR=value ...]
  C_ID=$1 C_TITLE=$2 C_VARIANT=$3 C_MODE=$4
  C_ARGS=$(printf '{"start":"%s","goal":"%s"}' "$5" "$6")
  shift 6
  C_STUBENV=$*
  C_GRAPHSHA=$graph_sha_p1
  [ "$C_VARIANT" = pre ] && C_GRAPHSHA=$graph_sha_pre
  W=$T/work/$C_ID
  rm -rf "$W"
  mkdir -p "$W"
  INV=0 SESSION= RUN=
  install_plugins "$C_VARIANT"
  rm -rf "$T/fb-$C_ID"
  mkdir -p "$T/fb-$C_ID"
  export FAKE_BROWSER=$plug/browser FB_STATE=$T/fb-$C_ID
  start_stub "$C_MODE" "$@"
  echo "case $C_ID: $C_TITLE"
}

next_invocation() { INV=$((INV + 1)); P=$W/$(printf '%02d' "$INV"); }

invoke() { # run | resume <answer> | continue (a resume with no answer, for a run that was cut mid-flight)
  next_invocation
  local rc=0
  case $1 in
    run)
      printf run > "$P.kind"
      printf '%s' "$C_ARGS" | eid workflow run "$plug/jev" wiki-hop --args - --provider mock --cwd "$T/cwd" >"$P.stdout" 2>"$P.stderr" || rc=$?
      ;;
    resume)
      printf resume > "$P.kind"
      printf '%s' "$2" > "$P.answer"
      eid workflow resume --plugin "$plug/jev" --provider mock "$SESSION" "$RUN" "--answer=$2" >"$P.stdout" 2>"$P.stderr" || rc=$?
      ;;
    continue)
      printf resume > "$P.kind"
      eid workflow resume --plugin "$plug/jev" --provider mock "$SESSION" "$RUN" >"$P.stdout" 2>"$P.stderr" || rc=$?
      ;;
  esac
  finish_invocation "$rc"
}

# a run that gets TERM or KILL while jev_choose is held inside the stub (STUB_DELAY_CHOOSE)
invoke_stop() { # TERM | KILL
  next_invocation
  printf run > "$P.kind"
  printf '%s' "$1" > "$P.signal"
  assert_temp
  printf '%s' "$C_ARGS" | "$EID" workflow run "$plug/jev" wiki-hop --args - --provider mock --cwd "$T/cwd" >"$P.stdout" 2>"$P.stderr" &
  local pid=$! i calls seen=0
  for i in $(seq 200); do
    calls=$(curl -s "http://127.0.0.1:$STUB_PORT/calls" || true)
    case $calls in *'"choose"'*) seen=1; break ;; esac
    sleep 0.1
  done
  if [ $seen = 0 ]; then kill "$pid" 2>/dev/null || true; die "case $C_ID: the run never reached jev_choose"; fi
  kill -"$1" "$pid"
  local rc=0
  { wait "$pid" || rc=$?; } 2>/dev/null
  finish_invocation "$rc"
}

finish_invocation() { # exit code of the invocation
  printf '%s' "$1" > "$P.exit"
  local line
  if [ -z "$SESSION" ]; then
    line=$(py session "$P.stdout")
    SESSION=$(cut -f1 <<<"$line")
    RUN=$(cut -f2 <<<"$line")
    if [ -z "$SESSION" ]; then
      SESSION=$(ls -t "$XDG_DATA_HOME"/eidolon/sessions/*.eid 2>/dev/null | head -n 1 || true)
      RUN=1
    fi
  fi
  [ -n "$SESSION" ] || die "case $C_ID: no session after invocation $INV"
  eid log --json "$SESSION" > "$W/journal.ndjson" 2>"$P.logerr" || die "case $C_ID: eidolon log failed: $(cat "$P.logerr")"
  py records "$W/journal.ndjson" > "$P.records"
  # mock is proved on the first journal before anything else runs: a mock SessionStart and no ModelSpend record
  local spend
  spend=$(grep -c ModelSpend "$W/journal.ndjson" || true)
  py proof "$W/journal.ndjson" || die "case $C_ID: the journal is not a mock run's"
  [ "$spend" = 0 ] || die "case $C_ID: grep -c ModelSpend is $spend"
  if [ $proved = 0 ]; then echo "mock proved on the first journal: SessionStart model mock, grep -c ModelSpend $spend"; proved=1; fi
  # a record kind the cases do not know, or a provider in the door's words, stops the capture
  local k unknown=
  for k in $(py kinds "$W/journal.ndjson"); do
    case " $KNOWN_KINDS " in *" $k "*) ;; *) unknown="$unknown $k" ;; esac
  done
  if [ -n "$unknown" ]; then
    if [ "$explore" = 1 ]; then note "      [new kinds]$unknown"; else die "case $C_ID: unexpected record kinds:$unknown"; fi
  fi
  if grep -qiE 'provider (error|returned|failed)|api key|unauthori[sz]ed|rate limit' "$P.stderr" "$P.stdout"; then
    die "case $C_ID: a provider error in invocation $INV: $(tail -n 3 "$P.stderr")"
  fi
  printf '  #%d %-6s%s exit=%s records=%s  %s\n' "$INV" "$(cat "$P.kind")" "$( [ -e "$P.answer" ] && printf ' answer=%s' "$(cat "$P.answer")" )" \
    "$1" "$(cat "$P.records")" "$(py lastline "$P.stdout" | cut -c1-200)"
}

end_case() {
  stop_stub
  export C_ID C_TITLE C_MODE C_STUBENV C_VARIANT C_ARGS C_GRAPHSHA
  py build "$W" "$T/out/$C_ID"
  echo "  $(py tools "$T/out/$C_ID" | head -n 1 | cut -c1-600)"
}

# how many journal lines hold this text (grep -c, so a count of lines, not matches)
in_journal() { grep -cF -- "$1" "$W/journal.ndjson" || true; }
in_stderr() { grep -cF -- "$2" "$W/$(printf '%02d' "$1").stderr" || true; }

# the door's last line, parsed (status, and the report the graph printed)
stdout_field() { # <invocation number> <field of the door's line | report.field>
  local p; p=$W/$(printf '%02d' "$1")
  py field "$p.stdout" "$2"
}
exit_of() { cat "$W/$(printf '%02d' "$1").exit"; }

# ---- the cases -----------------------------------------------------------------------------------------------
# The fake site has five pages: Cat -> Felidae, Ancient Egypt; Felidae -> Cat; Egypt -> Cat, Felidae, Ancient Egypt;
# Ancient_Egypt -> Egypt, Cat; and a long List_of_cat_breeds (130 links, 13.5 KB, "Ancient Egypt" the 31st). A page
# with a link that is the goal is a rule pick (label equals goal); a page with one link left (visited ones are
# excluded) is a single pick; any other page goes to the chooser, the stub. Each case says what it must have been
# (expect), so a fixture cannot drift into meaning something else than its README row says.

has() { case $1 in *"$2"*) echo yes ;; *) echo no ;; esac; }

case_a() {
  begin_case a "first: a chooser pick, a rule pick, arrived" p1 first Ancient_Egypt Felidae
  invoke run
  expect "exit" "$(exit_of 1)" 0
  expect "status" "$(stdout_field 1 status)" completed
  expect "final_state" "$(stdout_field 1 report.final_state)" arrived
  expect "article path" "$(stdout_field 1 report.output.path)" '["Ancient Egypt", "Egypt", "Felidae"]'
  expect "chooser calls" "$(stdout_field 1 report.chooser_calls)" 1
  expect "rule picks" "$(stdout_field 1 report.rule_picks)" 1
  end_case
}

case_b() {
  begin_case b "flat: parked on a margin, answered 0, arrived" p1 flat Ancient_Egypt Felidae
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  expect "status of the run" "$(stdout_field 1 status)" parked
  expect "the park is a margin" "$(has "$(stdout_field 1 question)" "margin 0.000 under 0.050")" yes
  invoke resume 0
  expect "exit of the resume" "$(exit_of 2)" 0
  expect "final_state" "$(stdout_field 2 report.final_state)" arrived
  expect "escalations" "$(stdout_field 2 report.escalations)" 1
  end_case
}

case_c() {
  begin_case c "none: the chooser is unavailable, a park that cannot be resumed" p1 none Ancient_Egypt Felidae
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  expect "status of the run" "$(stdout_field 1 status)" parked
  expect "the park says why" "$(has "$(stdout_field 1 question)" "chooser unavailable: stub: the model is not loaded")" yes
  invoke resume 0
  expect "exit of the resume" "$(exit_of 2)" 3
  expect "status of the resume" "$(stdout_field 2 status)" refused
  expect "the refusal names the first failed step" "$(has "$(stdout_field 2 error)" "step 5 (w1-5 jev_entail) is journaled as a failure")" yes
  end_case
}

# eidolon races the program against the cancel token (a tokio::select! in crates/rune/src/workflow.rs), so a SIGTERM
# that lands inside jev_choose ends one of two ways, on a coin toss: the token wins and the run is `failed`, "the run
# was cancelled", with the jev_choose step left without a result (case d); or the program wins, jev_choose comes back
# "cancelled while the tool was running", the graph reads that as an unavailable chooser and parks (case l). Both are
# real, so each case runs again, up to 40 times, until it has the end it is named for.
STOP_TRIES=40

case_d() {
  local try
  for try in $(seq "$STOP_TRIES"); do
    begin_case d "first, held in jev_choose and stopped with SIGTERM: the run is cancelled" p1 first Ancient_Egypt Felidae STUB_DELAY_CHOOSE=8
    invoke_stop TERM
    [ "$(stdout_field 1 status)" = failed ] && break
    note "      (attempt $try went the other way, a park; again)"
  done
  expect "exit" "$(exit_of 1)" 2
  expect "status" "$(stdout_field 1 status)" failed
  expect "reason" "$(stdout_field 1 reason)" "the run was cancelled"
  expect "eidolon said it was cancelling" "$(in_stderr 1 '[cancelling]')" 1
  expect "the WorkflowEnd" "$(in_journal '"status":"Failed"')" 1
  expect "jev_choose has no result" "$(in_journal '"tool_use_id":"w1-6"')" 1
  end_case
}

case_l() {
  local try
  for try in $(seq "$STOP_TRIES"); do
    begin_case l "first, held in jev_choose and stopped with SIGTERM: the tool is cancelled and the graph parks" p1 first Ancient_Egypt Felidae STUB_DELAY_CHOOSE=8
    invoke_stop TERM
    [ "$(stdout_field 1 status)" = parked ] && break
    note "      (attempt $try went the other way, a failed run; again)"
  done
  expect "exit" "$(exit_of 1)" 1
  expect "status" "$(stdout_field 1 status)" parked
  expect "eidolon said it was cancelling" "$(in_stderr 1 '[cancelling]')" 1
  expect "the tool result" "$(in_journal '"content":"cancelled while the tool was running"')" 1
  expect "the park says why" "$(has "$(stdout_field 1 question)" "chooser unavailable: cancelled while the tool was running")" yes
  end_case
}

case_e() {
  begin_case e "the graph's failed final, which the door calls completed (the graph from before P1, the long article)" pre first List_of_cat_breeds "Ancient Egypt"
  invoke run
  expect "exit" "$(exit_of 1)" 0
  expect "status" "$(stdout_field 1 status)" completed
  expect "final_state" "$(stdout_field 1 report.final_state)" failed
  expect "reason" "$(stdout_field 1 report.output.reason)" truncated
  expect "the failed mark" "$(in_journal 'marked wiki-hop final failed')" 1
  end_case
}

case_f() {
  begin_case f "first: single picks and a rule pick, no chooser call" p1 first Felidae Egypt
  invoke run
  expect "exit" "$(exit_of 1)" 0
  expect "final_state" "$(stdout_field 1 report.final_state)" arrived
  expect "article path" "$(stdout_field 1 report.output.path)" '["Felidae", "Cat", "Ancient Egypt", "Egypt"]'
  expect "chooser calls" "$(stdout_field 1 report.chooser_calls)" 0
  expect "rule picks" "$(stdout_field 1 report.rule_picks)" 1
  end_case
}

case_g() {
  begin_case g "flat, jev_entail erroring: the run goes on, parks, and the park cannot be resumed" p1 flat Ancient_Egypt Felidae STUB_BREAK=entail
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  expect "status of the run" "$(stdout_field 1 status)" parked
  expect "the park is a margin" "$(has "$(stdout_field 1 question)" "margin 0.000 under 0.050")" yes
  invoke resume 0
  expect "exit of the resume" "$(exit_of 2)" 3
  expect "the refusal names the entail step" "$(has "$(stdout_field 2 error)" "step 5 (w1-5 jev_entail) is journaled as a failure")" yes
  end_case
}

case_h() {
  begin_case h "flat: an answer that is no option re-parks and spends an escalation, then 0" p1 flat Ancient_Egypt Felidae
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  invoke resume banana
  expect "exit of the bad answer" "$(exit_of 2)" 1
  expect "status of the bad answer" "$(stdout_field 2 status)" parked
  expect "the park says it" "$(has "$(stdout_field 2 question)" "'banana' is not one of the options.")" yes
  invoke resume 0
  expect "exit of the good answer" "$(exit_of 3)" 0
  expect "final_state" "$(stdout_field 3 report.final_state)" arrived
  expect "escalations" "$(stdout_field 3 report.escalations)" 2
  end_case
}

case_i() {
  begin_case i "flat on the long article: 64 options, a park under the floor, answered by label" p1 flat List_of_cat_breeds Cat
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  expect "status of the run" "$(stdout_field 1 status)" parked
  expect "the park is under the floor" "$(has "$(stdout_field 1 question)" "top 0.015 under floor 0.350")" yes
  invoke resume "Registered cat breed number 002"
  expect "exit of the resume" "$(exit_of 2)" 0
  expect "final_state (the page it clicked to is not in the fake site)" "$(stdout_field 2 report.final_state)" failed
  end_case
}

case_j() {
  begin_case j "flat, jev_choose erroring: a chooser-unavailable park, and the refusal names the jev_choose step" p1 flat Ancient_Egypt Felidae STUB_BREAK=choose
  invoke run
  expect "exit of the run" "$(exit_of 1)" 1
  expect "status of the run" "$(stdout_field 1 status)" parked
  expect "the park says why" "$(has "$(stdout_field 1 question)" "chooser unavailable: stub: the chooser model is not loaded")" yes
  invoke resume 0
  expect "exit of the resume" "$(exit_of 2)" 3
  expect "the refusal names the choose step" "$(has "$(stdout_field 2 error)" "step 6 (w1-6 jev_choose) is journaled as a failure")" yes
  end_case
}


case_k() {
  begin_case k "first, killed with SIGKILL in jev_choose (a journal with no end), then resumed with no answer" p1 first Ancient_Egypt Felidae STUB_DELAY_CHOOSE=3
  invoke_stop KILL
  expect "exit of the kill" "$(exit_of 1)" 137
  expect "the door printed nothing" "$(py lastline "$W/01.stdout")" ""
  expect "records when it was cut" "$(cat "$W/01.records")" 24
  expect "no WorkflowEnd yet" "$(in_journal WorkflowEnd)" 0
  invoke continue
  expect "exit of the resume" "$(exit_of 2)" 0
  expect "status of the resume" "$(stdout_field 2 status)" completed
  expect "final_state" "$(stdout_field 2 report.final_state)" arrived
  expect "one WorkflowEnd" "$(in_journal WorkflowEnd)" 1
  end_case
}

# ---- fixtures: write, check, verify --------------------------------------------------------------------------

# the jev token is a random value of the throwaway tree; it must be in no captured file
scan_secrets() {
  local t
  t=$(cat "$tok")
  if grep -rqF -- "$t" "$T/out" "$T/work"; then die "the jev token is in a captured file"; fi
}

export_meta() {
  export EID_VERSION EID_SHA256 P1_REF BASE_REF STUB_PORT GRAPH_SHA_P1=$graph_sha_p1 GRAPH_SHA_PRE=$graph_sha_pre P1_SHA=$p1_sha BASE_SHA=$base_sha
}

write_fixtures() {
  local d id
  export_meta
  for d in "$fix_perch" "$fix_web"; do
    mkdir -p "$d"
    for id in $CASES; do
      rm -rf "${d:?}/$id"
      cp -r "$T/out/$id" "$d/$id"
    done
    py meta "$d" > "$d/meta.json"
    printf '* text eol=lf\n' > "$d/.gitattributes"
  done
  echo "wrote: $CASES -> ${fix_perch#"$root"/} and ${fix_web#"$root"/}"
}

check_fixtures() {
  local id out rc=0
  for id in $CASES; do
    if [ ! -d "$fix_perch/$id" ]; then echo "MISSING $id: no $fix_perch/$id"; rc=1; continue; fi
    if out=$(py compare "$T/out/$id" "$fix_perch/$id"); then
      echo "ok      $id"
    else
      echo "DIFFERS $id"
      sed 's/^/          /' <<<"$out"
      rc=1
    fi
    if ! diff -r "$fix_perch/$id" "$fix_web/$id" >/dev/null; then echo "DIFFERS $id: the webui copy is not perch's"; rc=1; fi
  done
  # what the kept fixtures were captured with, against what ran now
  local kept
  kept=$(sed -n 's/.*"version": "\(.*\)",/\1/p' "$fix_perch/meta.json" 2>/dev/null | head -n 1)
  [ "$kept" = "$EID_VERSION" ] || echo "note: the fixtures were captured with '$kept', this is '$EID_VERSION'"
  grep -q "\"p1\": \"$graph_sha_p1\"" "$fix_perch/meta.json" 2>/dev/null || echo "note: the P1 graph is not the one the fixtures were captured with ($graph_sha_p1)"
  return $rc
}

verify_static() {
  local d f n bad=0 shown
  for d in "$fix_perch" "$fix_web"; do
    shown=${d#"$root"/}
    if [ ! -d "$d" ]; then echo "BAD missing $shown"; bad=1; continue; fi
    for f in "$d"/*/journal.ndjson; do
      [ -e "$f" ] || continue
      n=$(grep -c ModelSpend "$f" || true)
      [ "$n" = 0 ] || { echo "BAD ${f#"$root"/}: grep -c ModelSpend is $n"; bad=1; }
    done
    echo "== $shown"
    py verify "$d" || bad=1
    for f in "$d"/*/case.json; do
      [ -e "$f" ] || continue
      n=$(basename "$(dirname "$f")")
      grep -q "^| $n |" "$d/README.md" 2>/dev/null || { echo "BAD $shown/README.md has no row for case $n"; bad=1; }
    done
  done
  diff -r -x README.md -x capture.sh "$fix_perch" "$fix_web" >/dev/null || { echo "BAD the two fixture folders differ"; bad=1; }
  return $bad
}

main() {
  if [ "$mode" = verify ]; then
    make_tree
    verify_static && echo "verify: ok"
    return
  fi
  EID=${EIDOLON_BIN:-$real_home/.local/bin/eidolon}
  [ -f "$EID" ] && [ -x "$EID" ] || die "no eidolon binary at $EID (it is a Linux program: run this in WSL, or set EIDOLON_BIN)"
  [ "$(head -c 2 "$EID")" != '#!' ] || die "$EID is a script (Minerva's bin/eidolon stub needs the real HOME); give the Linux binary"
  make_tree
  fetch_plugins
  setup_env
  EID_VERSION=$(eid --version)
  EID_SHA256=$(sha256sum "$EID" | cut -d' ' -f1)
  echo "eidolon: $EID_VERSION  (sha256 ${EID_SHA256:0:16}, $EID)"
  echo "plugins: $P1_REF = ${p1_sha:0:7}, $BASE_REF = ${base_sha:0:7}, from $PLUGINS_REPO"
  echo "graph:   P1 sha256:${graph_sha_p1:0:16}, before P1 sha256:${graph_sha_pre:0:16}; stub on 127.0.0.1:$STUB_PORT; tree $T"
  local id
  for id in $CASES; do
    declare -F "case_$id" >/dev/null || die "no case named $id"
    "case_$id"
  done
  scan_secrets
  case $mode in
    write) write_fixtures; verify_static ;;
    check) check_fixtures && echo "check: every case matches the kept fixtures" ;;
  esac
}

main
