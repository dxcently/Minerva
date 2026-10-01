"""jev extension service -- serves the choosers behind eidolon's `service_call`.

WHY THIS EXISTS: llama.cpp cannot serve these. They are not causal language
models, so there is no GGUF path for them at all:

  jevlike   a one-pass N-way chooser (PyTorch). Reads a context plus a list
            of options and returns one probability per option.
  openjev   Qwen3.5-4B fine-tuned as a 3-way NLI cross-encoder
            (Qwen3_5ForSequenceClassification). Entailment / contradiction /
            neutral over a premise and a hypothesis.

Both answer in ONE forward pass -- no token-by-token decode -- which is why
they are fast on this bandwidth-bound box even though it crawls on 27B dense
models.

THE CONTRACT: `GET /health` (process-up, not model-loaded -- see the comment
there) and `POST /call` with `{"method": "choose"|"entail", "args": {...}}`,
gated on a bearer token read from EIDOLON_SERVICE_TOKEN, on the port in
EIDOLON_SERVICE_PORT; neither has a default, and the process dies on import
without them. (This used to fake an OpenAI chat surface for Open WebUI; the
decision on record, docs/Decisions.md, "jevlike and openjev are tools, not chat
models", retired that.)

The eidolon side is the `jev` plugin (eidolon-plugins/jev; tools/choose.rn and
tools/entail.rn register jev_choose and jev_entail). It expects port 8091 and
the token file ~/.config/eidolon/jev.token; the operator starts this by hand
(see the plugin README's Install step 3). Graphs and automation live in that
plugin (Rune interpreter); the old Python automation.* methods are retired.
Manual smoke:

    EIDOLON_SERVICE_PORT=8091 EIDOLON_SERVICE_TOKEN=dev python jev/server.py
"""
from __future__ import annotations

import asyncio
import hmac
import json
import os
import sys
import threading
import time
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
import uvicorn

import _paths

# --- jevlike -----------------------------------------------------------------
# Vendored at jev/jevlike (the three modules imported below) with its 169 KB
# checkpoint at jev/runs/synthetic.pt, so a fresh clone chooses with nothing
# to download. It used to be imported from a sibling checkout on one dev box;
# a tool members run unmaintained is better served by a frozen copy than by
# a path that exists on one machine. JEVLIKE_ROOT still overrides it.
_JEV_DIR = Path(__file__).resolve().parent
JEVLIKE_ROOT = Path(os.getenv("JEVLIKE_ROOT", str(_JEV_DIR)))
JEVLIKE_CKPT = Path(os.getenv("JEVLIKE_CKPT", str(_JEV_DIR / "runs" / "synthetic.pt")))

# openjev's weights (~9 GB) are the one download: jev/get-openjev.sh puts them
# in <repo>/models/openjev. Its model code is vendored at jev/openjev, pinned
# to the revision those weights were checked against.
OPENJEV_DIR = Path(os.getenv("OPENJEV_DIR", str(_JEV_DIR.parent / "models" / "openjev")))
OPENJEV_CODE_DIR = _JEV_DIR / "openjev"

# --- the extension contract ---------------------------------------------------
# Minted by the extension host and handed to this process in the environment
# only -- never on the command line (a Linux /proc/<pid>/cmdline is
# world-readable; /proc/<pid>/environ is not), never logged, never echoed
# back in a response. Reading them with `os.environ[...]` rather than
# `.get(...)` is deliberate: a service that cannot get a real token has no
# business binding a port at all, so it should die on import, not serve
# unauthenticated.
PORT = int(os.environ["EIDOLON_SERVICE_PORT"])
TOKEN = os.environ["EIDOLON_SERVICE_TOKEN"]

app = FastAPI(title="jev extension service")

_loaded: dict[str, object] = {}

# One lock per model, held across the lazy load AND every forward pass --
# not just the load. Plain `nn.Module.forward` under `torch.no_grad()` is
# ordinarily fine to call from several threads at once (no dropout, no
# batchnorm running stats in either model below), so this is belt-and-braces
# for jevlike -- but `_openjev`'s tokenizer is a different story; see its own
# docstring. `asyncio.to_thread` (in `/call` below) dispatches `choose` and
# `entail` onto real OS threads, so a plain `threading.Lock` -- blocking the
# *worker* thread it runs on, never the event loop `/call` already keeps free
# for `/health` -- is the right tool, not `asyncio.Lock`.
_jevlike_lock = threading.Lock()
_openjev_lock = threading.Lock()


def _jevlike():
    """Load the chooser once, on first use -- not at import, so the server
    starts instantly and a missing checkpoint is a per-request error rather
    than a boot failure. Returns a `run` closure that scores one call at a
    time; see `_jevlike_lock` above for why."""
    if "jevlike" in _loaded:
        return _loaded["jevlike"]
    with _jevlike_lock:
        if "jevlike" in _loaded:  # lost the race to load; use what won it
            return _loaded["jevlike"]
        if not JEVLIKE_ROOT.is_dir():
            # Without this check, `from jevlike...` a few lines down fails
            # with a bare `ModuleNotFoundError: No module named 'jevlike'`
            # that names neither this setting nor what it was looking for.
            # The extension host does not hand this process a JEVLIKE_ROOT
            # -- only EIDOLON_SERVICE_TOKEN/PORT and EIDOLON_EXTENSION_
            # DIR/NAME (crates/rune/src/ext.rs's `service_env_names`) -- so
            # the default above is this one dev box's sibling checkout and
            # nothing else's; anywhere else this needs the environment
            # variable set by hand. Same failure shape D9 already requires
            # for the venv interpreter (docs/Decisions.md, "an extension
            # cannot name its interpreter portably"): name the setting at
            # the point of use instead of surfacing an import error from
            # deep inside jevlike.
            raise RuntimeError(
                f"JEVLIKE_ROOT is not a directory: {JEVLIKE_ROOT} -- set the "
                f"JEVLIKE_ROOT environment variable to a jevlike checkout"
            )
        if str(JEVLIKE_ROOT) not in sys.path:
            sys.path.insert(0, str(JEVLIKE_ROOT))
        import torch
        from jevlike.data import ChoiceExample
        from jevlike.model import load_checkpoint, select_device
        from jevlike.train import move

        device = select_device("cpu")
        model, collator, _ = load_checkpoint(str(JEVLIKE_CKPT), device)
        model.eval()

        def run(context: str, options: list[str]) -> list[float]:
            with _jevlike_lock:
                batch = move(collator([ChoiceExample(context, tuple(options), 0)]), device)
                with torch.no_grad():
                    return model(batch).softmax(-1)[0, : len(options)].cpu().tolist()

        _loaded["jevlike"] = run
        return run


def _openjev():
    """Qwen3.5-4B as a 3-way NLI cross-encoder. ~9 GB, CPU, one pass.

    Loaded through the repo's OWN `OpenJevCrossEncoder`, not bare
    AutoModelForSequenceClassification. That distinction is the whole ball
    game: this head was trained on ONE string built from the template in
    `config.nli_template` --

        Premise: {premise}
Hypothesis: {hypothesis}

    -- whereas `tok(premise, hypothesis)` produces the tokenizer's *pair*
    encoding, a different input distribution. The model answers either way,
    which is exactly the danger: the first wiring here scored "the service is
    healthy" as 0.72 neutral against a premise of "the server returned 500 on
    every request", plausible-looking and meaningless. The vendor class also
    right-pads and pools the last non-pad token, matching how the head was
    fit. Use it rather than re-deriving it.

    Serialised the same way as jevlike (`_openjev_lock`, held across the load
    and every `run`), and here it is not just insurance: `ce.predict` tokenizes
    through a HuggingFace *fast* tokenizer, and `PreTrainedTokenizerFast`
    mutates shared state on the call -- `set_truncation_and_padding` compares
    the backend tokenizer's current truncation/padding config to the one this
    call wants and calls `enable_truncation`/`enable_padding` when they
    differ (checked directly against the installed transformers==5.17.0:
    `tokenization_utils_tokenizers.py`). `_encode` above always asks for the same
    config, so after the first call there is nothing left to mutate -- but the
    first call, or two calls racing before either has run once, both see
    "differs" and both write. They would write the same value, so this may
    never produce a wrong tokenization on this version; it is exactly the
    kind of vendor-internal behavior this file already got bitten trusting
    once (the incident above), so it is not spent trusting a second time.
    """
    if "openjev" in _loaded:
        return _loaded["openjev"]
    with _openjev_lock:
        if "openjev" in _loaded:
            return _loaded["openjev"]
        if not OPENJEV_DIR.is_dir():
            # Same reasoning as `_jevlike`'s `JEVLIKE_ROOT` check above: name
            # the setting here, at the point of use, rather than letting
            # `from modeling_openjev...` below fail with a bare
            # `ModuleNotFoundError` naming neither it nor what it was
            # looking for. The extension host does not set OPENJEV_DIR
            # either.
            raise RuntimeError(
                f"OPENJEV_DIR is not a directory: {OPENJEV_DIR} -- set the "
                f"OPENJEV_DIR environment variable to the openjev weights "
                f"(jev/get-openjev.sh downloads them)"
            )
        import torch

        if str(OPENJEV_CODE_DIR) not in sys.path:
            sys.path.insert(0, str(OPENJEV_CODE_DIR))
        from modeling_openjev import OpenJevCrossEncoder

        # float32 on CPU: bf16 is the repo default for GPU, and CPU bf16
        # matmul is both slower and lossier here.
        ce = OpenJevCrossEncoder(str(OPENJEV_DIR), device="cpu", dtype=torch.float32, bs=8)
        labels = ["contradiction", "entailment", "neutral"]  # CON, ENT, NEU -- the repo's order

        def run(premise: str, hypotheses: list[str]) -> list[dict]:
            with _openjev_lock:
                probs = ce.predict([(premise, h) for h in hypotheses])
                return [{labels[i]: float(row[i]) for i in range(len(labels))} for row in probs]

        _loaded["openjev"] = run
        return run


# --- the decision log ---------------------------------------------------------
# One JSONL row per `choose` call: {context, options, label, probs, ts}. This
# *is* jevlike's training data -- `runs/synthetic.pt` is 169 KB and trained on
# synthetic examples only, and stays a toy until it is retrained on rows
# shaped like the decisions it is actually asked to make (docs/Decisions.md,
# "Automation is a statechart, and jev is its learned transition function").
#
# The path is chosen deliberately: outside every repo, because this file
# grows without bound for as long as the extension is enabled and is not
# something to check in or to have `git clean` eat; and beside jev's own
# machine-scoped state (`<cache dir>/eidolon/extensions/jev.json`, the
# port/token/pid record the loader overwrites on every start -- see
# crates/rune/src/ext.rs's `Record`/`record_dir()`) rather than inside it, so
# restarting the service, which rewrites that record, never touches this one.
# It survives an `eidolon ext stop jev` / `start jev` cycle, a machine
# reboot, and the extension being disabled and re-enabled -- anything short
# of deleting it by hand.
def _cache_dir() -> Path:
    """Mirrors `record_dir()`'s own `dirs::cache_dir()`
    (crates/rune/src/ext.rs), platform by platform, so this log and jev's
    machine-scoped ext record stay siblings everywhere this runs -- not only
    on Windows, where both happen to land under `%LOCALAPPDATA%`.

    Delegates to `_paths.cache_dir()` -- see that module's docstring for the
    full reasoning (the `XDG_CACHE_HOME` absoluteness check, the
    `PurePosixPath`-under-`WindowsPath` trap `CacheDirTests` below exercises,
    and the one fallback (`record_dir()`'s `unwrap_or_else(temp_dir)`) that
    neither this function nor `_paths.cache_dir()` replicates. Kept as a
    thin wrapper, rather than inlined as `_paths.cache_dir` directly, only
    so `server._cache_dir()` stays a stable name for this module's own
    tests and any future caller in this file.
    """
    return _paths.cache_dir()


# `os.environ.get(...) or <fallback>`, the same short-circuiting pattern
# `_cache_dir()` itself uses above for `LOCALAPPDATA` and `XDG_CACHE_HOME` --
# not `os.getenv("JEV_DECISIONS_LOG", <fallback>)`. Python evaluates a
# call's arguments, default included, before the call itself runs, so the
# `os.getenv` form ran `_cache_dir()` on every import even when
# `JEV_DECISIONS_LOG` was set and its result was simply discarded. That is
# not only wasted work: on a POSIX box where `$HOME` is unset and the
# running UID has no `/etc/passwd` entry (a known arbitrary-UID container
# gotcha), `Path.home()` -- called from inside `_cache_dir()` -- raises
# `RuntimeError("Could not determine home directory.")`, which took the
# whole import down. That fired even when the operator did exactly the
# right thing and pointed `JEV_DECISIONS_LOG` somewhere disposable (see
# test_server.py's own comment on why its default does exactly that) --
# `or` below means `_cache_dir()` now runs only when its result would
# actually be used.
DECISIONS_LOG = Path(
    os.environ.get("JEV_DECISIONS_LOG")
    or (_cache_dir() / "eidolon" / "extensions" / "jev" / "decisions.jsonl")
)


# `_O_BINARY`/`_lock_file`/`_unlock_file` used to be defined here, an
# import-time platform branch (`msvcrt` on Windows, `fcntl` on POSIX) that
# the retired `automation/decisions.py` log also needed. Rather than
# a second copy of the same platform branch, both modules share one
# implementation in `_paths.py` -- see its docstring for the full reasoning
# (in particular the Windows measurement behind why this is load-bearing:
# six processes appending 200 rows each with the lock stubbed out lost 37%
# of rows to `O_APPEND`'s reposition-then-write race). Aliased here under
# their old names so nothing below has to change.
_O_BINARY = _paths.O_BINARY
_lock_file = _paths.lock_file
_unlock_file = _paths.unlock_file


# Guards every append below against this one process's *other threads* --
# `_lock_file`/`_unlock_file` above are the cross-process half, which a
# `threading.Lock` cannot reach at all.
_decisions_log_lock = threading.Lock()


def _log_decision(context: str, options: list[str], label: int, probs: dict[str, float]) -> None:
    """Best-effort: a full disk or a locked file should not turn a working
    `choose` call into a failed one -- the caller already has its answer by
    the time this runs, and the log is a record of that answer, not a
    precondition for giving it.

    `label` is the option's *index* into `options`, not its text --
    jevlike's own loader (`jevlike.data.validate`) raises "label must be an
    option index" on anything else, so a text label would mean every row
    logged here trains nothing. Matches the native shape
    `jevlike.data.build_wikispeedia` writes (`"label": menu.index(click)`)
    and the row docs/design/automation.md ss5 specifies for this log.
    """
    row = {"context": context, "options": options, "label": label, "probs": probs, "ts": time.time()}
    line = (json.dumps(row) + "\n").encode("utf-8")
    try:
        with _decisions_log_lock:
            # `_paths.append_locked` is `_lock_file`/`_unlock_file` around
            # `os.open`/`os.write` on a raw fd, not `Path.open("a")` --
            # `msvcrt.locking`/`fcntl.flock` both need a raw fd, not a file
            # object, and `_O_BINARY` matters so the byte count `os.write`
            # returns is the byte count that actually lands on disk; a
            # text-mode `TextIOWrapper` on Windows silently expands every
            # "\n" to "\r\n", which would make that return value a lie. See
            # `_paths.append_locked`'s docstring for why the lock itself is
            # needed at all on Windows, and `_decisions_log_lock` above for
            # the in-process half it does not cover.
            _paths.append_locked(DECISIONS_LOG, line)
    except OSError:
        pass  # the answer already went to the caller; losing the row is not losing the call


# --- the two methods -----------------------------------------------------------
def _choose(args: dict) -> dict:
    """context + options -> one probability per option, plus the top one by
    name so a caller that only wants an answer does not have to compute its
    own argmax over `probs`."""
    context = args.get("context") or ""
    options = [o for o in (args.get("options") or []) if isinstance(o, str) and o.strip()]
    if len(options) < 2:
        raise ValueError("choose needs a context and at least two non-empty options")
    if len(set(options)) != len(options):
        # `scored` below is a dict keyed by option text, so two identical
        # options collapse to one key -- silently dropping a probability
        # (the returned `probs` would no longer sum to 1) rather than
        # raising. Caught here instead: a clear reason beats a quietly
        # short-counted answer.
        raise ValueError("choose needs distinct options; two options were identical")
    probs = _jevlike()(context, options)
    scored = {opt: float(p) for opt, p in zip(options, probs)}
    label = max(scored, key=scored.get)
    _log_decision(context, options, options.index(label), scored)
    return {"probs": scored, "best": label}


def _entail(args: dict) -> dict:
    """premise + hypotheses -> contradiction/entailment/neutral per
    hypothesis, each with its own top label -- entailment is not logged for
    training the way `choose` is: it is a fact-check, not a decision with a
    single supervised label."""
    premise = (args.get("premise") or "").strip()
    hypotheses = [h for h in (args.get("hypotheses") or []) if isinstance(h, str) and h.strip()]
    if not premise or not hypotheses:
        raise ValueError("entail needs a premise and at least one hypothesis")
    scored = _openjev()(premise, hypotheses)
    return {
        "results": [
            {"hypothesis": h, "label": max(s, key=s.get), "scores": s}
            for h, s in zip(hypotheses, scored)
        ]
    }


METHODS = {
    "choose": _choose,
    "entail": _entail,
}


# --- the endpoints ----------------------------------------------------------
@app.get("/health")
def health():
    """2xx means only "this process can answer", never "the models are
    loaded". jevlike and openjev load lazily on first `choose`/`entail` (see
    _jevlike/_openjev above) specifically so the process starts instantly --
    if health forced that load it would defeat the whole point twice over:
    the extension host's own adoption probe, run once per session against a
    service already recorded as running, gets a 2 second budget
    (`HEALTH_TIMEOUT` in crates/tools/src/service.rs) that openjev's ~9 GB
    load blows through by more than an order of magnitude (a real cold load
    measured here: ~24s), so every session after the first would pay the
    load it was trying to skip, and fail its probe doing it.

    The two booleans below cost a stat and a glob, not a load, so they ride
    along for free -- and the openjev check looks for the weights file
    specifically, not just the directory: a 9 GB download still in flight
    leaves config.json sitting there, and reporting true on that basis would
    be a lie the first `entail` call immediately contradicts.
    """
    return {
        "status": "ok",
        "jevlike": JEVLIKE_CKPT.exists(),
        "openjev": any(OPENJEV_DIR.glob("*.safetensors")),
    }


@app.post("/call")
async def call(request: Request):
    # Checked before anything else runs, including reading the body: an
    # unauthenticated call must not touch a model or the decision log.
    # Headers are already off the wire by the time a handler is invoked, so
    # this costs nothing and happens strictly before `request.json()` below
    # -- the same ordering as the minimal reference service in
    # eidolon/docs/extensions.md.
    # `hmac.compare_digest`, not `!=`: a plain string compare short-circuits
    # on the first differing byte, which leaks -- through how long the
    # comparison took -- how much of a guessed token was right. Comparing
    # `bytes` rather than `str`: `compare_digest` restricts `str` input to
    # ASCII and raises `TypeError` on anything else, which turned a
    # non-ASCII guess -- one stray high byte in the header is enough -- into
    # an unhandled 500 instead of a 401, since this check sits outside the
    # try/except below that only wraps JSON parsing. Starlette decodes every
    # header value with `latin-1` (`Headers.get`, starlette/datastructures.py),
    # a decode that is total over byte values 0-255 and never fails, so
    # `.encode("latin-1")` below is its exact inverse: it always succeeds and
    # recovers the literal bytes the client sent, for `supplied` and for the
    # fixed, ASCII `TOKEN` alike -- confirmed directly (non-ASCII `str` vs
    # `str` raises `TypeError: comparing strings with non-ASCII characters is
    # not supported`; the `bytes` form of the same inputs does not).
    # Comparing digests rather than text is also what `compare_digest` is
    # actually for, so this is not a workaround, and the constant-time
    # property is unaffected -- still no secret leaks either way. `or ""`
    # keeps `supplied` defined when the header is absent; `compare_digest(
    # None, ...)` raises instead of just failing closed.
    supplied = (request.headers.get("authorization") or "").encode("latin-1")
    expected = f"Bearer {TOKEN}".encode("latin-1")
    if not hmac.compare_digest(supplied, expected):
        return JSONResponse({"ok": False, "error": "bad token"}, status_code=401)
    try:
        body = await request.json()
        if not isinstance(body, dict):
            raise ValueError("the body must be a JSON object")
    except Exception:
        return JSONResponse({"ok": False, "error": "invalid JSON body"}, status_code=400)
    method = body.get("method")
    args = body.get("args") or {}
    fn = METHODS.get(method)
    if fn is None:
        return {"ok": False, "error": f"unknown method {method!r}"}
    try:
        # jevlike is instant but openjev is a real forward pass -- seconds on
        # this box once loaded, longer still on the first call that loads it
        # -- and this handler is `async def`, so running it inline would
        # stall the event loop for every other request, /health included.
        # `asyncio.to_thread` is the plain-stdlib way off it.
        result = await asyncio.to_thread(fn, args)
    except Exception as e:  # a chooser that fails should say why, not 500 silently
        return {"ok": False, "error": f"{type(e).__name__}: {e}"}
    return {"ok": True, "result": result}


if __name__ == "__main__":
    print(
        f"jev extension service on http://127.0.0.1:{PORT}"
        f"  (jevlike {'[#]' if JEVLIKE_CKPT.exists() else '[ ]'}"
        f", openjev {'[#]' if any(OPENJEV_DIR.glob('*.safetensors')) else '[ ]'})"
    )
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")
