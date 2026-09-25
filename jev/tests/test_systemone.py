"""Tests for automation/systemone.py -- the System One chooser over HTTP --
and the park it causes in run.py when it cannot score a menu. Every request
goes to a stub server on 127.0.0.1 started here; nothing reaches a real
endpoint, and the only key is a fake one written to a temp file."""
from __future__ import annotations

import json
import logging
import socket
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import automation.decisions as decisions  # noqa: E402
import automation.run as run_mod  # noqa: E402
import automation.systemone as systemone  # noqa: E402

FAKE_KEY = "sk-fake-3f9a1c0e7b"
GRAPHS_DIR = Path(__file__).resolve().parent.parent.parent / "extensions" / "jev" / "graphs"


def _choice_reply(probabilities, *, model="jev-1.13.0", choice=None) -> dict:
    answer = {"type": "choice", "probabilities": probabilities, "confidence": 0.8}
    if choice is not None:
        answer["choice"] = choice
    return {"model": model, "answers": {"next": answer}, "usage": {"input_tokens": 1, "output_tokens": 0}}


class _Stub:
    """A loopback HTTP server whose next reply is whatever `respond` says:
    `(status, headers, body_bytes)`, optionally after `delay_s`. Every
    request it sees is kept, headers included."""

    def __init__(self):
        self.requests: list[dict] = []
        self.status = 200
        self.headers: dict = {"Content-Type": "application/json"}
        self.body = json.dumps(_choice_reply({"x": 0.9, "y": 0.1})).encode()
        self.delay_s = 0.0
        stub = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                stub.requests.append({
                    "path": self.path,
                    "headers": dict(self.headers),
                    "body": json.loads(self.rfile.read(length) or b"null"),
                })
                if stub.delay_s:
                    time.sleep(stub.delay_s)
                self.send_response(stub.status)
                for k, v in stub.headers.items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(stub.body)))
                self.end_headers()
                self.wfile.write(stub.body)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.server.handle_error = lambda request, client_address: None
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class _StubCase(unittest.TestCase):
    def setUp(self):
        self.stub = _Stub()
        self.addCleanup(self.stub.close)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.key_file = self.tmp / "jev.key"
        self.key_file.write_text(FAKE_KEY + "\n", encoding="utf-8")
        self.log_records: list[logging.LogRecord] = []
        handler = logging.Handler(logging.DEBUG)
        handler.emit = self.log_records.append
        root = logging.getLogger()
        old_level = root.level
        root.addHandler(handler)
        root.setLevel(logging.DEBUG)
        self.addCleanup(lambda: (root.removeHandler(handler), root.setLevel(old_level)))

    def chooser(self, **kw) -> systemone.SystemOneChooser:
        kw.setdefault("key_file", self.key_file)
        kw.setdefault("timeout_s", 2.0)
        return systemone.SystemOneChooser(f"systemone:{self.stub.base_url}#autojev", **kw)

    def reply(self, obj=None, *, status=200, raw: bytes | None = None, headers=None, delay_s=0.0):
        self.stub.status = status
        self.stub.body = raw if raw is not None else json.dumps(obj).encode()
        self.stub.delay_s = delay_s
        if headers is not None:
            self.stub.headers = headers

    def assert_parks(self, chooser, labels=("x", "y")) -> run_mod.ChooserUnavailable:
        with self.assertRaises(run_mod.ChooserUnavailable) as caught:
            chooser("the context", list(labels))
        e = caught.exception
        self.assert_no_key(e)
        self.assertEqual(e.chooser["spec"], chooser.spec)
        self.assertIn("error", e.chooser)
        return e

    def assert_no_key(self, e: BaseException):
        seen = [str(e), repr(e), json.dumps(getattr(e, "chooser", None))]
        seen += [str(e.__cause__), str(e.__context__)]
        seen += [r.getMessage() for r in self.log_records]
        for text in seen:
            self.assertNotIn(FAKE_KEY, text)


class SuccessTests(_StubCase):
    def test_scores_come_back_in_menu_order_with_who_scored_them(self):
        self.reply(_choice_reply({"y": 0.25, "x": 0.75}, choice="x"))
        scores = self.chooser()("ctx", ["x", "y"])
        self.assertEqual(list(scores), [0.75, 0.25])
        self.assertEqual(scores.chooser["spec"], f"systemone:{self.stub.base_url}#autojev")
        self.assertEqual(scores.chooser["model"], "autojev")
        self.assertEqual(scores.chooser["served_model"], "jev-1.13.0")
        self.assertIsInstance(scores.chooser["latency_ms"], int)
        self.assertNotIn("error", scores.chooser)

    def test_the_request_is_one_choice_question_keyed_by_label(self):
        self.reply(_choice_reply({"x": 0.5, "y": 0.5}))
        self.chooser()("the state text", ["x", "y"])
        request = self.stub.requests[0]
        self.assertEqual(request["path"], "/v1/systemone")
        self.assertEqual(request["body"], {
            "model": "autojev",
            "state": "the state text",
            "questions": {"next": {
                "type": "choice",
                "instructions": systemone.INSTRUCTIONS,
                "criteria": {"x": "x", "y": "y"},
            }},
        })
        self.assertEqual(request["headers"]["Authorization"], f"Bearer {FAKE_KEY}")

    def test_no_key_file_sends_no_authorization(self):
        self.chooser(key_file=None)("ctx", ["x", "y"])
        self.assertNotIn("Authorization", self.stub.requests[0]["headers"])

    def test_a_reply_without_a_model_name_keeps_the_requested_one(self):
        reply = _choice_reply({"x": 0.9, "y": 0.1})
        del reply["model"]
        self.reply(reply)
        meta = self.chooser()("ctx", ["x", "y"]).chooser
        self.assertEqual((meta["model"], meta["served_model"]), ("autojev", None))

    def test_the_served_model_name_is_capped_and_must_be_printable(self):
        self.reply(_choice_reply({"x": 0.9, "y": 0.1}, model="m" * 10_000))
        meta = self.chooser()("ctx", ["x", "y"]).chooser
        self.assertEqual(meta["served_model"], "m" * systemone.MAX_MODEL_NAME)
        self.assertEqual(meta["model"], "autojev")
        self.reply(_choice_reply({"x": 0.9, "y": 0.1}, model="evil\nX-Forged: 1"))
        self.assertIsNone(self.chooser()("ctx", ["x", "y"]).chooser["served_model"])


class FailClosedTests(_StubCase):
    def test_timeout(self):
        self.reply(_choice_reply({"x": 0.9, "y": 0.1}), delay_s=1.0)
        e = self.assert_parks(self.chooser(timeout_s=0.2))
        self.assertIn("timed out", str(e))

    def test_non_2xx(self):
        for status in (401, 422, 429, 500, 529):
            with self.subTest(status=status):
                self.reply({"error": "no"}, status=status)
                self.assertIn(f"HTTP {status}", str(self.assert_parks(self.chooser())))

    def test_a_redirect_is_not_followed(self):
        self.reply({}, status=302, headers={"Location": "http://127.0.0.1:9/elsewhere"})
        self.assertIn("HTTP 302", str(self.assert_parks(self.chooser())))
        self.assertEqual(len(self.stub.requests), 1)

    def test_malformed_json(self):
        self.reply(raw=b"{not json")
        self.assertIn("not JSON", str(self.assert_parks(self.chooser())))

    def test_missing_answer(self):
        for body in ({}, {"answers": {}}, {"answers": {"next": "x"}}, [], {"answers": []}):
            with self.subTest(body=body):
                self.reply(body)
                self.assert_parks(self.chooser())

    def test_wrong_number_of_scores(self):
        for probabilities in ({"x": 1.0}, {"x": 0.5, "y": 0.25, "z": 0.25}, {"x": 0.5, "z": 0.5}):
            with self.subTest(probabilities=probabilities):
                self.reply(_choice_reply(probabilities))
                self.assert_parks(self.chooser())

    def test_a_list_of_probabilities_is_not_guessed_into_menu_order(self):
        self.reply(_choice_reply([0.9, 0.1]))
        self.assert_parks(self.chooser())

    def test_nan_and_inf(self):
        for literal in ("NaN", "Infinity", "-Infinity"):
            with self.subTest(literal=literal):
                self.reply(raw=b'{"answers": {"next": {"probabilities": {"x": %s, "y": 0.1}}}}' % literal.encode())
                self.assert_parks(self.chooser())

    def test_out_of_range_or_not_a_number(self):
        for bad in (1.5, -0.1, True, "0.9", None):
            with self.subTest(bad=bad):
                self.reply(_choice_reply({"x": bad, "y": 0.1}))
                self.assert_parks(self.chooser())

    def test_probabilities_that_do_not_sum_to_one(self):
        self.reply(_choice_reply({"x": 0.3, "y": 0.3}))
        self.assertIn("sum", str(self.assert_parks(self.chooser())))

    def test_a_choice_outside_the_menu(self):
        self.reply(_choice_reply({"x": 0.9, "y": 0.1}, choice="z"))
        self.assert_parks(self.chooser())

    def test_a_non_choice_answer(self):
        reply = _choice_reply({"x": 0.9, "y": 0.1})
        reply["answers"]["next"]["type"] = "score"
        self.reply(reply)
        self.assert_parks(self.chooser())

    def test_an_oversized_reply(self):
        self.reply(raw=b" " * (systemone.MAX_REPLY_BYTES + 10))
        self.assertIn("1 MiB", str(self.assert_parks(self.chooser())))

    def test_a_huge_integer_probability_parks_instead_of_raising(self):
        self.reply(raw=b'{"answers": {"next": {"probabilities": {"x": 1%s, "y": 0}}}}' % (b"0" * 400))
        self.assert_parks(self.chooser())

    def test_duplicate_keys_in_the_reply_park(self):
        self.reply(raw=b'{"answers": {"next": {"probabilities": {"x": 0.0, "y": 0.1, "x": 0.9}}}}')
        self.assertIn("unique keys", str(self.assert_parks(self.chooser())))

    def test_any_unexpected_exception_becomes_a_park(self):
        chooser = self.chooser()

        def explode(*a, **kw):
            raise RuntimeError(f"boom {FAKE_KEY}")

        chooser._post = explode
        e = self.assert_parks(chooser)
        self.assertEqual(str(e), "chooser failed: RuntimeError")

    def test_connection_refused_is_retried_once_then_parks(self):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        chooser = systemone.SystemOneChooser(f"systemone:http://127.0.0.1:{port}#m", key_file=self.key_file)
        attempts = []
        real_exchange = chooser._exchange
        chooser._exchange = lambda *a: (attempts.append(1), real_exchange(*a))[1]
        e = self.assert_parks(chooser)
        self.assertIn("unreachable", str(e))
        self.assertEqual(len(attempts), 2)

    def test_too_many_or_duplicate_options_never_reach_the_wire(self):
        self.assert_parks(self.chooser(), labels=[f"o{i}" for i in range(256)])
        self.assert_parks(self.chooser(), labels=["x", "x"])
        self.assertEqual(self.stub.requests, [])

    def test_a_missing_or_empty_key_file_parks_before_any_request(self):
        empty = self.tmp / "empty.key"
        empty.write_text("\n", encoding="utf-8")
        for key_file in (self.tmp / "absent.key", empty, self.tmp):
            with self.subTest(key_file=key_file):
                self.assert_parks(self.chooser(key_file=key_file))
        self.assertEqual(self.stub.requests, [])

    def test_a_key_file_with_a_line_break_inside_parks_without_echoing_it(self):
        self.key_file.write_text(f"{FAKE_KEY}\r\nX-Evil: 1", encoding="utf-8")
        self.assert_parks(self.chooser())
        self.assertEqual(self.stub.requests, [])

    def test_a_server_echoing_the_key_back_never_puts_it_in_the_error(self):
        self.reply({"error": f"bad key Bearer {FAKE_KEY}"}, status=401)
        self.assert_parks(self.chooser())
        self.reply(raw=f"not json {FAKE_KEY}".encode())
        self.assert_parks(self.chooser())


class _Drip:
    """A raw socket server that sends a 200 reply one byte at a time,
    `interval_s` apart -- from the status line on (`from_headers`), or only
    once the headers are out."""

    def __init__(self, body: bytes, *, interval_s: float, from_headers: bool):
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen()
        self.port = self.sock.getsockname()[1]
        head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n" % len(body)

        def serve():
            try:
                conn, _ = self.sock.accept()
                with conn:
                    conn.recv(65536)
                    if not from_headers:
                        conn.sendall(head)
                    for byte in (head + body if from_headers else body):
                        time.sleep(interval_s)
                        conn.sendall(bytes([byte]))
            except OSError:
                pass

        threading.Thread(target=serve, daemon=True).start()

    def close(self):
        self.sock.close()


class DeadlineTests(_StubCase):
    BODY = json.dumps(_choice_reply({"x": 0.9, "y": 0.1})).encode()

    def _dripped(self, from_headers: bool):
        drip = _Drip(self.BODY, interval_s=0.2, from_headers=from_headers)
        self.addCleanup(drip.close)
        chooser = systemone.SystemOneChooser(f"systemone:http://127.0.0.1:{drip.port}#m", timeout_s=1.0)
        started = time.monotonic()
        e = self.assert_parks(chooser)
        elapsed = time.monotonic() - started
        self.assertIn("timed out", str(e))
        self.assertLess(elapsed, 2.0)

    def test_a_dripping_body_hits_the_total_deadline(self):
        self._dripped(from_headers=False)

    def test_dripping_headers_hit_the_total_deadline(self):
        self._dripped(from_headers=True)


class SpecTests(unittest.TestCase):
    def test_loopback_http_is_allowed(self):
        for base in ("http://127.0.0.1:8080", "http://localhost:8080/", "http://[::1]:8080"):
            with self.subTest(base=base):
                url, model = systemone.parse_spec(f"systemone:{base}#autojev")
                self.assertEqual(model, "autojev")
                self.assertFalse(url.endswith("/"))

    def test_plain_http_to_a_remote_host_is_refused_even_when_remote_is_allowed(self):
        for base in ("http://10.0.0.5:8080", "http://api.typesafe.ai", "http://127.0.0.1.nip.io"):
            with self.subTest(base=base), self.assertRaises(ValueError):
                systemone.parse_spec(f"systemone:{base}#m", allow_remote=True)

    def test_https_to_a_remote_host_needs_the_opt_in(self):
        with self.assertRaises(ValueError):
            systemone.parse_spec("systemone:https://api.typesafe.ai#jev-latest")
        self.assertEqual(
            systemone.parse_spec("systemone:https://api.typesafe.ai#jev-latest", allow_remote=True),
            ("https://api.typesafe.ai", "jev-latest"),
        )

    def test_malformed_specs_are_refused(self):
        for spec in (
            "http://127.0.0.1:8080#m",
            "systemone:http://127.0.0.1:8080",
            "systemone:http://127.0.0.1:8080#",
            "systemone:#m",
            "systemone:ftp://127.0.0.1#m",
            f"systemone:https://user:{FAKE_KEY}@127.0.0.1#m",
            "systemone:http://127.0.0.1:8080/?key=x#m",
        ):
            with self.subTest(spec=spec), self.assertRaises(ValueError) as caught:
                systemone.parse_spec(spec)
            self.assertNotIn(FAKE_KEY, str(caught.exception))

    def test_whitespace_control_characters_and_backslashes_are_refused(self):
        for spec in (
            "systemone:http://127.0.0.1\t:1#m",
            "systemone:http://127.0.0.1:1\r\nHost: evil#m",
            "systemone:http://127.0.0.1:80\\evil.com#m",
            "systemone:http://127.0.0.1:1#m m",
            "systemone:http://127.0.0.1:1#m\x7f",
        ):
            with self.subTest(spec=spec), self.assertRaises(ValueError):
                systemone.parse_spec(spec)

    def test_the_url_is_rebuilt_from_its_checked_parts(self):
        self.assertEqual(systemone.parse_spec("systemone:http://LOCALHOST:8081/base/#m")[0], "http://LOCALHOST:8081/base")
        chooser = systemone.SystemOneChooser("systemone:http://[::1]:8081/base/#m")
        self.assertEqual((chooser._host, chooser._port, chooser._path), ("::1", 8081, "/base/v1/systemone"))
        with self.assertRaises(ValueError):
            systemone.parse_spec("systemone:http://127.0.0.1:99999#m")

    def test_the_timeout_must_be_finite_and_positive(self):
        for bad in ("inf", "-inf", "nan", "0", "-1"):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                systemone.from_env({"JEV_CHOOSER": "systemone:http://127.0.0.1:1#m", "JEV_CHOOSER_TIMEOUT_S": bad})

    def test_from_env_defaults_to_jevlike(self):
        self.assertIsNone(systemone.from_env({}))
        self.assertIsNone(systemone.from_env({"JEV_CHOOSER": "jevlike"}))
        chooser = systemone.from_env({
            "JEV_CHOOSER": "systemone:https://api.typesafe.ai#jev-latest",
            "JEV_CHOOSER_ALLOW_REMOTE": "1",
            "JEV_CHOOSER_KEY_FILE": "C:/nowhere/jev.key",
            "JEV_CHOOSER_TIMEOUT_S": "5",
        })
        self.assertEqual((chooser.model, chooser.timeout_s), ("jev-latest", 5.0))
        with self.assertRaises(ValueError):
            systemone.from_env({"JEV_CHOOSER": "systemone:https://api.typesafe.ai#jev-latest"})
        with self.assertRaises(ValueError):
            systemone.from_env({"JEV_CHOOSER": "systemone:http://127.0.0.1:1#m", "JEV_CHOOSER_TIMEOUT_S": "0"})


def _menu_graph() -> dict:
    return {
        "id": "systemone-menu",
        "initial": "a",
        "meta": {"jev": {"schema": 1, "defaults": {"floor": 0.5, "margin": 0.1}}},
        "context": {},
        "states": {
            "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
            "b": {"type": "final"},
        },
    }


class RunIntegrationTests(_StubCase):
    def setUp(self):
        super().setUp()
        for name, sub in (("JEV_DECISIONS_DIR", "decisions"), ("JEV_RUNS_DIR", "runs")):
            import os

            old = os.environ.get(name)
            os.environ[name] = str(self.tmp / sub)
            self.addCleanup(lambda n=name, o=old: os.environ.pop(n, None) if o is None else os.environ.__setitem__(n, o))

    def rows(self) -> list[dict]:
        path = decisions.graph_log_path("systemone-menu")
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def all_written_text(self) -> str:
        return "".join(p.read_text(encoding="utf-8") for p in self.tmp.rglob("*.jsonl"))

    def test_a_clean_answer_is_logged_with_the_chooser_that_gave_it(self):
        self.reply(_choice_reply({"x": 0.9, "y": 0.1}))
        started = run_mod.start(_menu_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=self.chooser(), ckpt_id=None)
        self.assertEqual(started["request"]["kind"], "final")
        row = self.rows()[0]
        self.assertEqual((row["chosen"], row["source"], row["probs"]), ("x", "chooser", [0.9, 0.1]))
        self.assertEqual(row["chooser"]["spec"], f"systemone:{self.stub.base_url}#autojev")
        self.assertEqual((row["chooser"]["model"], row["chooser"]["served_model"]), ("autojev", "jev-1.13.0"))
        self.assertIsInstance(row["chooser"]["latency_ms"], int)
        self.assertNotIn(FAKE_KEY, self.all_written_text())

    def test_a_failed_call_parks_and_the_answer_is_logged_against_it(self):
        self.reply({"error": f"echo {FAKE_KEY}"}, status=500)
        started = run_mod.start(_menu_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=self.chooser(), ckpt_id=None)
        esc = started["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertEqual(esc["why"], "chooser unavailable: HTTP 500")
        self.assertEqual([(o["label"], o["p"]) for o in esc["options"]], [("x", 0.0), ("y", 0.0)])
        self.assertEqual(esc["chooser"]["error"], "HTTP 500")
        self.assertNotIn(FAKE_KEY, json.dumps(esc))
        self.assertNotIn(FAKE_KEY, json.dumps(run_mod.status(started["run"])))
        self.assertFalse(decisions.graph_log_path("systemone-menu").exists())
        waiting = run_mod.status(started["run"])["decisions"][-1]
        self.assertEqual((waiting["source"], waiting["top_score"], waiting["gap"]), ("unavailable", None, None))
        self.assertEqual([o["score"] for o in waiting["options"]], [None, None])

        final = run_mod.answer(started["run"], pick="y", by="human", request=esc["request"])["request"]
        self.assertEqual(final["kind"], "final")
        row = self.rows()[0]
        self.assertEqual((row["chosen"], row["source"], row["verified"]), ("y", "human", "human"))
        self.assertIsNone(row["probs"])
        picked = run_mod.status(started["run"])["decisions"][-1]
        self.assertEqual((picked["top_score"], picked["gap"]), (None, None))
        self.assertEqual(row["chooser"]["error"], "HTTP 500")
        self.assertNotIn(FAKE_KEY, self.all_written_text())

    def test_a_restarted_park_still_logs_which_chooser_failed(self):
        self.reply(raw=b"garbage")
        started = run_mod.start(_menu_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=self.chooser(), ckpt_id=None)
        esc = started["request"]
        run_mod._RUNS.pop(started["run"])
        final = run_mod.answer(
            started["run"], pick=0, request=esc["request"], graphs_dir=GRAPHS_DIR, chooser=self.chooser(),
        )["request"]
        self.assertEqual(final["kind"], "final")
        row = self.rows()[0]
        self.assertEqual(row["chooser"]["error"], "the reply is not JSON with unique keys")
        self.assertIsNone(row["probs"])


class LogCompatibilityTests(unittest.TestCase):
    OLD_ROW = (
        '{"id": "r_1-0001", "context": "c", "options": ["a", "b"], "label": 0, "chosen": "a", '
        '"probs": [0.9, 0.1], "source": "jevlike", "verified": null, "floor": 0.5, "margin": 0.1, '
        '"run": "r_1", "graph": "g@1", "state": "s", "step": 1, "ts": "2026-09-01T00:00:00Z", '
        '"ckpt": "deadbeef", "action": null, "warrant": null}'
    )

    def test_a_row_from_before_the_chooser_field_still_reads(self):
        row = json.loads(self.OLD_ROW)
        self.assertIsNone(row.get("chooser"))
        self.assertEqual((row["label"], row["source"], row["ckpt"]), (0, "jevlike", "deadbeef"))

    def test_a_jevlike_row_keeps_every_old_field_and_adds_chooser_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            import os

            old = os.environ.get("JEV_DECISIONS_DIR")
            os.environ["JEV_DECISIONS_DIR"] = tmp
            try:
                decisions.log_decision(
                    id="r_1-0001", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.9, 0.1],
                    source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="g@1",
                    state="s", step=1, ckpt="deadbeef", action=None, ts=0,
                )
                new = json.loads(decisions.graph_log_path("g").read_text(encoding="utf-8"))
            finally:
                if old is None:
                    os.environ.pop("JEV_DECISIONS_DIR", None)
                else:
                    os.environ["JEV_DECISIONS_DIR"] = old
        old_row = json.loads(self.OLD_ROW)
        self.assertEqual(set(new) - set(old_row), {"chooser"})
        self.assertIsNone(new["chooser"])
        for key in old_row:
            if key != "ts":
                self.assertEqual(new[key], old_row[key], key)


if __name__ == "__main__":
    unittest.main()
