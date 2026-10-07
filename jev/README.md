# jev -- the chooser service

`server.py` is the Python half of the `jev` eidolon plugin (FastAPI, WSL). It
answers two questions in one forward pass each, which llama.cpp cannot serve:

| method | model | what it does |
|---|---|---|
| `choose` | jevlike, a small PyTorch chooser | a context and N options in, one probability per option out |
| `entail` | openjev, Qwen3.5-4B as a 3-way NLI cross-encoder | a premise and hypotheses in, contradiction / entailment / neutral per hypothesis out |

That is all it serves. Graphs and automation live in the plugin
([`eidolon-plugins/jev`](../eidolon-plugins/jev/README.md)), which calls this
service through `jev_choose` and `jev_entail`. Both models load lazily on first
use, so the process starts instantly.

The operator starts it by hand on 127.0.0.1:8091 with the token in
`~/.config/eidolon/jev.token`; the plugin README's Install step 3 has the exact
command. For a manual smoke test, set the two required variables yourself:

```sh
EIDOLON_SERVICE_PORT=8091 EIDOLON_SERVICE_TOKEN=dev jev/.venv/bin/python jev/server.py
```

Setup: `python3 -m venv jev/.venv && jev/.venv/bin/pip install -r jev/requirements.txt`
(the installer uses the hash-pinned `requirements.torch.txt` and
`requirements.pinned.txt` instead). The openjev weights (~9 GB) are the one
download: `jev/get-openjev.sh`.

## Environment

All of it is read once, at import. A bad `OPENJEV_DTYPE`, or a missing port or
token, stops the service from starting; a missing `JEVLIKE_ROOT`, checkpoint or
`OPENJEV_DIR` is an error on the first call that needs it, naming the setting.

| variable | default | meaning |
|---|---|---|
| `EIDOLON_SERVICE_PORT` | none, required | port to bind on 127.0.0.1 |
| `EIDOLON_SERVICE_TOKEN` | none, required | bearer token `POST /call` checks |
| `JEVLIKE_ROOT` | `jev/` | where the vendored `jevlike` package is imported from |
| `JEVLIKE_CKPT` | `jev/runs/synthetic.pt` | the chooser checkpoint (`jev/runs/jev-base-v0.pt` is the other one shipped) |
| `OPENJEV_DIR` | `models/openjev` | the openjev weights |
| `OPENJEV_DTYPE` | `float32` | `float32` or `bfloat16`: the precision openjev loads in. The weights are stored as bfloat16, so float32 holds about twice the ~9 GB in memory; `bfloat16` loads them as stored, for a box that cannot spare it, with slower and lossier CPU matmul. Unset or empty means `float32`; anything else stops the service at start. |
| `JEV_DECISIONS_LOG` | `<cache dir>/eidolon/extensions/jev/decisions.jsonl` | the file every `choose` appends one row to, `{context, options, label, probs, ts}`. `off` (any case) turns the log off entirely: no row is built and no file is created. Unset or empty is the default path. |

`<cache dir>` is `$XDG_CACHE_HOME` (absolute) or `~/.cache` on Linux,
`~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows.

The decision log is the only file `server.py` itself writes.

## `GET /health`

Unauthenticated, never loads a model (a probe against a service that has not
loaded openjev yet must come back well inside two seconds):

```json
{
  "status": "ok",
  "jevlike": true,
  "openjev": true,
  "decisions_log": "/home/you/.cache/eidolon/extensions/jev/decisions.jsonl",
  "ckpt": "synthetic.pt",
  "loaded": {"jevlike": false, "openjev": false},
  "openjev_dtype": "float32"
}
```

| field | meaning |
|---|---|
| `status` | `"ok"` whenever the process can answer |
| `jevlike` | the checkpoint file at `JEVLIKE_CKPT` exists |
| `openjev` | a `*.safetensors` file exists in `OPENJEV_DIR` |
| `decisions_log` | `"off"` when `JEV_DECISIONS_LOG=off`, otherwise the path `choose` appends to |
| `ckpt` | basename of `JEVLIKE_CKPT`, present or not |
| `loaded` | `jevlike` / `openjev` are `true` only once that model is loaded in this process, that is, after a first `choose` / `entail` has paid for it |
| `openjev_dtype` | `"float32"` or `"bfloat16"`, from `OPENJEV_DTYPE` |

## Tests

`unittest`, from the repo root, under a throwaway HOME and XDG_CACHE_HOME so
nothing reaches the real `~/.cache`. `OPENJEV_DIR` must name an existing
directory (any empty one will do) in a checkout without `models/openjev`; the
weights themselves are faked unless `JEV_TEST_OPENJEV=1`.

```sh
T=$(mktemp -d) && mkdir -p "$T/home" "$T/cache" "$T/openjev" &&
  HOME="$T/home" XDG_CACHE_HOME="$T/cache" OPENJEV_DIR="$T/openjev" \
  jev/.venv/bin/python jev/test_server.py
```
