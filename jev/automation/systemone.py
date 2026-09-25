"""systemone.py -- a `run.Chooser` over HTTP, for any Jev-family model that
answers TypeSafe's System One wire format at `POST <base>/v1/systemone`: a
local jev.cpp `llama-server --system-one` (AutoJev, Decider, JevK5 GGUFs) or
the hosted Jev API. One menu is one `choice` question; its `probabilities`
map, read back by label, is the score list `run.py` weighs against the
floor. Anything short of a clean, complete, in-range answer raises
`run.ChooserUnavailable`, which parks the choice -- never a silent pick.

Wire format, as documented (fetched 2026-09-25):
  hosted  https://docs.typesafe.ai/api.md, https://docs.typesafe.ai/primitives/choice.md
  local   https://github.com/thomasgauthier/jev.cpp/blob/master/tools/server/README.md
The local README elides the shape of `probabilities`; the hosted docs give
an object keyed by option name. Only that shape is accepted -- a list
parks rather than being guessed into menu order. `request_body` and
`read_scores` are the whole mapping, kept apart from the transport, which
is stdlib `http.client`, so no proxy or redirect is ever followed.

Configured from the service's environment, the same way `server.py` reads
everything else. Unset `JEV_CHOOSER` keeps jevlike.

  JEV_CHOOSER               systemone:<base_url>#<model>
  JEV_CHOOSER_KEY_FILE      a file holding the bearer token; sent only if set
  JEV_CHOOSER_ALLOW_REMOTE  1 to allow a non-loopback host (data leaves the box)
  JEV_CHOOSER_TIMEOUT_S     one deadline per call, connect to last byte; default 10
"""
from __future__ import annotations

import http.client
import ipaddress
import json
import math
import os
import socket
import threading
import time
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

from .run import ChooserUnavailable

PREFIX = "systemone:"
QUESTION = "next"
INSTRUCTIONS = "Which option should be taken next?"
MAX_OPTIONS = 255
MAX_REPLY_BYTES = 1 << 20
MAX_MODEL_NAME = 128
SUM_TOLERANCE = 0.01


class Scores(list):
    """The chooser's `list[float]`, carrying who scored it."""

    def __init__(self, probs: list[float], chooser: dict):
        super().__init__(probs)
        self.chooser = chooser


def _is_loopback(host: str) -> bool:
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def parse_spec(spec: str, *, allow_remote: bool = False) -> tuple[str, str]:
    """`systemone:<base_url>#<model>` -> `(base_url, model)`, refusing
    anything that could leak a request: plain http off loopback, a remote
    host without the profile's opt-in, credentials or a query in the URL.
    The URL returned is rebuilt from the parts that were checked."""
    if not spec.startswith(PREFIX):
        raise ValueError(f"a systemone spec starts with {PREFIX!r}")
    if any(ord(c) <= 0x20 or ord(c) == 0x7F or c == "\\" for c in spec):
        raise ValueError("a systemone spec must not contain whitespace, control characters or backslashes")
    base_url, sep, model = spec[len(PREFIX):].rpartition("#")
    if not sep or not model or not base_url:
        raise ValueError("a systemone spec is systemone:<base_url>#<model>")
    parts = urlsplit(base_url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise ValueError("the base_url must be an http(s) URL with a host")
    if parts.username is not None or parts.password is not None:
        raise ValueError("the base_url must not carry credentials; name a key file instead")
    if parts.query or parts.fragment:
        raise ValueError("the base_url must not carry a query or fragment")
    _ = parts.port  # a malformed port raises ValueError here, not at the first call
    loopback = _is_loopback(parts.hostname)
    if parts.scheme == "http" and not loopback:
        raise ValueError(f"refusing plain http to non-loopback host {parts.hostname!r}; use https")
    if not loopback and not allow_remote:
        raise ValueError(f"{parts.hostname!r} is not loopback; set JEV_CHOOSER_ALLOW_REMOTE=1 to send data off the box")
    return urlunsplit((parts.scheme, parts.netloc, parts.path.rstrip("/"), "", "")), model


def request_body(model: str, context: str, labels: list[str]) -> dict:
    if len(labels) > MAX_OPTIONS:
        raise ChooserUnavailable(f"{len(labels)} options is over System One's {MAX_OPTIONS}")
    if len(set(labels)) != len(labels):
        raise ChooserUnavailable("option labels must be distinct to key a choice question")
    return {
        "model": model,
        "state": context,
        "questions": {
            QUESTION: {
                "type": "choice",
                "instructions": INSTRUCTIONS,
                "criteria": {label: label for label in labels},
            }
        },
    }


def _unique_keys(pairs: list) -> dict:
    if len({k for k, _ in pairs}) != len(pairs):
        raise ValueError("duplicate key")
    return dict(pairs)


def _probability(value) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ChooserUnavailable("a probability is not a number")
    try:
        p = float(value)
    except OverflowError:
        p = math.inf
    if not math.isfinite(p) or not 0.0 <= p <= 1.0:
        raise ChooserUnavailable("a probability is not a finite number in [0, 1]")
    return p


def _served_model(value) -> str | None:
    if isinstance(value, str) and value.isprintable():
        return value[:MAX_MODEL_NAME]
    return None


def read_scores(raw: bytes, labels: list[str]) -> tuple[list[float], str | None]:
    """The reply's probabilities in `labels` order, and the model name it
    reports -- or `ChooserUnavailable` naming what was wrong with it."""
    try:
        reply = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_keys)
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise ChooserUnavailable("the reply is not JSON with unique keys") from None
    answers = reply.get("answers") if isinstance(reply, dict) else None
    answer = answers.get(QUESTION) if isinstance(answers, dict) else None
    if not isinstance(answer, dict):
        raise ChooserUnavailable(f"the reply has no answers.{QUESTION} object")
    if answer.get("type", "choice") != "choice":
        raise ChooserUnavailable(f"answers.{QUESTION} is not a choice answer")
    probabilities = answer.get("probabilities")
    if not isinstance(probabilities, dict):
        raise ChooserUnavailable("answers.probabilities is not an object keyed by option")
    if set(probabilities) != set(labels):
        raise ChooserUnavailable(f"the reply scored {len(probabilities)} option(s), not the {len(labels)} asked")
    probs = [_probability(probabilities[label]) for label in labels]
    if abs(sum(probs) - 1.0) > SUM_TOLERANCE:
        raise ChooserUnavailable(f"the probabilities sum to {sum(probs):.3f}, not 1")
    if "choice" in answer and answer["choice"] not in labels:
        raise ChooserUnavailable("the reply's choice is not one of the options")
    return probs, _served_model(reply.get("model"))


def _sever(connection: http.client.HTTPConnection) -> None:
    sock = connection.sock
    if sock is not None:
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


class SystemOneChooser:
    def __init__(
        self,
        spec: str,
        *,
        key_file: str | Path | None = None,
        allow_remote: bool = False,
        timeout_s: float = 10.0,
    ):
        self.base_url, self.model = parse_spec(spec, allow_remote=allow_remote)
        if not (math.isfinite(timeout_s) and timeout_s > 0):
            raise ValueError("timeout_s must be finite and positive")
        self.spec = spec
        self.key_file = Path(key_file) if key_file else None
        self.timeout_s = timeout_s
        parts = urlsplit(self.base_url)
        self._https = parts.scheme == "https"
        self._host = parts.hostname
        self._port = parts.port
        self._path = f"{parts.path}/v1/systemone"

    def __repr__(self) -> str:
        return f"SystemOneChooser({self.spec!r})"

    def __call__(self, context: str, labels: list[str]) -> Scores:
        started = time.monotonic()
        identity = {"spec": self.spec, "model": self.model}
        try:
            body = request_body(self.model, context, labels)
            raw = self._post(body, deadline=started + self.timeout_s)
            probs, served = read_scores(raw, labels)
        except ChooserUnavailable as e:
            failure = e
        except Exception as e:
            failure = ChooserUnavailable(f"chooser failed: {type(e).__name__}")
        else:
            return Scores(probs, {**identity, "served_model": served, "latency_ms": _ms_since(started)})
        # Raised outside the handler with its chain cut, so no underlying
        # exception -- whose text this module does not control -- rides along.
        failure.__cause__ = failure.__context__ = None
        failure.chooser = {**identity, "latency_ms": _ms_since(started), "error": str(failure)}
        raise failure

    def _headers(self) -> dict:
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if self.key_file is None:
            return headers
        try:
            key = self.key_file.read_text(encoding="utf-8").strip()
        except (OSError, UnicodeDecodeError):
            raise ChooserUnavailable("JEV_CHOOSER_KEY_FILE is unreadable") from None
        if not key or not key.isascii() or not key.isprintable() or any(c.isspace() for c in key):
            raise ChooserUnavailable("the key file does not hold a single-line token")
        headers["Authorization"] = f"Bearer {key}"
        return headers

    def _connection(self, timeout: float) -> http.client.HTTPConnection:
        if self._https:
            return http.client.HTTPSConnection(self._host, self._port, timeout=timeout)
        return http.client.HTTPConnection(self._host, self._port, timeout=timeout)

    def _post(self, body: dict, *, deadline: float) -> bytes:
        payload = json.dumps(body).encode("utf-8")
        headers = self._headers()
        try:
            return self._exchange(payload, headers, deadline)
        except ConnectionRefusedError:
            pass
        try:
            return self._exchange(payload, headers, deadline)
        except ConnectionRefusedError:
            raise ChooserUnavailable("unreachable: ConnectionRefusedError") from None

    def _exchange(self, payload: bytes, headers: dict, deadline: float) -> bytes:
        """One request under the call's single deadline: connect waits at
        most what is left of it, and a watchdog severs the socket when it
        passes, so a server dripping headers or body cannot stretch a call
        past `timeout_s`."""
        timed_out = ChooserUnavailable(f"timed out after {self.timeout_s:g}s")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise timed_out
        connection = self._connection(remaining)
        watchdog = threading.Timer(remaining, _sever, (connection,))
        watchdog.daemon = True
        watchdog.start()
        try:
            connection.request("POST", self._path, body=payload, headers=headers)
            response = connection.getresponse()
            if not 200 <= response.status < 300:
                raise ChooserUnavailable(f"HTTP {response.status}")
            chunks, size = [], 0
            while size <= MAX_REPLY_BYTES:
                chunk = response.read1(MAX_REPLY_BYTES + 1 - size)
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
        except (ChooserUnavailable, ConnectionRefusedError):
            raise
        except Exception as e:
            if time.monotonic() >= deadline:
                raise timed_out from None
            raise ChooserUnavailable(f"transport failed: {type(e).__name__}") from None
        finally:
            watchdog.cancel()
            connection.close()
        if time.monotonic() >= deadline:
            raise timed_out
        if size > MAX_REPLY_BYTES:
            raise ChooserUnavailable("the reply is over 1 MiB")
        return b"".join(chunks)


def _ms_since(started: float) -> int:
    return round((time.monotonic() - started) * 1000)


def from_env(env=os.environ) -> SystemOneChooser | None:
    spec = (env.get("JEV_CHOOSER") or "").strip()
    if not spec or spec == "jevlike":
        return None
    return SystemOneChooser(
        spec,
        key_file=env.get("JEV_CHOOSER_KEY_FILE") or None,
        allow_remote=env.get("JEV_CHOOSER_ALLOW_REMOTE") == "1",
        timeout_s=float(env.get("JEV_CHOOSER_TIMEOUT_S") or 10),
    )
