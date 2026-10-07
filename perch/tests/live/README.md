# tests/live/ — a throwaway perch for a person to look at

`cargo test` proves the routes; it cannot prove that the page boots against them. `fake-perch.sh` is the live half of a slice's check: the
real perch binary in front of the stand-in door (`src/bin/fake_eidolon.rs`), serving the real page from `webui/term`, in a temp tree of its own.
Nothing here is an installer, a demo or a way to run Minerva: it exists so that someone can open a URL, click, and read the console.

| File | What it is |
|---|---|
| `fake-perch.sh` | start, `--url` (a fresh login URL) and `--stop` (stop it and delete its temp tree) for one test perch on `PORT` |

```text
PORT=48102 perch/tests/live/fake-perch.sh          # prints http://127.0.0.1:48102/#code=<code>, and only that, on stdout
PORT=48102 perch/tests/live/fake-perch.sh --url    # another code (30 s, single use) for the running one
PORT=48102 perch/tests/live/fake-perch.sh --stop
```

- **`PORT` is required and has no default**; each slice names its own (the plan's port list), and **47820, the operator's perch, is refused
  outright**. A port that is already listening is refused too.
- **Nothing of the operator's is read or written.** `HOME`, `MINERVA_HOME` and `XDG_CONFIG/DATA/STATE/CACHE/RUNTIME` are all inside
  `$TMPDIR/minerva-fake-perch-<PORT>` (mode 0700; `FAKE_PERCH_BASE` moves it), and the perch runs under `env -i` with only those, `PATH`
  (no Windows entry), `LANG` and `RUST_LOG`. Its doors are the fake, whose model is `mock`; no provider credential exists in the tree, and no
  model call can be made. The member's `config.toml` is empty and its `policy.rn` is the harness's four-entry stub, so manual mode has a table to wrap.
- **The bearer token never leaves its 0600 file.** It goes to curl through `-H @file` (a 0600 header file made and removed per call), is read
  and written by shell builtins only, and is in no argv, no environment, no log line and no stdout. The one line on stdout is a URL with a
  login code, which the page trades for a cookie.
- **It needs two binaries built with `--features test-bins`**: the perch and `fake_eidolon`. Give `PERCH_BIN` and `FAKE_EIDOLON_BIN`, or set
  `CARGO_TARGET_DIR` (default `~/.cache/minerva-slices-target`) and the script builds them there; `CARGO_NET_OFFLINE=true` keeps cargo off the
  network. Run it in WSL.
- **Stop it when done.** `--stop` sends SIGTERM (a perch that ignores it for 10 s is killed), and removes the temp tree only if the tree carries
  this script's marker file.

A slice's own live check adds to the perch through two variables, so it never edits the script:

```text
FAKE_PERCH_ARGS="--jev-health http://127.0.0.1:48140/health --knob term_ring=4096"   more perch flags
FAKE_PERCH_ENV="FAKE_WORKFLOW_STEP_MS=500 FAKE_ASKS=/path/to/asks"                   more environment (the fake door inherits it)
```

Both are whitespace-separated words, so no word holds a space. The environment cannot reach the isolation: `HOME`, `MINERVA_HOME`, `PATH` and
every `XDG_` name are refused.
