"""Tests for jev/server.py -- the eidolon extension service.

Run with the jevlike venv's python (it already has fastapi, httpx and the
jevlike package this service imports lazily -- nothing extra to install):

    C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\test_server.py

`unittest`, not pytest: that venv is jevlike's own checkout, not this repo's,
and pytest is not in it -- installing a test runner into someone else's venv
to test this file is a heavier ask than the stdlib already sitting there.

`choose` runs against the real 169 KB jevlike checkpoint in every run here --
it is fast enough on CPU that faking it would not save anything and would
stop testing the thing that actually matters (see docs/Architecture.md,
"Instant on CPU"). `entail` runs against the real ~9 GB openjev weights only
when JEV_TEST_OPENJEV=1 is set, so the rest of the suite stays fast by
default; that one test asserts the exact regression this service exists to
avoid -- see its docstring.
"""
from __future__ import annotations

import hmac
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

# server.py reads its port and token at import time -- see its own comment on
# why -- so both must be set before the only `import server` below.
os.environ.setdefault("EIDOLON_SERVICE_PORT", "8090")
# Forced, not `setdefault`: server.py's own docstring tells a reader to set
# this by hand for its manual smoke test (`EIDOLON_SERVICE_TOKEN=dev python
# jev/server.py`), and PowerShell's `$env:EIDOLON_SERVICE_TOKEN = '...'`
# outlives that one command -- it persists for the rest of the session. A
# developer who runs that smoke test and then this suite in the same window
# would otherwise silently test against whatever they typed, not
# "test-token", because `setdefault` leaves an already-set variable alone.
# Unlike `JEV_DECISIONS_LOG` below, there is no legitimate reason for this
# suite to authenticate with any token other than its own, so this process's
# ambient environment does not get a vote -- the identical hygiene gap
# `JEV_DECISIONS_LOG` closes by pointing its *default* somewhere disposable,
# closed here by not reading the ambient value at all.
os.environ["EIDOLON_SERVICE_TOKEN"] = "test-token"
# Most tests below call `choose` over HTTP without caring about the decision
# log, so they do not patch `DECISIONS_LOG` the way the two tests that do
# care (`test_logs_one_row_shaped_for_training`,
# `test_does_not_write_to_the_decision_log`) patch it per-test. Without a
# default override here every one of those calls appends a row to the real,
# production `decisions.jsonl` -- confirmed the hard way: running this suite
# during the extension-conversion review left dozens of synthetic rows
# (`"concurrent probe 3"`, `"pick one"`, …) in the real log. Point the
# default somewhere disposable instead; a test that wants the real path can
# still read `os.environ["JEV_DECISIONS_LOG"]` or patch `server.DECISIONS_LOG`
# explicitly.
os.environ.setdefault("JEV_DECISIONS_LOG", str(Path(tempfile.mkdtemp()) / "decisions.jsonl"))

import server  # noqa: E402  (must follow the environ mutations above)
from fastapi.testclient import TestClient  # noqa: E402

TOKEN = os.environ["EIDOLON_SERVICE_TOKEN"]
AUTH = {"authorization": f"Bearer {TOKEN}"}


class HealthTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)

    def test_health_is_2xx_and_says_what_is_on_disk(self):
        r = self.client.get("/health")
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertEqual(body["status"], "ok")
        self.assertIn("jevlike", body)
        self.assertIn("openjev", body)

    def test_health_never_loads_a_model(self):
        # The whole point of lazy loading: a health probe must not be the
        # thing that pays for it. `_loaded` is server.py's own cache dict,
        # populated only inside `_jevlike()`/`_openjev()`.
        server._loaded.clear()
        self.client.get("/health")
        self.assertEqual(server._loaded, {})


class CacheDirTests(unittest.TestCase):
    """`_cache_dir()` mirrors the Rust side's `dirs::cache_dir()`
    (crates/rune/src/ext.rs's `record_dir()`) platform by platform, so
    `DECISIONS_LOG`'s default stays a sibling of jev's machine-scoped ext
    record everywhere, not only on Windows. Pure branching on `sys.platform`
    plus one environment variable per branch, so every branch is provably
    covered here even though this suite only runs on Windows -- what is NOT
    proven this way is genuine POSIX path semantics (`pathlib.Path` on this
    interpreter is `WindowsPath` regardless of which `sys.platform` string
    the branch below reads), only that each platform routes to the lookup
    it should.
    """

    def test_windows_uses_localappdata(self):
        with mock.patch.object(server.sys, "platform", "win32"), mock.patch.dict(
            os.environ, {"LOCALAPPDATA": "C:\\fake\\local"}
        ):
            self.assertEqual(server._cache_dir(), Path("C:\\fake\\local"))

    def test_windows_falls_back_to_home_without_localappdata(self):
        with mock.patch.object(server.sys, "platform", "win32"), mock.patch.dict(
            os.environ, {"LOCALAPPDATA": ""}
        ):
            self.assertEqual(server._cache_dir(), Path.home())

    def test_macos_uses_library_caches(self):
        with mock.patch.object(server.sys, "platform", "darwin"):
            self.assertEqual(server._cache_dir(), Path.home() / "Library" / "Caches")

    def test_linux_prefers_xdg_cache_home(self):
        with mock.patch.object(server.sys, "platform", "linux"), mock.patch.dict(
            os.environ, {"XDG_CACHE_HOME": "/fake/cache"}
        ):
            self.assertEqual(server._cache_dir(), Path("/fake/cache"))

    def test_linux_falls_back_to_dot_cache_without_xdg(self):
        with mock.patch.object(server.sys, "platform", "linux"), mock.patch.dict(
            os.environ, {"XDG_CACHE_HOME": ""}
        ):
            self.assertEqual(server._cache_dir(), Path.home() / ".cache")

    def test_linux_ignores_a_relative_xdg_cache_home(self):
        # `dirs-sys`'s `is_absolute_path` (pinned `dirs` 6.0.0 / `dirs-sys`
        # 0.5.0 -- see `_cache_dir()`'s own docstring) rejects a
        # set-but-relative `XDG_CACHE_HOME` and falls back to `~/.cache`,
        # per the XDG spec. A bare `or` (truthiness only -- the bug this
        # closes) would have accepted "relative/cache" as-is instead; this
        # value is deliberately non-empty so it passes a truthiness check
        # and only fails an actual `is_absolute()` one.
        with mock.patch.object(server.sys, "platform", "linux"), mock.patch.dict(
            os.environ, {"XDG_CACHE_HOME": "relative/cache"}
        ):
            self.assertEqual(server._cache_dir(), Path.home() / ".cache")


class DecisionsLogImportTests(unittest.TestCase):
    """`DECISIONS_LOG`'s default used to run `_cache_dir()` unconditionally
    at import -- Python evaluates a call's arguments, defaults included,
    before the call itself runs, so `os.getenv("JEV_DECISIONS_LOG",
    _cache_dir() / ...)` called `_cache_dir()` even when `JEV_DECISIONS_LOG`
    was already set and its result was thrown away. On a POSIX box with no
    resolvable `$HOME` (no `$HOME` set and no `/etc/passwd` entry for the
    running UID -- a known arbitrary-UID container gotcha), `_cache_dir()`
    reaches `Path.home()`, which raises `RuntimeError`, taking the whole
    import down -- even when the operator had already pointed
    `JEV_DECISIONS_LOG` somewhere disposable, exactly as this suite's own
    module-level `os.environ.setdefault("JEV_DECISIONS_LOG", ...)` above
    does.

    This cannot be proven by reloading `server` in this process: reload
    re-runs the module body in the SAME namespace the rest of this suite
    already imported and is relying on (a fresh `app`, fresh locks, a
    cleared `_loaded` cache), which would leak into every test that runs
    after it. A subprocess that imports `server` fresh, with
    `JEV_DECISIONS_LOG` set and `Path.home()` forced to fail, proves the
    same thing without that risk -- it is the only way to observe genuine
    import-time behavior in isolation.
    """

    def test_import_survives_an_unresolvable_home_when_the_env_var_is_set(self):
        jev_dir = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "decisions.jsonl"
            code = f"""
import sys
from pathlib import Path
from unittest import mock

sys.path.insert(0, {jev_dir!r})
with mock.patch.object(
    Path, "home", side_effect=RuntimeError("Could not determine home directory.")
):
    import server  # DECISIONS_LOG's default must not reach Path.home() here

print("IMPORT_OK", server.DECISIONS_LOG)
"""
            env = dict(os.environ)
            env["JEV_DECISIONS_LOG"] = str(log_path)
            r = subprocess.run(
                [sys.executable, "-c", code],
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
            )
        self.assertEqual(
            r.returncode,
            0,
            "import crashed even though JEV_DECISIONS_LOG was set -- "
            f"_cache_dir() ran anyway; stdout={r.stdout!r} stderr={r.stderr!r}",
        )
        self.assertIn("IMPORT_OK", r.stdout)
        self.assertIn(str(log_path), r.stdout)


class MissingRootTests(unittest.TestCase):
    """The extension host does not set JEVLIKE_ROOT/OPENJEV_DIR for the
    child process (only EIDOLON_SERVICE_TOKEN/PORT and EIDOLON_EXTENSION_
    DIR/NAME), so anywhere but the box these default to, this is the actual
    first-run experience -- it must name the setting and what it was
    looking for, not surface a bare `ModuleNotFoundError` from inside
    jevlike/openjev."""

    def setUp(self):
        self.client = TestClient(server.app)

    def test_jevlike_root_missing_names_the_setting(self):
        missing = Path(tempfile.mkdtemp()) / "no-jevlike-here"
        with mock.patch.object(server, "_loaded", {}), mock.patch.object(
            server, "JEVLIKE_ROOT", missing
        ):
            with self.assertRaises(RuntimeError) as ctx:
                server._jevlike()
        self.assertIn("JEVLIKE_ROOT", str(ctx.exception))
        self.assertIn(str(missing), str(ctx.exception))

    def test_openjev_dir_missing_names_the_setting(self):
        missing = Path(tempfile.mkdtemp()) / "no-openjev-here"
        with mock.patch.object(server, "_loaded", {}), mock.patch.object(
            server, "OPENJEV_DIR", missing
        ):
            with self.assertRaises(RuntimeError) as ctx:
                server._openjev()
        self.assertIn("OPENJEV_DIR", str(ctx.exception))
        self.assertIn(str(missing), str(ctx.exception))

    def test_missing_root_is_a_clean_ok_false_over_http_not_a_500(self):
        # Proves the propagation path, not just the exception in isolation:
        # `_choose` runs inside `asyncio.to_thread`, and `/call`'s `except
        # Exception` below turns this into `{"ok": false, "error": "..."}"`,
        # same as any other chooser failure -- never a bare 500.
        missing = Path(tempfile.mkdtemp()) / "no-jevlike-here"
        with mock.patch.object(server, "_loaded", {}), mock.patch.object(
            server, "JEVLIKE_ROOT", missing
        ):
            r = self.client.post(
                "/call",
                headers=AUTH,
                json={"method": "choose", "args": {"context": "x", "options": ["a", "b"]}},
            )
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("JEVLIKE_ROOT", body["error"])


class AuthTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)

    def test_missing_token_is_401_and_runs_nothing(self):
        spy = mock.Mock()
        with mock.patch.dict(server.METHODS, {"choose": spy}):
            r = self.client.post("/call", json={"method": "choose", "args": {}})
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_wrong_token_is_401_and_runs_nothing(self):
        spy = mock.Mock()
        with mock.patch.dict(server.METHODS, {"choose": spy}):
            r = self.client.post(
                "/call",
                json={"method": "choose", "args": {}},
                headers={"authorization": "Bearer not-the-token"},
            )
        self.assertEqual(r.status_code, 401)
        spy.assert_not_called()

    def test_non_ascii_wrong_token_is_401_and_runs_nothing(self):
        # `hmac.compare_digest` rejects a non-ASCII `str` outright
        # (`TypeError: comparing strings with non-ASCII characters is not
        # supported`) -- pins the actual failure mode this closes: a guessed
        # token with a stray high byte must be a clean 401 like any other
        # wrong guess, not a 500. httpx itself refuses to encode a non-ASCII
        # `str` header (`UnicodeEncodeError`, client-side, before anything
        # reaches the server), so the value goes in as `bytes` here -- the
        # same latin-1 bytes a real client puts on the wire either way.
        spy = mock.Mock()
        with mock.patch.dict(server.METHODS, {"choose": spy}):
            r = self.client.post(
                "/call",
                json={"method": "choose", "args": {}},
                headers={"authorization": "Bearer café-guess-xyz".encode("latin-1")},
            )
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_raw_high_byte_token_is_401_and_runs_nothing(self):
        # A byte that was never meant to decode as text at all, not just
        # "non-ASCII text" -- the harsher half of the same regression.
        spy = mock.Mock()
        with mock.patch.dict(server.METHODS, {"choose": spy}):
            r = self.client.post(
                "/call",
                json={"method": "choose", "args": {}},
                headers={"authorization": b"Bearer \xff"},
            )
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_invalid_json_body_is_a_clean_400_not_a_500(self):
        r = self.client.post(
            "/call",
            headers={**AUTH, "content-type": "application/json"},
            content=b"{not json",
        )
        self.assertEqual(r.status_code, 400)
        self.assertFalse(r.json()["ok"])

    def test_unknown_method_is_an_ok_false_not_a_500(self):
        r = self.client.post("/call", headers=AUTH, json={"method": "nope", "args": {}})
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("nope", body["error"])

    def test_a_json_body_that_is_not_an_object_is_a_clean_400(self):
        # Valid JSON, but `.get("method")` on a list or a bare number is an
        # AttributeError, not a missing method -- caught explicitly so a
        # malformed call is a clean error and not a bare 500.
        for payload in ([1, 2, 3], "just a string", 42, None):
            with self.subTest(payload=payload):
                r = self.client.post("/call", headers=AUTH, json=payload)
                self.assertEqual(r.status_code, 400)
                self.assertFalse(r.json()["ok"])

    def test_the_token_check_goes_through_compare_digest(self):
        # A plain `!=` on the header short-circuits on the first differing
        # byte -- a timing side channel on a bearer token. This does not
        # measure timing (too noisy to be a reliable test); it pins the
        # *code path* so a future edit back to `!=` fails a test instead of
        # only a security review.
        real = hmac.compare_digest
        calls = []

        def spy(a, b):
            calls.append((a, b))
            return real(a, b)

        with mock.patch("server.hmac.compare_digest", spy):
            self.client.post("/call", headers=AUTH, json={"method": "nope", "args": {}})
        self.assertEqual(len(calls), 1)


class ChooseTests(unittest.TestCase):
    """Against the real jevlike checkpoint -- 169 KB, CPU, fast enough that
    there is no point faking it."""

    def setUp(self):
        self.client = TestClient(server.app)

    def test_needs_two_options_and_does_not_touch_the_model(self):
        fake = mock.Mock()
        with mock.patch.object(server, "_jevlike", fake):
            r = self.client.post(
                "/call",
                headers=AUTH,
                json={"method": "choose", "args": {"context": "x", "options": ["only-one"]}},
            )
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("two", body["error"])
        fake.assert_not_called()

    def test_rejects_duplicate_options_and_does_not_touch_the_model(self):
        # `scored` in `_choose` is a dict keyed by option text; two identical
        # options would collapse to one key, silently dropping a probability
        # rather than raising. Caught before the model runs.
        fake = mock.Mock()
        with mock.patch.object(server, "_jevlike", fake):
            r = self.client.post(
                "/call",
                headers=AUTH,
                json={"method": "choose", "args": {"context": "x", "options": ["a", "a", "b"]}},
            )
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("identical", body["error"])
        fake.assert_not_called()

    def test_scores_every_option_and_names_the_best(self):
        r = self.client.post(
            "/call",
            headers=AUTH,
            json={
                "method": "choose",
                "args": {
                    "context": "Which queue handles this? The customer wants their money back.",
                    "options": ["refund", "sales", "technical support"],
                },
            },
        )
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertTrue(body["ok"], body)
        probs = body["result"]["probs"]
        self.assertEqual(set(probs), {"refund", "sales", "technical support"})
        self.assertAlmostEqual(sum(probs.values()), 1.0, places=3)
        self.assertEqual(body["result"]["best"], max(probs, key=probs.get))

    def test_logs_one_row_shaped_for_training(self):
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "nested" / "decisions.jsonl"
            with mock.patch.object(server, "DECISIONS_LOG", log_path):
                r = self.client.post(
                    "/call",
                    headers=AUTH,
                    json={"method": "choose", "args": {"context": "c", "options": ["a", "b"]}},
                )
            self.assertTrue(r.json()["ok"])
            lines = log_path.read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 1)
            row = json.loads(lines[0])
            self.assertEqual(set(row), {"context", "options", "label", "probs", "ts"})
            self.assertEqual(row["context"], "c")
            self.assertEqual(row["options"], ["a", "b"])
            # The fix: `label` is the option's index, not its text -- anything
            # else and jevlike's own loader refuses the row (Decisions.md,
            # "label must be an option index"). Proved against that loader
            # directly, not a reimplementation of its rule.
            self.assertIsInstance(row["label"], int)
            self.assertIn(row["label"], (0, 1))
            if str(server.JEVLIKE_ROOT) not in sys.path:
                sys.path.insert(0, str(server.JEVLIKE_ROOT))
            from jevlike.data import validate as jevlike_validate

            example = jevlike_validate(row)  # raises on the old (text) shape
            self.assertEqual(example.options[example.label], r.json()["result"]["best"])

    def test_a_row_with_the_old_text_label_is_the_shape_validate_refuses(self):
        # Pins the regression this fix closes: the exact row shape B1 wrote
        # before (label as the option's text) is rejected by jevlike's own
        # loader, which is *why* it needed fixing.
        if str(server.JEVLIKE_ROOT) not in sys.path:
            sys.path.insert(0, str(server.JEVLIKE_ROOT))
        from jevlike.data import validate as jevlike_validate

        bad_row = {"context": "c", "options": ["a", "b"], "label": "b"}
        with self.assertRaises(ValueError):
            jevlike_validate(bad_row)


class EntailTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)

    def test_needs_a_premise_and_a_hypothesis_and_does_not_touch_the_model(self):
        fake = mock.Mock()
        with mock.patch.object(server, "_openjev", fake):
            r = self.client.post(
                "/call",
                headers=AUTH,
                json={"method": "entail", "args": {"premise": "", "hypotheses": []}},
            )
        body = r.json()
        self.assertFalse(body["ok"])
        fake.assert_not_called()

    def test_does_not_write_to_the_decision_log(self):
        # Only `choose` trains jevlike; `entail` is a fact-check with no
        # single supervised label, so it must not grow the log at all.
        def fake_openjev():
            return lambda premise, hyps: [
                {"contradiction": 0.9, "entailment": 0.05, "neutral": 0.05} for _ in hyps
            ]

        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "decisions.jsonl"
            with mock.patch.object(server, "DECISIONS_LOG", log_path), mock.patch.object(
                server, "_openjev", fake_openjev
            ):
                r = self.client.post(
                    "/call",
                    headers=AUTH,
                    json={"method": "entail", "args": {"premise": "p", "hypotheses": ["h"]}},
                )
            self.assertTrue(r.json()["ok"], r.json())
            self.assertFalse(log_path.exists())

    @unittest.skipUnless(
        os.environ.get("JEV_TEST_OPENJEV"),
        "loads the real ~9 GB openjev weights; set JEV_TEST_OPENJEV=1 to run it",
    )
    def test_the_vendor_class_probe_from_the_incident(self):
        # The exact regression this service exists to not repeat: wired
        # through the tokenizer's pair encoding instead of
        # `OpenJevCrossEncoder`, this premise/hypothesis pair scored 0.72
        # *neutral* (docs/Decisions.md, "openjev through the vendor class").
        # Through the vendor class it is a confident contradiction.
        r = self.client.post(
            "/call",
            headers=AUTH,
            json={
                "method": "entail",
                "args": {
                    "premise": "the server returned 500 on every request after the deploy",
                    "hypotheses": ["the deploy broke the service", "the service is healthy"],
                },
            },
        )
        body = r.json()
        self.assertTrue(body["ok"], body)
        healthy = body["result"]["results"][1]
        self.assertEqual(healthy["hypothesis"], "the service is healthy")
        self.assertEqual(healthy["label"], "contradiction")
        self.assertGreater(healthy["scores"]["contradiction"], 0.8)


class _RecordingLock:
    """Wraps a real lock so a test can prove the *code under test* actually
    calls `acquire`/`release` on it -- not just that a bare lock excludes,
    which is true of any lock by construction and would keep passing even if
    the `with <lock>:` this test means to cover were deleted from server.py
    entirely. Swap this in for the module-level lock under test via
    `mock.patch.object`, then drive the real function (`_choose`, `_entail`,
    `_log_decision`) through it: a dropped `with` shows up as
    `acquire_count == 0`, not a passing test.
    """

    def __init__(self, real_lock):
        self._real = real_lock
        self.acquire_count = 0
        self.max_concurrent = 0
        self._current = 0
        self._meta = threading.Lock()  # guards the two counters, not `_real`

    def acquire(self, *args, **kwargs):
        ok = self._real.acquire(*args, **kwargs)
        if ok:
            with self._meta:
                self._current += 1
                self.max_concurrent = max(self.max_concurrent, self._current)
            self.acquire_count += 1
            # Widen the held window so a second, genuinely concurrent caller
            # has time to reach its own `acquire` and be seen blocking on
            # `_real` -- without this a fast critical section could finish
            # before a broken exclusion ever had a chance to show itself.
            time.sleep(0.1)
        return ok

    def release(self, *args, **kwargs):
        with self._meta:
            self._current -= 1
        self._real.release(*args, **kwargs)

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.release()


class ConcurrencyTests(unittest.TestCase):
    """`asyncio.to_thread` (in `/call`) puts `choose` and `entail` on real OS
    threads, so two calls can genuinely overlap. These prove the locks added
    to close that gap actually serialise real calls to `_choose`, `_entail`
    and `_log_decision` via `_RecordingLock` above -- not just that a bare
    `threading.Lock` excludes, which is true by construction and would not
    notice one of those functions losing its `with <lock>:`."""

    def setUp(self):
        self.client = TestClient(server.app)

    def test_jevlike_calls_are_serialized(self):
        server._jevlike()  # load for real (169 KB, fast) so only the
        # per-call lock inside `run`, not the one-time double-checked load
        # lock, is what the concurrent calls below contend on
        rec = _RecordingLock(server._jevlike_lock)
        args = {"context": "x", "options": ["a", "b"]}
        with mock.patch.object(server, "_jevlike_lock", rec):
            with ThreadPoolExecutor(max_workers=2) as ex:
                results = list(ex.map(lambda _: server._choose(args), range(2)))
        self.assertEqual(
            rec.acquire_count, 2, "_choose never acquired _jevlike_lock -- is the `with` still there?"
        )
        self.assertEqual(rec.max_concurrent, 1, "two _choose calls held _jevlike_lock at once")
        for result in results:
            self.assertEqual(set(result["probs"]), {"a", "b"})

    def test_openjev_calls_are_serialized(self):
        # The real ~9 GB weights stay unloaded, same as EntailTests: a fake
        # `modeling_openjev.OpenJevCrossEncoder`, injected into `sys.modules`
        # before the real `_openjev()` runs, stands in for the one genuinely
        # expensive line. `_openjev()` itself and the `run` closure it
        # returns -- `with _openjev_lock` included -- are the real code from
        # server.py, unmocked.
        fake_ce = mock.Mock()
        fake_ce.predict.return_value = [[0.1, 0.8, 0.1]]  # CON, ENT, NEU
        fake_module = types.ModuleType("modeling_openjev")
        fake_module.OpenJevCrossEncoder = mock.Mock(return_value=fake_ce)
        args = {"premise": "p", "hypotheses": ["h"]}
        with mock.patch.dict(sys.modules, {"modeling_openjev": fake_module}), mock.patch.object(
            server, "_loaded", {}
        ):
            server._openjev()  # load through the real function with the
            # fake weights above, same reasoning as jevlike's pre-load
            rec = _RecordingLock(server._openjev_lock)
            with mock.patch.object(server, "_openjev_lock", rec):
                with ThreadPoolExecutor(max_workers=2) as ex:
                    results = list(ex.map(lambda _: server._entail(args), range(2)))
        self.assertEqual(
            rec.acquire_count, 2, "_entail never acquired _openjev_lock -- is the `with` still there?"
        )
        self.assertEqual(rec.max_concurrent, 1, "two _entail calls held _openjev_lock at once")
        for result in results:
            self.assertEqual(result["results"][0]["hypothesis"], "h")

    def test_decision_log_appends_are_serialized(self):
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "decisions.jsonl"
            rec = _RecordingLock(server._decisions_log_lock)
            with mock.patch.object(server, "DECISIONS_LOG", log_path), mock.patch.object(
                server, "_decisions_log_lock", rec
            ):
                with ThreadPoolExecutor(max_workers=2) as ex:
                    list(
                        ex.map(
                            lambda _: server._log_decision("c", ["a", "b"], 0, {"a": 0.6, "b": 0.4}),
                            range(2),
                        )
                    )
            self.assertEqual(
                rec.acquire_count,
                2,
                "_log_decision never acquired _decisions_log_lock -- is the `with` still there?",
            )
            self.assertEqual(rec.max_concurrent, 1, "two _log_decision calls held the lock at once")
            lines = log_path.read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 2)  # both rows landed intact, not interleaved

    def test_concurrent_choose_calls_over_http_agree_with_each_other(self):
        # Against the real jevlike checkpoint, deterministic and fast enough
        # that firing a batch at once and checking every answer matches is
        # cheap. A corrupted forward pass under concurrency (two threads'
        # batches bleeding into each other) would show up here as answers
        # that disagree for identical input.
        args = {
            "context": "Which queue handles this? The customer wants their money back.",
            "options": ["refund", "sales", "technical support"],
        }

        def one(_):
            r = self.client.post("/call", headers=AUTH, json={"method": "choose", "args": args})
            body = r.json()
            self.assertTrue(body["ok"], body)
            return body["result"]["probs"]

        with ThreadPoolExecutor(max_workers=8) as ex:
            results = list(ex.map(one, range(16)))
        for probs in results[1:]:
            self.assertEqual(probs, results[0])


# Worker script for `CrossProcessLockTests` below: written to disk and run
# as a real, separate OS process (never imported into this one) so the test
# exercises genuine cross-process contention on `_lock_file`/`_unlock_file`
# -- the half of the lock a `threading.Lock` and `_RecordingLock` above
# cannot reach at all, because they only ever see this one process's own
# threads. Static text, not an f-string template: everything that varies
# per run (which jev checkout to import, the shared log path, how many rows
# to write) comes in over argv instead, so there is no Python-source
# interpolation to get wrong.
_CROSS_PROCESS_WORKER_SRC = '''\
import os
import sys
from pathlib import Path

jev_dir, log_path, n_rows = sys.argv[1], sys.argv[2], int(sys.argv[3])
sys.path.insert(0, jev_dir)
import server

server.DECISIONS_LOG = Path(log_path)
pid = os.getpid()
for i in range(n_rows):
    server._log_decision(
        "cross-process worker %d row %d" % (pid, i),
        ["a", "b"],
        0,
        {"a": 0.5, "b": 0.5},
    )
'''


class CrossProcessLockTests(unittest.TestCase):
    """`ConcurrencyTests` above proves `_log_decision` serialises this
    process's OWN threads through `_decisions_log_lock`, a `threading.Lock`
    -- but by the time any thread reaches `_lock_file`, every other thread
    in that SAME process is already blocked on that Python-level lock, so
    `_lock_file`/`_unlock_file` (the cross-process half; see the module
    comment above them in server.py) never see real contention there,
    `_RecordingLock` or not. `unittest` here is also single-process, so
    nothing else in this suite reaches them under contention either.

    This class is the one place that spawns genuine second (and third, and
    fourth...) OS processes, so it is the only place either the Windows
    `msvcrt.locking` branch or the POSIX `fcntl.flock` branch -- whichever
    `sys.platform` selected at import, see server.py -- is exercised while
    actually excluding another process, not a mock standing in for one.
    That is the same claim server.py's own module comment makes from a
    one-off manual run ("six processes appending 200 rows each with
    `_lock_file` stubbed out lost 37% of rows to exactly this race") --
    committed here so a future change that weakens the lock fails a test
    run instead of only a rerun of that one-off script, which is not part
    of this suite and nothing here depends on.
    """

    NUM_PROCESSES = 5
    ROWS_PER_PROCESS = 20

    def test_concurrent_processes_do_not_lose_or_corrupt_rows(self):
        jev_dir = str(Path(__file__).resolve().parent)
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "decisions.jsonl"
            worker_path = Path(tmp) / "_cross_process_worker.py"
            worker_path.write_text(_CROSS_PROCESS_WORKER_SRC, encoding="utf-8")

            procs = [
                subprocess.Popen(
                    [
                        sys.executable,
                        str(worker_path),
                        jev_dir,
                        str(log_path),
                        str(self.ROWS_PER_PROCESS),
                    ],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                for _ in range(self.NUM_PROCESSES)
            ]
            # Gathered only after every process above has already been
            # started -- `Popen` does not block, so all `NUM_PROCESSES` are
            # already racing each other for `log_path` by the time the
            # first `communicate()` call below waits for one to finish.
            results = [p.communicate(timeout=60) for p in procs]

            for i, (proc, (out, err)) in enumerate(zip(procs, results)):
                self.assertEqual(
                    proc.returncode,
                    0,
                    f"worker {i} exited {proc.returncode}: "
                    f"stdout={out.decode(errors='replace')!r} "
                    f"stderr={err.decode(errors='replace')!r}",
                )

            lines = log_path.read_text(encoding="utf-8").splitlines()
            expected = self.NUM_PROCESSES * self.ROWS_PER_PROCESS
            self.assertEqual(
                len(lines),
                expected,
                "row count is off -- a lost or extra line means "
                "_lock_file/_unlock_file did not exclude every writer",
            )

            contexts = set()
            for line in lines:
                row = json.loads(line)  # raises if a row got interleaved/corrupted
                self.assertEqual(set(row), {"context", "options", "label", "probs", "ts"})
                contexts.add(row["context"])
            self.assertEqual(
                len(contexts),
                expected,
                "duplicate or garbled `context` values -- two processes' "
                "writes collided instead of landing one after another",
            )


if __name__ == "__main__":
    unittest.main()
